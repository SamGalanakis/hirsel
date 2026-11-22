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
