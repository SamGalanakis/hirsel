fn capture_native_profile(program: &Path, out: &Path) -> hirsel_prof::Profile {
    let status = hirsel_prof::run(
        &hirsel_prof::Launch {
            executable: program,
            arguments: &[],
            record: None,
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
        let built = product.build(&entry, &options).unwrap();
        let profile = capture_native_profile(&built.executable, &dir.path().join("fibers.hprof"));
        let db = profile.connection();
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
