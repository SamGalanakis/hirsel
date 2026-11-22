//! Native Exec provider adapters. The logical launch is checked before issue;
//! opening the working directory and creating the child are delivered effects.
use crate::{
    Context,
    entry::{hirsel_core_capability_handle, hirsel_core_capability_provider},
    exec::Grant,
    os_exec::{Process, Signal, SpawnInput, Termination},
    provider::*,
};
use std::{
    ffi::CString,
    io,
    os::fd::{AsFd, FromRawFd, OwnedFd},
    ptr,
};

const PROC_BIT: u64 = 1 << 63;
pub(crate) const PROC_CLOSE: u8 = 0x80;
const DENIED: Outcome = failure(1, 0);
const TIMED_OUT: Outcome = failure(2, 0);
const ABORTED: Outcome = failure(3, 0);

const fn failure(tag: u8, code: u32) -> Outcome {
    Outcome {
        value: 0,
        error: IoError { tag, code },
        written: 0,
        status: 0,
        kind: 0,
    }
}

fn other(error: io::Error) -> Outcome {
    failure(4, error.raw_os_error().unwrap_or(libc::EIO) as u32)
}

pub(crate) struct LogicalLaunch<'a> {
    pub program: &'a str,
    pub args: Vec<&'a [u8]>,
    pub env: Vec<(&'a str, &'a [u8])>,
    pub dir: &'a str,
    pub path: &'a str,
    pub piped: [bool; 3],
}

impl LogicalLaunch<'_> {
    fn valid(&self) -> bool {
        let fits = |len| u32::try_from(len).is_ok();
        fits(self.program.len())
            && fits(self.dir.len())
            && fits(self.path.len())
            && fits(self.args.len())
            && fits(self.env.len())
            && self
                .args
                .iter()
                .all(|arg| fits(arg.len()) && !arg.contains(&0))
            && self.env.iter().all(|(key, value)| {
                fits(key.len())
                    && fits(value.len())
                    && hirsel_core_net::exec::valid_key(key)
                    && !value.contains(&0)
            })
            && self.env.windows(2).all(|pair| pair[0].0 != pair[1].0)
            && (self.path.is_empty() || hirsel_core_net::valid_relative_path(self.path.as_bytes()))
    }

    fn permitted(&self, grant: &Grant) -> bool {
        let keys: Vec<_> = self.env.iter().map(|(key, _)| *key).collect();
        grant
            .rules
            .permits(self.program, &self.args, &keys, self.dir)
    }
}

struct ProcessHandle {
    provider: Provider,
    handle: u64,
    native: Option<Process>,
    cached: Option<u64>,
}

pub(crate) struct SpawnCall<'a> {
    pub launch: LogicalLaunch<'a>,
    files: *mut crate::host_file::Files,
    process: Option<Process>,
    pub pipes: [u64; 3],
}

pub(crate) struct ProcCall<'a> {
    pub until: Option<(Provider, u64, u64)>,
    native: &'a mut Option<Process>,
}

/// The OS dispatch retains these callbacks only when an Exec adapter is used.
#[repr(C)]
pub(crate) struct Call<T> {
    pub ops: &'static Operations,
    pub data: T,
}

pub(crate) struct Operations {
    pub spawn: unsafe extern "C" fn(*mut u8, *mut Request) -> Outcome,
    pub process: unsafe fn(&mut Request, u64) -> Step,
    pub abort: unsafe fn(&mut Request, u8) -> Outcome,
}

static OPERATIONS: Operations = Operations {
    spawn: spawn_native,
    process: process_native,
    abort: abort_native,
};

pub(crate) unsafe fn operations(r: *const Request) -> &'static Operations {
    // SAFETY: all Exec requests begin with this callback table.
    unsafe { (*(*r).path.cast::<Call<()>>()).ops }
}

fn cwd(base: &str, path: &str) -> io::Result<OwnedFd> {
    let base = CString::new(base).unwrap();
    // SAFETY: the configured label is a validated absolute host path.
    let root = unsafe {
        libc::open(
            base.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if root < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: open returned a fresh descriptor.
    let root = unsafe { OwnedFd::from_raw_fd(root) };
    if path.is_empty() {
        return Ok(root);
    }
    let path = CString::new(path).unwrap();
    hirsel_core_net::unix::open_beneath(root.as_fd(), &path, libc::O_PATH | libc::O_DIRECTORY)
}

unsafe extern "C" fn spawn_native(_: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: a native capability pins Grant. Its adapter pins SpawnCall.
    unsafe {
        let r = &mut *request;
        let call = &mut (*r.path.cast_mut().cast::<Call<SpawnCall>>()).data;
        let grant = &*(r.handle as *const Grant);
        let launch = &call.launch;
        let profile = &grant.profiles[launch.program];
        let Ok(cwd) = cwd(&profile.spec.dirs[launch.dir], launch.path) else {
            return DENIED;
        };
        let argv = std::iter::once(profile.spec.argv0.as_bytes())
            .chain(launch.args.iter().copied())
            .map(|arg| CString::new(arg).unwrap())
            .collect();
        let mut env: Vec<_> = profile
            .spec
            .fixed
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_bytes()))
            .chain(launch.env.iter().copied())
            .collect();
        env.sort_unstable_by_key(|(key, _)| *key);
        let envp = env
            .into_iter()
            .map(|(key, value)| {
                let mut bytes = key.as_bytes().to_vec();
                bytes.push(b'=');
                bytes.extend(value);
                CString::new(bytes).unwrap()
            })
            .collect();
        let sockets = &mut (*call.files).sockets;
        if launch.piped.contains(&true)
            && let Err(error) = sockets.reserve(launch.piped.iter().filter(|pipe| **pipe).count())
        {
            return other(error);
        }
        let result = grant.supervisor.spawn(SpawnInput {
            image: profile.image.file.clone(),
            interpreter: profile.image.interpreter.clone(),
            cwd,
            argv,
            envp,
            piped: launch.piped,
        });
        match result {
            Ok(child) => {
                for (i, fd) in [child.stdin, child.stdout, child.stderr]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(fd) = fd {
                        call.pipes[i] = sockets.insert_pipe(fd, i == 0).unwrap();
                    }
                }
                call.process = Some(child.process);
                Outcome {
                    value: 1,
                    ..Outcome::default()
                }
            }
            Err(error) => other(error.error),
        }
    }
}

fn termination(termination: Termination) -> u64 {
    match termination {
        Termination::Exited(code) => u64::from(code),
        Termination::Signalled(number) => 256 + u64::from(number),
    }
}

unsafe fn process_native(r: &mut Request, token: u64) -> Step {
    // SAFETY: the typed, exclusively borrowed Proc pins this call, and only
    // the native provider can reach this function.
    let call = unsafe { &mut (*r.path.cast_mut().cast::<Call<ProcCall>>()).data };
    let outcome = match r.operation {
        CLOSE => {
            drop(call.native.take());
            Outcome::default()
        }
        EXEC_SIGNAL => {
            let signal = match r.mode {
                0 => Signal::Int,
                1 => Signal::Term,
                2 => Signal::Kill,
                _ => unreachable!(),
            };
            call.native
                .as_mut()
                .unwrap()
                .signal(signal)
                .map_or_else(other, |()| Outcome::default())
        }
        EXEC_WAIT => {
            let process = call.native.as_mut().unwrap();
            // SAFETY: the caller pins that Clock and its designation.
            let due = unsafe { crate::os_net::deadline(call.until, token) };
            if due == crate::os_net::Due::Passed {
                process.withdraw_wait();
                TIMED_OUT
            } else {
                match process.wait_or_park(Some(token)) {
                    Ok(Some(status)) => Outcome {
                        value: termination(status),
                        ..Outcome::default()
                    },
                    Ok(None) => {
                        return Step {
                            kind: STEP_PENDING,
                            deadline_ns: due.host().unwrap_or(0),
                            ..Step::default()
                        };
                    }
                    Err(error) => {
                        process.withdraw_wait();
                        other(error)
                    }
                }
            }
        }
        _ => unreachable!(),
    };
    Step {
        outcome,
        ..Step::default()
    }
}

unsafe fn abort_native(r: &mut Request, why: u8) -> Outcome {
    // SAFETY: the pending wait owns its mutable borrow through abort.
    let call = unsafe { &mut (*r.path.cast_mut().cast::<Call<ProcCall>>()).data };
    call.native.as_mut().unwrap().withdraw_wait();
    if why == ABORT_DEADLINE {
        TIMED_OUT
    } else {
        ABORTED
    }
}

#[repr(C)]
#[derive(Default)]
pub struct SpawnOutcome {
    pub(crate) outcome: Outcome,
    pub(crate) pipes: [u64; 3],
}

/// # Safety
/// The emitters pin the checked Launch field carriers through delivery.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_exec_spawn(
    context: *mut Context,
    exec: u64,
    program: *const Buffer,
    args: *const Buffer,
    env: *const Buffer,
    dir: *const Buffer,
    path: *const Buffer,
    stdin: u8,
    stdout: u8,
    stderr: u8,
    site: u64,
) -> SpawnOutcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let text = |view: *const Buffer| std::str::from_utf8(crate::tls::bytes(&*view)).unwrap();
        let list = |view: *const Buffer| {
            let view = &*view;
            if view.len == 0 {
                &[]
            } else {
                std::slice::from_raw_parts(view.data.cast::<Buffer>(), view.len)
            }
        };
        let env = &*env;
        let pairs = if env.len == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(env.data.cast::<[Buffer; 2]>(), env.len)
        };
        let mut env: Vec<_> = pairs
            .iter()
            .map(|[key, value]| (text(key), crate::tls::bytes(value)))
            .collect();
        env.sort_unstable_by_key(|(key, _)| *key);
        let launch = LogicalLaunch {
            program: text(program),
            args: list(args)
                .iter()
                .map(|arg| crate::tls::bytes(arg))
                .collect(),
            env,
            dir: text(dir),
            path: text(path),
            piped: [stdin != 0, stdout != 0, stderr != 0],
        };
        let provider = *hirsel_core_capability_provider(context, exec);
        let handle = hirsel_core_capability_handle(context, exec);
        let mut call = Call {
            ops: &OPERATIONS,
            data: SpawnCall {
                launch,
                files: (*context).host_files,
                process: None,
                pipes: [0; 3],
            },
        };
        let mut r = request(EXEC_SPAWN, 0, handle, &raw mut call, site);
        let mut outcome = drive_with(context, &provider, &mut r, None, |provider, r| {
            let launch = &(*r.path.cast::<Call<SpawnCall>>()).data.launch;
            #[cfg(feature = "record")]
            let handle = if crate::tape::owns(provider) {
                if let Some((_, handle)) = crate::tape::source(provider, r.handle) {
                    handle
                } else {
                    return false;
                }
            } else {
                r.handle
            };
            #[cfg(not(feature = "record"))]
            let handle = {
                let _ = provider;
                r.handle
            };
            launch.valid() && launch.permitted(&*(handle as *const Grant))
        });
        if outcome.error == IoError::OK && outcome.status == 0 {
            let proc = Box::new(ProcessHandle {
                provider,
                handle: outcome.value,
                native: call.data.process,
                cached: None,
            });
            outcome.value = Box::into_raw(proc) as u64 | PROC_BIT;
        }
        SpawnOutcome {
            outcome,
            pipes: call.data.pipes,
        }
    }
}

fn request<T>(operation: u8, mode: u8, handle: u64, call: *mut Call<T>, site: u64) -> Request {
    Request {
        operation,
        mode,
        handle,
        path: call.cast(),
        path_len: size_of::<Call<T>>(),
        buffer: crate::net::empty_buffer(),
        site,
    }
}

pub(crate) fn is_proc(key: u64) -> bool {
    key & PROC_BIT != 0
}

pub(crate) unsafe fn drop_proc(context: *mut Context, key: u64) {
    // SAFETY: the tagged carrier has one owner, consumed by this finalizer.
    unsafe {
        let mut proc = Box::from_raw((key & !PROC_BIT) as *mut ProcessHandle);
        (*context).cancel_shield += 1;
        let _ = proc_op(context, &mut proc, CLOSE, PROC_CLOSE, None, 0);
        (*context).cancel_shield -= 1;
    }
}

unsafe fn proc_op(
    context: *mut Context,
    proc: &mut ProcessHandle,
    op: u8,
    mode: u8,
    until: Option<(u64, u64)>,
    site: u64,
) -> Outcome {
    let mut call = Call {
        ops: &OPERATIONS,
        data: ProcCall {
            // SAFETY: the caller pins its optional Clock designation.
            until: until
                .and_then(|(clock, at)| unsafe { crate::net::deadline(context, 1, clock, at) }),
            native: &mut proc.native,
        },
    };
    let mut r = request(op, mode, proc.handle, &raw mut call, site);
    // SAFETY: the Proc pins its provider and all operands through delivery.
    unsafe { drive(context, &proc.provider, &mut r, None) }
}

/// # Safety
/// The caller exclusively borrows this live Proc and the optional Clock.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_exec_wait(
    context: *mut Context,
    key: u64,
    until_kind: u8,
    clock: u64,
    at: u64,
    site: u64,
) -> Outcome {
    // SAFETY: the typed carrier pins the record through this exclusive borrow.
    unsafe {
        let proc = &mut *((key & !PROC_BIT) as *mut ProcessHandle);
        if let Some(value) = proc.cached {
            crate::process::deny_describe_capability(context);
            let _commit = crate::tasks::before_provider_entry();
            if crate::tasks::check_cancel(context, site) != 0 {
                return CANCELLED;
            }
            return Outcome {
                value,
                ..Outcome::default()
            };
        }
        let outcome = proc_op(
            context,
            proc,
            EXEC_WAIT,
            0,
            (until_kind != 0).then_some((clock, at)),
            site,
        );
        if outcome.error == IoError::OK && outcome.status == 0 {
            proc.cached = Some(outcome.value);
        }
        outcome
    }
}

/// # Safety
/// The caller exclusively borrows this live Proc and supplies a checked Signal.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_exec_signal(
    context: *mut Context,
    key: u64,
    signal: u8,
    site: u64,
) -> Outcome {
    // SAFETY: the typed carrier pins the record through this exclusive borrow.
    unsafe {
        let proc = &mut *((key & !PROC_BIT) as *mut ProcessHandle);
        if proc.cached.is_some() {
            crate::process::deny_describe_capability(context);
            let _commit = crate::tasks::before_provider_entry();
            if crate::tasks::check_cancel(context, site) != 0 {
                return CANCELLED;
            }
            return Outcome::default();
        }
        proc_op(context, proc, EXEC_SIGNAL, signal, None, site)
    }
}
