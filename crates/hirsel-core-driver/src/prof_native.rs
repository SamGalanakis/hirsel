fn capture_native_profile(
    program: &Path,
    out: &Path,
    observation: Option<&Path>,
) -> hirsel_prof::Profile {
    let status = hirsel_prof::run(
        &hirsel_prof::Launch {
            executable: program,
            arguments: &[],
            record: None,
            observation: observation.map(|path| hirsel_prof::ObservationOptions {
                path,
                tasks: true,
                mem: None,
            }),
            source_root: program.parent().unwrap(),
        },
        &hirsel_prof::CaptureOptions {
            out,
            freq: 997,
            pmu: Some("cpu-clock"),
            stack_bytes: 32768,
        },
        |path, bytes| crate::prof_command::symbol_source(path, bytes, None),
    )
    .unwrap();
    assert!(status.success(), "{status}");
    hirsel_prof::Profile::open(out).unwrap()
}

#[test]
fn profiler_attach_uses_snapshot_before_sampling_existing_mappings() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("hot.c");
    let executable = dir.path().join("hot");
    let out = dir.path().join("attach.hprof");
    std::fs::write(
        &source,
        r#"
#include <stdio.h>
#include <time.h>

int main(void) {
    struct timespec start, end;
    volatile unsigned long value = 1;
    puts("ready");
    fflush(stdout);
    clock_gettime(CLOCK_THREAD_CPUTIME_ID, &start);
    do {
        for (int i = 0; i < 100000; i++) value = value * 13 + 1;
        clock_gettime(CLOCK_THREAD_CPUTIME_ID, &end);
    } while ((end.tv_sec - start.tv_sec) * 1000000000L + end.tv_nsec - start.tv_nsec < 900000000L);
    return value == 0;
}
"#,
    )
    .unwrap();
    let clang = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/clang");
    command(
        Command::new(clang)
            .args(["-O2", "-fasynchronous-unwind-tables"])
            .arg(&source)
            .arg("-o")
            .arg(&executable),
    )
    .unwrap();
    let mut child = Command::new(&executable)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "ready\n");
    let maps = std::fs::read_to_string(format!("/proc/{}/maps", child.id())).unwrap();
    let hex = |pc: &str| u64::from_str_radix(pc.trim_start_matches("0x"), 16).unwrap();
    let executable_mapping = maps
        .lines()
        .filter(|line| line.ends_with(executable.to_str().unwrap()))
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let range = fields.next()?;
            fields.next()?.contains('x').then_some(range)
        })
        .unwrap();
    let (start, end) = executable_mapping.split_once('-').unwrap();
    let executable_range = hex(start)..hex(end);
    let result = hirsel_prof::attach(
        child.id(),
        Some(Duration::from_millis(250)),
        &hirsel_prof::CaptureOptions {
            out: &out,
            freq: 997,
            pmu: Some("cpu-clock"),
            stack_bytes: 32768,
        },
        |path, bytes| {
            // Keep the target executing while attach resolves the image.
            std::thread::sleep(Duration::from_millis(75));
            crate::prof_command::symbol_source(path, bytes, None)
        },
    );
    assert!(child.wait().unwrap().success());
    result.unwrap();
    let profile = hirsel_prof::Profile::open(&out).unwrap();
    let mut stmt = profile
        .connection()
        .prepare("SELECT raw_pc_hex, unwind_status FROM cpu_sample")
        .unwrap();
    let mut samples = stmt.query([]).unwrap();
    let mut in_image = 0;
    while let Some(row) = samples.next().unwrap() {
        let pc: String = row.get(0).unwrap();
        if executable_range.contains(&hex(&pc)) {
            let status: String = row.get(1).unwrap();
            assert_ne!(status, "unknown_mapping", "application PC {pc}");
            in_image += 1;
        }
    }
    assert!(in_image > 0, "expected CPU samples in the existing mapping");
}

#[test]
fn profiler_keeps_interleaved_fibers_and_source_lines_on_both_backends() {
    const SOURCE: &str = r#"type Endpoints: send { tx: Sender[u64], rx: Receiver[u64] }

fn alpha(take ends: Endpoints) -> u64 waits {
    let mut n = 1u64
    for _ in 0u64 .. 6u64 {
        for i in 0u64 .. 7500000u64 {
            n = (n * 5u64 + i % 19u64) % 2147483647u64
        }
        result.unwrap(channel.send(ends.tx, n))
        n = result.unwrap(channel.receive(ends.rx))
    }
    n
}

fn beta(take ends: Endpoints) -> u64 waits {
    let mut n = 0u64
    for _ in 0u64 .. 6u64 {
        n = result.unwrap(channel.receive(ends.rx))
        for i in 0u64 .. 7500000u64 {
            n = (n * 7u64 + i % 13u64) % 2147483647u64
        }
        result.unwrap(channel.send(ends.tx, n))
    }
    n
}

pub fn main(tasks: Tasks) -> Result[(), Exit] waits {
    let (tx_a, rx_a): (Sender[u64], Receiver[u64]) = channel.new(1)
    let (tx_b, rx_b): (Sender[u64], Receiver[u64]) = channel.new(1)
    let first = task.spawn(tasks, bytes 4096, Endpoints(tx: take tx_a, rx: take rx_b), alpha)
    let second = task.spawn(tasks, bytes 4096, Endpoints(tx: take tx_b, rx: take rx_a), beta)
    let x = result.unwrap(task.join(take first))
    let y = result.unwrap(task.join(take second))
    if x == y { Ok(()) } else { Error(Exit(status: 1)) }
}
"#;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let dir = tempfile::tempdir().unwrap();
        let mut request = source_request(SOURCE, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        assert!(
            product.accepted(),
            "{backend:?}: {}",
            serde_json::to_string(&product.machine_diagnostics().unwrap()).unwrap()
        );
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let mut options = native_options(backend, dir.path());
        options.instance_limit = 512;
        options.grant_mask = 1 << grant_kind("Tasks").unwrap();
        options.runtime = root.join(std::env::var_os("HIRSEL_CORE_RT_PROF_ARCHIVE").unwrap());
        let built = product.build(&entry, &options).unwrap();
        let raw = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        let profile = capture_native_profile(
            &built.executable,
            &dir.path().join("fibers.hprof"),
            Some(raw.path()),
        );
        let db = profile.connection();
        let handoffs: i64 = db
            .query_row(
                "SELECT count(*) FROM causal_edge e JOIN task src ON src.id=e.src_id
             JOIN task dst ON dst.id=e.dst_task_id
             JOIN task_slice w ON w.task_id=dst.id AND w.object_id=e.object_id
             WHERE e.src_kind='task' AND src.runtime_id=4 AND dst.runtime_id=3
               AND w.wait_reason='channel_recv' AND e.ts_ns BETWEEN w.start_ns AND w.end_ns",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            handoffs, 6,
            "{backend:?}: beta wakes alpha on each return handoff"
        );
        let tier: String = db
            .query_row(
                "SELECT value FROM run_meta WHERE key='symbol_tier'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tier, "full_dwarf", "{backend:?}");

        let mut stmt = db
            .prepare(
                "SELECT f.def_path,fr.file,fr.line,fr.col
             FROM cpu_sample s JOIN callsite c ON c.id=s.callsite_id
             JOIN frame fr ON fr.id=c.frame_id JOIN function f ON f.id=fr.function_id
             WHERE s.stack_root='fiber' AND s.unwind_status='fiber_root'
             AND f.def_path IN ('app@1/main/alpha','app@1/main/beta')
             ORDER BY s.ts_ns,s.id",
            )
            .unwrap();
        let ordered = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<u32>>(2)?,
                    row.get::<_, Option<u32>>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        // The two channels force consecutive CPU bursts to switch between
        // alpha and beta. Inspect the order without predicting sample counts.
        let transitions = ordered
            .windows(2)
            .filter(|rows| rows[0].0 != rows[1].0)
            .count();
        assert!(
            transitions >= 2,
            "{backend:?}: sampled {transitions} fiber transitions"
        );
        let mut functions_with_source = BTreeSet::new();
        for (function, file, line, col) in ordered {
            // Compiler-generated instructions can lack a source location.
            let Some(line) = line else {
                continue;
            };
            let valid_line = if function.ends_with("/alpha") {
                3..=13
            } else {
                15..=25
            };
            assert!(
                valid_line.contains(&line)
                    && col.is_some_and(|col| col > 0)
                    && file
                        .as_deref()
                        .is_some_and(|file| file.ends_with(".hirsel")),
                "{backend:?}: {function} at {file:?}:{line}:{col:?}"
            );
            functions_with_source.insert(function);
        }
        assert_eq!(
            functions_with_source.len(),
            2,
            "{backend:?}: {functions_with_source:?}"
        );
    }
}

#[test]
fn profiler_labels_foreign_callback_as_host_stack() {
    shared_foreign_thunk_fixture(true);
}

#[test]
fn profiler_times_provider_callbacks_separately_from_pending() {
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = r#"pub fn main(c: Clock) -> Result[(), Exit] waits {
        host.sleep_until(c, host.mono_ns(c) + 100000000u64)
        Ok(())
    }"#;
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted());
    let mut options = native_options(Backend::Cranelift, dir.path());
    options.runtime = root.join(std::env::var_os("HIRSEL_CORE_RT_PROF_ARCHIVE").unwrap());
    options.grant_mask = 1 << grant_kind("Clock").unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let built = product.build(&entry, &options).unwrap();
    let raw = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
    let profile = capture_native_profile(
        &built.executable,
        &dir.path().join("io.hprof"),
        Some(raw.path()),
    );
    let db = profile.connection();
    let (span, callbacks, pending, wait, issue, deliver, status, error): (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
    ) = db
        .query_row(
            "SELECT end_ns-start_ns,callback_ns,pending_ns,wait_callback_ns,
                    issue_callback_ns,deliver_callback_ns,status,error_tag
             FROM io_op WHERE operation=?1",
            [hirsel_core_ids::host_op::SLEEP_UNTIL],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .unwrap();
    assert_eq!((status, error), (0, 0));
    assert!(wait > 0 && issue > 0 && deliver > 0);
    assert_eq!(callbacks, wait + issue + deliver);
    assert!(pending > 0 && callbacks + pending <= span);
    let timer: i64 = db
        .query_row(
            "SELECT count(*) FROM causal_edge e JOIN task_slice w
           ON w.task_id=e.dst_task_id AND w.object_id=e.object_id
         WHERE e.src_kind='timer' AND w.wait_reason='timer'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(timer, 1);
}

#[test]
fn profiler_replay_memory_peaks_match_the_runtime_metrics() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir(&app).unwrap();
    std::fs::write(
        app.join("main.hirsel"),
        r#"
@compact(u32)
type Stamp {
    n: u64
}

fn stamp(r: Region) -> Result[Stamp in r, Capacity] {
    match build_in(r, bytes 8, fn(mut d: Dest[Stamp]) -> Result[Child[Stamp], BuildError] {
        dest.put(mut d, Stamp(n: 11))
    }) {
        Ok(take built) => Ok(finish(take built)),
        Error(Overflow) => fail("fixed document size"),
    }
}

fn reserve(n: u64) -> u64 {
    let b: Bytes = list.with_capacity(n)
    list.capacity(b)
}

pub fn main() -> Result[(), Exit] {
    let first = result.unwrap(bound bytes 160 { reserve(136) })
    let second = result.unwrap(bound bytes 160 { reserve(120) })
    let refused = bound bytes 160 {
        bound bytes 300 { reserve(8) }
    }
    if let Ok(_) = result.unwrap(refused) { return Error(Exit(status: 4)) }
    if first != 136 || second != 120 { return Error(Exit(status: 2)) }
    let field = region r { result.unwrap(stamp(r)).n }
    if field != 11 { return Error(Exit(status: 3)) }
    Ok(())
}
"#,
    )
    .unwrap();

    let llvm = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    let recording = root.join(std::env::var_os("HIRSEL_CORE_RT_RECORD_ARCHIVE").unwrap());
    let observe = root.join(std::env::var_os("HIRSEL_CORE_RT_PROF_RECORD_ARCHIVE").unwrap());
    let tape = dir.path().join("run.tape");
    let profile = dir.path().join("memory.hprof");
    let args = |command: &[&str], runtime: &Path, options: &[&str]| {
        command
            .iter()
            .copied()
            .chain([
                app.to_str().unwrap(),
                "--backend",
                "cranelift",
                "--source-core",
                "--libraries",
                root.join("core").to_str().unwrap(),
                "--cc",
                llvm.join("bin/clang").to_str().unwrap(),
                "--runtime",
                runtime.to_str().unwrap(),
                "--store",
                dir.path().join("store").to_str().unwrap(),
            ])
            .chain(options.iter().copied())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let (code, _, err) = process::tape_request(
        &args(&["run"], &recording, &["--record", tape.to_str().unwrap()]),
        dir.path(),
    );
    assert_eq!(code, Some(0), "record: {err}");
    let before = std::fs::read(&tape).unwrap();
    let out = process::tape_request_command(
        &args(
            &["prof", "replay"],
            &observe,
            &[
                "--tape",
                tape.to_str().unwrap(),
                "--mem=exact",
                "--out",
                profile.to_str().unwrap(),
            ],
        ),
        dir.path(),
    )
    .env("HIRSEL_PROFILE_CHUNK_BYTES", "4194304")
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "replay: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read(tape).unwrap(), before);

    let quality = hirsel_prof::query(&profile, hirsel_prof::Query::PeakMem, 20).unwrap();
    assert_eq!(quality.quality["capture_mode"], "replay");
    assert_eq!(quality.quality["mem_records_lost"], 0);
    assert_eq!(quality.quality["mem_callsites_exact"], true);
    let profile = hirsel_prof::Profile::open(&profile).unwrap();
    let db = profile.connection();
    let peaks = db
        .prepare(
            "SELECT metric,total_bytes,sum(live_bytes) FROM peak_snapshot
         JOIN peak_callsite ON snapshot_id=id WHERE scope_id IS NULL
         GROUP BY id ORDER BY metric",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    // Charged: root startup (256 + 32) plus the full 160-byte scope.
    // Retained at the region allocation: startup slot, cause cell, inputs,
    // main result, root Task, Region header and one 4 KiB backing request.
    let retained = 16 + 56 + 16 + 24 + 656 + 272 + 4096;
    // The Stamp takes one 24-byte native slot in the first chunk.
    assert_eq!(
        peaks,
        [
            ("charged_bytes".into(), 448, 448),
            ("retained_bytes".into(), retained, retained),
            ("slack_bytes".into(), 4096 - 24, 4096 - 24),
        ]
    );
    let budgets = db
        .prepare(
            "SELECT limit_val,peak_val,refused,exit_ts IS NULL FROM budget_scope_summary
         WHERE scope_kind='bytes' ORDER BY refused",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(budgets, [(160, 136, false, false), (300, 0, true, true)]);
    let task_bytes: i64 = db.query_row(
        "SELECT sum(observed_count-freed_count) FROM mem_alloc_agg WHERE allocation_kind='task_storage'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(task_bytes, 0, "the root Task and both stores close");
}
