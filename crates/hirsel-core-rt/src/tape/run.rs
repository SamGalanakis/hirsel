//! Run tapes are owned by process startup. Main receives only substitutes;
//! Replay constructs its grants from the entry signature, with no host provider.
use super::*;
use crate::{
    Context,
    abi_sites::SourceTable,
    causes::{self, Cause},
    entry::{self, Grant, OsConfig},
    process::{self, Config, Entry, MainResult, View},
};
use hirsel_core_ids::launch::Metadata;
use std::{
    ffi::{CStr, OsStr},
    os::unix::ffi::OsStrExt,
    path::Path,
};

type DescribeOutput = unsafe extern "C" fn(*mut u8, *const u8, usize) -> bool;

struct Run {
    entry: Entry,
    requirements: *const entry::Requirement,
    names: *const View,
    requirements_len: usize,
    path: Vec<u8>,
    identity: Vec<u8>,
    revision: [u8; 32],
    mode: u64,
    sources: *const SourceTable,
    stderr: i32,
    diagnostic: Vec<u8>,
    stop: Option<u8>,
    record_truncated: Option<(u64, IoError)>,
    grants: Vec<u64>,
    replay: Option<ReplayInput>,
    sink: Box<crate::os::OsProvider>,
    launch: Box<Metadata>,
    describe_output: Option<(DescribeOutput, *mut u8)>,
    describe_tape: (Provider, u64),
    host: *const entry::HostLaunch,
    record_id: Option<[u8; 16]>,
}

unsafe extern "C" fn write_describe(state: *mut u8, request: *mut Request, _: u64) -> Step {
    // SAFETY: Run pins the original launch callback/state and the request's bytes.
    unsafe {
        let run = &*state.cast::<Run>();
        let bytes = &(*request).buffer;
        let written = run
            .describe_output
            .is_some_and(|(write, state)| write(state, bytes.data, bytes.len));
        done(Outcome {
            written: if written { bytes.len as u64 } else { 0 },
            error: if written {
                IoError::OK
            } else {
                IoError { tag: 5, code: 0 }
            },
            ..Outcome::default()
        })
    }
}

unsafe extern "C" fn emit_describe(state: *mut u8, bytes: *const u8, len: usize) -> bool {
    // SAFETY: process startup pins Run; the private transport pins its output.
    unsafe {
        let (p, h) = (*state.cast::<Run>()).describe_tape;
        let mut r = request(
            STDOUT,
            0,
            h,
            &[],
            Buffer {
                data: bytes.cast_mut(),
                len,
                capacity: 0,
            },
        );
        if (p.wait)(p.state, &mut r, 0).kind != WAIT_READY {
            return false;
        }
        let step = (p.issue)(p.state, &mut r, 0);
        if step.kind != STEP_DONE {
            return false;
        }
        let out = (p.deliver)(p.state, &mut r, step.outcome);
        out.status == 0 && out.error == IoError::OK
    }
}

fn write_launch(out: &mut Vec<u8>, launch: &Metadata, managed: bool) {
    for bytes in [launch.digest(), launch.target().as_bytes()] {
        put_varint(out, bytes.len() as u64);
        out.extend_from_slice(bytes);
    }
    out.extend_from_slice(&launch.n_place().to_le_bytes());
    out.push(u8::from(launch.is_describe()) | (u8::from(managed) << 1));
}

fn read_launch(arg: &[u8]) -> Result<(Metadata, bool), Corrupt> {
    let mut c = Cursor::new(arg);
    for _ in 0..c.varint()? {
        c.bytes()?;
    }
    let digest = match c.bytes()? {
        [] => None,
        bytes => Some(bytes.try_into().map_err(|_| Corrupt)?),
    };
    let target = std::str::from_utf8(c.bytes()?).map_err(|_| Corrupt)?;
    let n_place = u64::from_le_bytes(c.fixed()?);
    let flags = c.u8()?;
    if !c.done() || flags > 2 {
        return Err(Corrupt);
    }
    Ok((
        Metadata::embedded(digest, target, n_place, flags == 1),
        flags == 2,
    ))
}
unsafe extern "C" fn diagnostic(state: *mut u8, bytes: *const u8, len: usize) {
    // SAFETY: the process pins Run and the caller pins this diagnostic view.
    unsafe {
        crate::os::diagnostic((*state.cast::<Run>()).stderr, bytes, len);
    }
}
unsafe extern "C" fn describe(state: *mut u8, cause: *const Cause) -> View {
    // SAFETY: the source binder and cause stay pinned through reporting.
    unsafe {
        let run = &mut *state.cast::<Run>();
        run.diagnostic = crate::abi_sites::format_cause(&*cause, run.sources).into_bytes();
        View {
            data: run.diagnostic.as_ptr(),
            len: run.diagnostic.len(),
        }
    }
}
fn refused(run: &mut Run, error: OpenError) -> u8 {
    let reason = match error {
        OpenError::Refused(Refusal::Code) => "code".to_owned(),
        OpenError::Refused(Refusal::Revision) => "revision".to_owned(),
        OpenError::Refused(Refusal::Arg) => "arg".to_owned(),
        OpenError::Refused(Refusal::Limit) => "limit".to_owned(),
        OpenError::Refused(Refusal::Damaged(block)) => format!("damaged block {block}"),
        OpenError::Refused(Refusal::HeaderTooLarge) => "header too large".to_owned(),
        OpenError::Unreadable(Fault::Truncated(at)) => format!("truncated at {at}"),
        OpenError::Unreadable(Fault::Damaged(block)) => format!("damaged block {block}"),
        OpenError::Unreadable(Fault::Corrupt) => "corrupt".to_owned(),
        OpenError::Unreadable(other) => format!("{other}"),
        OpenError::Io(e) => format_io_error(e),
    };
    let line = if run.mode != 0 {
        format!("replay: refused: {reason}\n")
    } else {
        format!("record: refused: {reason}\n")
    };
    // SAFETY: configured fd is borrowed through this call, line owns its bytes.
    unsafe {
        crate::os::diagnostic(run.stderr, line.as_ptr(), line.len());
    }
    run.stop = Some(if run.mode != 0 {
        2
    } else {
        process::NOT_STARTED
    });
    1
}

fn format_io_error(e: IoError) -> String {
    match e.tag {
        1 => "not found".to_owned(),
        2 => "denied".to_owned(),
        3 => "exists".to_owned(),
        4 => "closed".to_owned(),
        _ if e.code != 0 => format!("os error {}", e.code),
        _ => format!("io ({}, {})", e.tag, e.code),
    }
}

fn report_truncation(stderr: i32, at: u64, err: IoError) {
    let line = format!("record: truncated at {at}: {}\n", format_io_error(err));
    // SAFETY: the launcher keeps stderr open for the duration of the run.
    unsafe { crate::os::diagnostic(stderr, line.as_ptr(), line.len()) };
}

fn stopped(run: &mut Run, message: impl std::fmt::Display) -> u8 {
    let line = if run.mode != 0 {
        format!("replay: {message}\n")
    } else {
        format!("tape: {message}\n")
    };
    // SAFETY: configured fd is borrowed through this call, line owns its bytes.
    unsafe {
        crate::os::diagnostic(run.stderr, line.as_ptr(), line.len());
    }
    run.stop = Some(if run.mode != 0 {
        1
    } else {
        process::NOT_STARTED
    });
    1
}
unsafe extern "C" fn refuse_wait(_: *mut u8, _: *mut Request, _: u64) -> Wait {
    Wait::default()
}
unsafe extern "C" fn refuse_issue(_: *mut u8, _: *mut Request, _: u64) -> Step {
    done(REFUSED)
}
unsafe extern "C" fn refuse_resume(_: *mut u8, _: *mut Request, _: u64) -> Step {
    done(REFUSED)
}
unsafe extern "C" fn refuse_abort(_: *mut u8, _: *mut Request, _: u64, _: u8) -> Outcome {
    REFUSED
}
unsafe extern "C" fn refuse_deliver(_: *mut u8, _: *mut Request, o: Outcome) -> Outcome {
    o
}
const NO_HOST: Provider = Provider {
    state: ptr::null_mut(),
    wait: refuse_wait,
    issue: refuse_issue,
    resume: refuse_resume,
    abort: refuse_abort,
    deliver: refuse_deliver,
};
/// Build a portable Run outcome, excluding diagnostic sites (R9).
unsafe fn ending(context: *const Context, failed: u8, result: &MainResult) -> Ending {
    // SAFETY: the run owns its cause chain until process reporting completes.
    unsafe {
        if failed == 0 {
            let exit = match result.kind {
                0 => 0u32,
                2 if (1..=100).contains(&result.exit) => u32::from(result.exit),
                _ => 1,
            };
            return Ending::returned(&exit.to_le_bytes());
        }
        let items = (*context)
            .causes
            .iter()
            .map(|cause| {
                let text = cause.message().map(|m| {
                    if m.len == 0 {
                        &[][..]
                    } else {
                        std::slice::from_raw_parts(m.data, m.len as usize)
                    }
                });
                (cause.kind(), text)
            })
            .collect::<Vec<_>>();
        let kind = if items.first().is_some_and(|c| c.0 == causes::CANCELLED) {
            END_CANCELLED
        } else {
            END_FAILED
        };
        Ending::causes(kind, items)
    }
}
fn tape_directory<'a>(
    sink: &mut crate::os::OsProvider,
    path: &'a [u8],
) -> Result<(Inner, &'a [u8]), String> {
    let path = Path::new(OsStr::from_bytes(path));
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path.file_name().ok_or("expected a tape file path")?;
    let handle = sink.grant(parent).map_err(|e| e.to_string())?;
    Ok((
        Inner {
            provider: sink.provider(),
            handle,
        },
        name.as_bytes(),
    ))
}

unsafe fn record_header(
    context: *mut Context,
    config: *const Config,
    run: &Run,
    tape_id: [u8; 16],
) -> Result<Header, Fault> {
    // SAFETY: startup pins the launch inputs and their owned grants.
    unsafe {
        let mut arg = Vec::new();
        put_varint(&mut arg, (*config).args_len as u64);
        for i in 0..(*config).args_len {
            let b = &*(*config).args.add(i);
            put_varint(&mut arg, b.len as u64);
            if b.len != 0 {
                arg.extend_from_slice(std::slice::from_raw_parts(b.data, b.len));
            }
        }
        write_launch(&mut arg, &*(*config).launch, !run.host.is_null());
        let mut net_rules = vec![];
        if !(*(*config).launch).is_describe() {
            let net = entry::hirsel_core_entry_grant(config, entry::NET, 0);
            if net != 0 {
                let handle = entry::hirsel_core_capability_handle(context, net);
                for rule in (*(handle as *const crate::net::Rules)).as_slice() {
                    let Some(rule) = rule.decode() else {
                        return Err(Fault::Corrupt);
                    };
                    net_rules.push(rule);
                }
            }
        }
        let mut secret_grants = Vec::new();
        if !(*(*config).launch).is_describe() {
            for i in 0..run.requirements_len {
                let req = &*run.requirements.add(i);
                if req.kind == entry::SECRET {
                    let source = entry::hirsel_core_entry_grant(config, req.kind, req.index);
                    let handle = entry::hirsel_core_capability_handle(context, source);
                    secret_grants.push((*(handle as *const crate::secret::Granted)).record.clone());
                }
            }
        }
        let env = if !(*(*config).launch).is_describe() {
            let source = entry::hirsel_core_entry_grant(config, entry::ENV, 0);
            (source != 0).then(|| {
                let handle = entry::hirsel_core_capability_handle(context, source);
                crate::env::from_handle(handle).record()
            })
        } else {
            None
        };
        Ok(Header {
            kind: KIND_RUN,
            tape_id,
            identity: run.identity.clone(),
            revision: run.revision,
            charge_policy: 0,
            limit: (*config).root_limit,
            bound: 0,
            chooser: crate::tasks::recorded_chooser(),
            arg,
            net_rules,
            secret_grants,
            env,
            map_hash_key: format::MapHashKey((*context).map_hash_key.to_bytes()),
        })
    }
}

unsafe extern "C" fn invoke(
    context: *mut Context,
    config: *const Config,
    result: *mut MainResult,
) -> u8 {
    // SAFETY: process startup pins Config, Run, the entry and all borrowed tables.
    unsafe {
        let run = &mut *(*config).state.cast::<Run>();
        let expect = Expect {
            identity: &run.identity,
            charge_policy: 0,
            limit: (*config).root_limit,
            arg: None,
            across: run.mode == 2,
        };
        let opened = if let Some(replay) = run.replay.take() {
            Tape::open_with_startup(run.revision, TapeInput::Replay(replay), &expect)
        } else {
            let (dir, name) = match tape_directory(&mut run.sink, &run.path) {
                Ok(place) => place,
                Err(error) => return stopped(run, error),
            };
            let mut tape_id = run.record_id.unwrap_or_default();
            if run.record_id.is_none() {
                let n = libc::getrandom(tape_id.as_mut_ptr().cast(), tape_id.len(), 0);
                if n != tape_id.len() as isize {
                    return stopped(run, std::io::Error::last_os_error());
                }
                // The unit recording law uses a fixed tape id.
                if let Some(v) = std::env::var_os("HIRSEL_TAPE_ID_FIXED") {
                    tape_id = [0; 16];
                    tape_id[..8].copy_from_slice(&(v.len() as u64 + 0x5eed).to_le_bytes());
                }
            }
            let header = match record_header(context, config, run, tape_id) {
                Ok(header) => header,
                Err(error) => return stopped(run, error),
            };
            Tape::open_with_startup(
                run.revision,
                TapeInput::Record {
                    dir,
                    name,
                    header: &header,
                },
                &expect,
            )
        };
        let (tape, startup) = match opened {
            Ok(t) => t,
            Err(e) => return refused(run, e),
        };
        if run.mode != 0 {
            tape.set_replay_abort(ReplayAbort {
                stderr: run.stderr,
                sources: run.sources,
                diagnose: run.mode == 2,
            });
        }
        let (launch, managed) = if run.mode == 0 {
            ((*(*config).launch).clone(), !run.host.is_null())
        } else {
            match read_launch(&startup.arg) {
                Ok(launch) => launch,
                Err(_) => return stopped(run, Fault::Corrupt),
            }
        };
        *run.launch = launch;
        if run.launch.is_describe() {
            let source = if run.mode == 0 {
                Provider {
                    state: (run as *mut Run).cast(),
                    issue: write_describe,
                    ..NO_HOST
                }
            } else {
                NO_HOST
            };
            run.describe_tape = match tape.on_tape(entry::STDOUT, (source, 0)) {
                Ok(p) => p,
                Err(f) => return stopped(run, f),
            };
        }
        if tape.revision_differs() {
            let label = b"replay: revision differs\n";
            crate::os::diagnostic(run.stderr, label.as_ptr(), label.len());
        }
        let replay_net_rules = (!startup.net_rules.is_empty())
            .then(|| crate::net::RulesStorage::new(startup.net_rules));
        let replay_env = startup.env.map(crate::env::Env::new);
        let files = (*context).host_files;
        (*files).secret_mac_tape = Some(tape.secret_mac());
        let mut secret_grants = startup.secret_grants.into_iter();
        for i in 0..run.requirements_len {
            let req = &*run.requirements.add(i);
            let source = entry::hirsel_core_entry_grant(config, req.kind, req.index);
            let value = if req.kind == entry::ARGS {
                let mut argv = Vec::new();
                for n in 0..(*config).args_len {
                    let b = &*(*config).args.add(n);
                    argv.push(if b.len == 0 {
                        &[][..]
                    } else {
                        std::slice::from_raw_parts(b.data, b.len)
                    });
                }
                match tape.on_tape_args(&argv) {
                    Ok(v) => (*files).registry.vector(v),
                    Err(f) => return stopped(run, f),
                }
            } else {
                let (provider, handle) = if run.mode == 0 {
                    (
                        *entry::hirsel_core_capability_provider(context, source),
                        entry::hirsel_core_capability_handle(context, source),
                    )
                } else {
                    (*(*config).provider, 0)
                };
                let taped = if format::is_stream_grant(req.kind) {
                    let terminal = run.mode == 0
                        && (*(*config).capabilities.add((source - 1) as usize)).terminal;
                    tape.on_tape_stream(req.kind, (provider, handle), terminal)
                } else {
                    tape.on_tape(req.kind, (provider, handle))
                        .map(|(p, h)| (p, h, false))
                };
                match taped {
                    Ok((p, h, terminal)) => {
                        if req.kind == entry::NET {
                            if run.mode != 0 && !run.launch.is_describe() {
                                let Some(rules) = &replay_net_rules else {
                                    return stopped(run, Fault::Corrupt);
                                };
                                tape.bind_replay_net_rules(h, rules.handle());
                            }
                            (*files).net_tape = Some(crate::net::SocketTape::new(p, h));
                        }
                        if req.kind == entry::SECRET {
                            let mut secret = if run.launch.is_describe() {
                                crate::secret::Secret {
                                    origin: crate::secret::Origin::Granted(0),
                                    policy: hirsel_core_net::SecretPolicy::default(),
                                    bytes: None,
                                }
                            } else if run.mode == 0 {
                                (*(handle as *const crate::secret::Granted)).secret.clone()
                            } else {
                                let Some(record) = secret_grants.next() else {
                                    return stopped(run, Fault::Corrupt);
                                };
                                let name = crate::tls::view_bytes(&*run.names.add(i));
                                if record.parameter.as_bytes() != name {
                                    return stopped(run, Fault::Corrupt);
                                }
                                crate::secret::Secret {
                                    origin: crate::secret::Origin::Granted(0),
                                    policy: hirsel_core_net::SecretPolicy::parse(&record.patterns)
                                        .unwrap(),
                                    bytes: None,
                                }
                            };
                            secret.origin = crate::secret::Origin::Granted(ordinal_of(h) as u64);
                            (*files)
                                .registry
                                .run_grants()
                                .secrets
                                .lock()
                                .unwrap()
                                .register(secret)
                        } else if req.kind == entry::ENV {
                            let handle = if run.launch.is_describe() || run.mode == 0 {
                                handle
                            } else {
                                let Some(env) = &replay_env else {
                                    return stopped(run, Fault::Corrupt);
                                };
                                env.handle()
                            };
                            (*files).registry.grant(p, handle)
                        } else {
                            (*files).registry.grant_with_terminal(p, h, terminal)
                        }
                    }
                    Err(f) => return stopped(run, f),
                }
            };
            run.grants.push(value);
        }
        if secret_grants.next().is_some() {
            return stopped(run, Fault::Corrupt);
        }
        if managed
            && let Err(fault) = super::managed::install(context, config, run.host.as_ref(), &tape)
        {
            return stopped(run, fault);
        }
        let mut substituted = *config;
        substituted.grants = run.grants.as_ptr();
        substituted.grants_len = run.grants.len();
        substituted.launch = &*run.launch;
        let failed = (run.entry)(context, &substituted, result);
        let outcome = ending(context, failed, &*result);
        match tape.end_with_truncation(&outcome) {
            Ok(None) => {
                if run.mode != 0 {
                    run.stop = Some(0);
                    0
                } else {
                    failed
                }
            }
            Ok(Some(truncated)) => {
                run.record_truncated = Some(truncated);
                failed
            }
            Err(f) => stopped(run, f),
        }
    }
}

/// Startup entry for a recording build (`hirsel build --recording` / `hirsel run --record`).
/// Reads and removes `HIRSEL_RECORD` before any grant snapshot is taken.
/// With HIRSEL_RECORD unset, forward to the ordinary runtime entry.
/// # Safety
/// The generated config, names, source tables, entry and 32-byte revision stay
/// pinned for the call. Identity is a NUL-terminated native string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_os_run_recording(
    input: *const OsConfig,
    names: *const View,
    sources: *const SourceTable,
    entry: Entry,
    identity: *const libc::c_char,
    revision: *const [u8; 32],
) -> u8 {
    // SAFETY: process startup is single-threaded before any task or grant snapshot.
    unsafe {
        if let Some((path, mode)) = pinned_replay(&*input) {
            return run_taped(
                input, names, sources, entry, path, mode, identity, revision, None,
            );
        }
        let path = std::env::var_os("HIRSEL_RECORD");
        let id = std::env::var_os("HIRSEL_RECORD_ID");
        std::env::remove_var("HIRSEL_RECORD");
        std::env::remove_var("HIRSEL_RECORD_ID");
        let Some(path) = path else {
            return entry::hirsel_core_os_run_with_sources(input, names, sources, entry);
        };
        let record_id = match id {
            None => None,
            Some(id) => match entry::prepare_hex(id.as_os_str().as_bytes()) {
                Ok(id) => Some(id),
                Err(_) => return process::NOT_STARTED,
            },
        };
        super::writer::start();
        // A recorded run is its root alone: helper units run on their own
        // threads and are not recorded (see units.rs, "A unit recording").
        let status = run_taped(
            input,
            names,
            sources,
            entry,
            path.as_os_str().as_bytes().to_vec(),
            0,
            identity,
            revision,
            record_id,
        );
        super::writer::flush();
        for (at, err) in super::writer::take_failures() {
            report_truncation((*input).stderr_fd, at, err);
        }
        status
    }
}

fn pinned_replay(input: &OsConfig) -> Option<(Vec<u8>, u64)> {
    if input.args_len != 3 {
        return None;
    }
    // SAFETY: the native launcher pins exactly args_len View records at entry.
    let args = unsafe { std::slice::from_raw_parts(input.args, input.args_len) };
    let command = unsafe { crate::tls::view_bytes(&args[0]) };
    if command != b"--hirsel-replay-recording" {
        return None;
    }
    let mode = match unsafe { crate::tls::view_bytes(&args[1]) } {
        b"same" => 1,
        b"diagnose" => 2,
        _ => return None,
    };
    let path = unsafe { crate::tls::view_bytes(&args[2]) }.to_vec();
    Some((path, mode))
}

/// Launcher-only replay entry (`hirsel replay --tape PATH [--diagnose]`).
/// Replay is never reachable from the program's environment.
/// # Safety
/// The generated config, names, source tables, entry and 32-byte revision stay
/// pinned for the call. Path and identity are NUL-terminated native strings.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_os_replay(
    input: *const OsConfig,
    names: *const View,
    sources: *const SourceTable,
    entry: Entry,
    path: *const libc::c_char,
    mode: u64,
    identity: *const libc::c_char,
    revision: *const [u8; 32],
) -> u8 {
    // SAFETY: every pointer is a borrowed generated startup operand.
    unsafe {
        run_taped(
            input,
            names,
            sources,
            entry,
            CStr::from_ptr(path).to_bytes().to_vec(),
            mode,
            identity,
            revision,
            None,
        )
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn run_taped(
    input: *const OsConfig,
    names: *const View,
    sources: *const SourceTable,
    entry: Entry,
    path: Vec<u8>,
    mode: u64,
    identity: *const libc::c_char,
    revision: *const [u8; 32],
    record_id: Option<[u8; 16]>,
) -> u8 {
    // SAFETY: every pointer is a borrowed generated startup operand.
    unsafe {
        let input = &*input;
        let sink = match (
            entry::duplicate_stream(input.stderr_fd),
            entry::duplicate_stream(input.stderr_fd),
        ) {
            (Ok(out), Ok(err)) => Box::new(crate::os::OsProvider::from_streams(out, err)),
            _ => return process::NOT_STARTED,
        };
        let stderr = sink.stderr_fd();
        let mut run = Run {
            entry,
            requirements: input.parameters,
            names,
            requirements_len: input.parameters_len,
            path,
            identity: CStr::from_ptr(identity).to_bytes().to_vec(),
            revision: *revision,
            mode,
            sources,
            stderr,
            diagnostic: vec![],
            stop: None,
            record_truncated: None,
            grants: vec![],
            sink,
            launch: Box::new(Metadata::source(
                "",
                hirsel_core_ids::launch::DEFAULT_N_PLACE,
                false,
            )),
            describe_output: None,
            describe_tape: (NO_HOST, 0),
            host: ptr::null(),
            record_id,
            replay: None,
        };
        let opened = if mode == 0 {
            match entry::OsProcess::new(
                &input.with_default_operations(),
                crate::os::OsProvider::probe_directory,
                entry::native_metadata,
            ) {
                Ok(process) => Some(process),
                Err(error) => {
                    entry::report_startup(error, run.stderr, names);
                    return process::NOT_STARTED;
                }
            }
        } else {
            None
        };
        if let Some(host) = opened.as_ref().and_then(|process| process.host_launch()) {
            run.host = host;
        }
        let no_host = NO_HOST;
        let mut metadata = Vec::new();
        let mut config = match &opened {
            Some(process) => process.config,
            None => {
                for i in 0..input.parameters_len {
                    metadata.push(Grant {
                        kind: (*input.parameters.add(i)).kind,
                        provider: &no_host,
                        handle: 0,
                        terminal: false,
                    });
                }
                Config {
                    root_limit: input.root_limit,
                    args: ptr::null(),
                    args_len: 0,
                    grants: ptr::null(),
                    grants_len: 0,
                    provider: &no_host,
                    state: ptr::null_mut(),
                    diagnostic,
                    describe: Some(describe),
                    scheduler: crate::tasks::SchedulerProvider::default(),
                    stop_fd: stderr,
                    capabilities: metadata.as_ptr(),
                    capabilities_len: metadata.len(),
                    close_grants: None,
                    launch: &*run.launch,
                    describe_output: None,
                }
            }
        };
        run.describe_output = config.describe_output.map(|write| (write, config.state));
        config.state = (&mut run as *mut Run).cast();
        config.diagnostic = diagnostic;
        config.describe = Some(describe);
        config.close_grants = None;
        config.describe_output = Some(emit_describe);
        let status = if mode == 0 {
            process::hirsel_core_run(&config, invoke)
        } else {
            let (dir, name) = match tape_directory(&mut run.sink, &run.path) {
                Ok(place) => place,
                Err(error) => return stopped(&mut run, error),
            };
            let replay = match ReplayInput::open(dir, name) {
                Ok(replay) => replay,
                Err(error) => return refused(&mut run, error),
            };
            if replay.stored.kind != KIND_RUN {
                return refused(&mut run, OpenError::Unreadable(Fault::Corrupt));
            }
            let key = replay.stored.map_hash_key.0;
            run.replay = Some(replay);
            process::run_with_map_hash_key(&config, invoke, key, None)
        };
        if let Some((at, err)) = run.record_truncated {
            report_truncation(run.stderr, at, err);
        }
        run.stop.unwrap_or(status)
    }
}

#[cfg(test)]
pub(super) fn check_launch_replay() {
    // The entropy-denial filter is permanent for its thread.
    std::thread::spawn(check_launch_replay_without_entropy)
        .join()
        .unwrap();
}

#[cfg(test)]
fn check_launch_replay_without_entropy() {
    fn deny_getrandom() {
        let mut filter = [
            libc::sock_filter {
                code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                jt: 0,
                jf: 0,
                k: 0,
            },
            libc::sock_filter {
                code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                jt: 0,
                jf: 1,
                k: libc::SYS_getrandom as u32,
            },
            libc::sock_filter {
                code: (libc::BPF_RET | libc::BPF_K) as u16,
                jt: 0,
                jf: 0,
                k: libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32,
            },
            libc::sock_filter {
                code: (libc::BPF_RET | libc::BPF_K) as u16,
                jt: 0,
                jf: 0,
                k: libc::SECCOMP_RET_ALLOW,
            },
        ];
        let program = libc::sock_fprog {
            len: filter.len() as u16,
            filter: filter.as_mut_ptr(),
        };
        // SAFETY: these calls affect only the fixture thread and the kernel copies the filter.
        unsafe {
            assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
            assert_eq!(
                libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program),
                0
            );
            let mut byte = 0u8;
            assert_eq!(libc::getrandom((&mut byte as *mut u8).cast(), 1, 0), -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ENOSYS)
            );
        }
    }
    #[derive(Debug, PartialEq)]
    struct Snapshot {
        digest: Vec<u8>,
        target: Vec<u8>,
        n_place: u64,
        describe: u64,
        emitted: bool,
        key: format::MapHashKey,
    }
    thread_local! {
        static OBSERVED: std::cell::RefCell<Vec<Snapshot>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    unsafe extern "C" fn entry(context: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
        // SAFETY: process startup pins the context and launch views through entry.
        unsafe {
            let word = |field, index| process::hirsel_core_launch_word(context, field, index);
            let mut target = empty_buffer();
            process::hirsel_core_launch_target(context, &mut target);
            let snapshot = Snapshot {
                digest: (0..word(1, 0)).map(|i| word(2, i) as u8).collect(),
                target: std::slice::from_raw_parts(target.data, target.len).to_vec(),
                n_place: word(0, 0),
                describe: word(5, 0),
                key: format::MapHashKey((*context).map_hash_key.to_bytes()),
                emitted: process::hirsel_core_describe_emit(
                    context,
                    &Buffer {
                        data: b"describe\n".as_ptr().cast_mut(),
                        len: 9,
                        capacity: 0,
                    },
                ),
            };
            OBSERVED.with_borrow_mut(|seen| seen.push(snapshot));
        }
        0
    }
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    use std::os::fd::AsRawFd;
    let output = std::fs::File::create(root.join("describe")).unwrap();
    let path = std::ffi::CString::new(root.join("run.tape").as_os_str().as_bytes()).unwrap();
    for mode in [0, 1] {
        if mode == 1 {
            deny_getrandom();
        }
        let (target, arguments): (&[u8], &[&[u8]]) = if mode == 0 {
            (
                b"record-target",
                &[b"--hirsel-describe", b"--hirsel-n-place=73"],
            )
        } else {
            (b"replay-target", &[b"--hirsel-n-place=1"])
        };
        let args: Vec<_> = arguments
            .iter()
            .map(|a| View {
                data: a.as_ptr(),
                len: a.len(),
            })
            .collect();
        let input = OsConfig {
            root_limit: 1 << 20,
            args: args.as_ptr(),
            args_len: args.len(),
            directories: ptr::null(),
            directories_len: 0,
            grant_mask: 0,
            stdout_fd: output.as_raw_fd(),
            stderr_fd: 2,
            parameters: ptr::null(),
            parameters_len: 0,
            net_rules: ptr::null(),
            net_rules_len: 0,
            directory_ops: std::ptr::null(),
            process_ops: std::ptr::null(),
            socket_activation: std::ptr::null(),
            secret_grants: core::ptr::null(),
            secret_grants_len: 0,
            env_patterns: crate::process::View {
                data: std::ptr::null(),
                len: 0,
            },
            secret_ops: std::ptr::null(),
            env_ops: std::ptr::null(),
            target: View {
                data: target.as_ptr(),
                len: target.len(),
            },
        };
        // SAFETY: all startup tables and names remain pinned until the run ends.
        let status = unsafe {
            run_taped(
                &input,
                ptr::null(),
                ptr::null(),
                entry,
                path.as_bytes().to_vec(),
                mode,
                c"launch-case".as_ptr(),
                &[7; 32],
                None,
            )
        };
        assert_eq!(status, 0);
    }
    let seen = OBSERVED.with(|seen| seen.take());
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].digest.len(), 32);
    assert_eq!(seen[0].target, b"record-target");
    assert_eq!(seen[0].n_place, 73);
    assert_eq!(seen[0].describe, 1);
    assert!(seen[0].emitted);
    let bytes = std::fs::read(root.join("run.tape")).unwrap();
    let mut data = bytes.as_slice();
    let (_, header) = Blocks::open(|buf| std::io::Read::read(&mut data, buf)).unwrap();
    assert_eq!(seen[0].key, header.map_hash_key);
    assert_eq!(seen[1], seen[0]);
    assert_eq!(std::fs::read(root.join("describe")).unwrap(), b"describe\n");
}
