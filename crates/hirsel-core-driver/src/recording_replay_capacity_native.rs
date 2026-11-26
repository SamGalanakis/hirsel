/// Each outer helper joins its child before the next outer helper starts.
/// During replay the edited TAKE ends each outer helper at a divergence,
/// so it stays parked and still holds its unit directory slot.
#[test]
#[ignore = "explicit native integration target"]
fn replay_admits_history_beyond_the_unit_sizing_hint() {
    crate::openat2_required::require();
    use std::os::unix::fs::FileExt;

    const ROOT_LIMIT: u64 = 16_384;
    const OUTERS: usize = 64;
    let logical_max = 1 + ROOT_LIMIT / 288;

    let source = r#"
type Job { tasks: Tasks, n: u64 }

fn inner(take n: u64) -> u64 { n * 3 }

fn outer(take job: Job) -> u64 waits {
    let h = result.unwrap(task.spawn_unit(job.tasks, bytes 1024, job.n, inner))
    result.unwrap(task.join(take h)) + 1
}

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let mut n = 0u64
    while n < 64 {
        let h = result.unwrap(task.spawn_unit(tasks, bytes 4096, Job(tasks: tasks, n: 7), outer))
        if result.unwrap(task.join(take h)) != 22 { return Error(Exit(status: 3)) }
        n = n + 1
    }
    result.unwrap(stdout.write(out, "units"))
    Ok(())
}
"#;
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);

    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let mut options = native_options(Backend::Cranelift, dir.path());
    options.root_limit = ROOT_LIMIT;
    options.runtime = env_record_runtime();
    options.recording = true;
    options.grant_mask = ["Tasks", "Stdout"]
        .iter()
        .fold(0, |m, g| m | 1 << grant_kind(g).unwrap());
    let program = product.build(&entry, &options).unwrap().executable;
    let tape_path = dir.path().join("recorded.tape");
    let recorded = std::process::Command::new(&program)
        .env("HIRSEL_UNTIED", "1")
        .env("HIRSEL_RECORD", &tape_path)
        .output()
        .unwrap();
    assert_eq!(
        (recorded.status.code(), recorded.stdout.as_slice()),
        (Some(0), b"units".as_slice()),
        "{recorded:?}"
    );

    let file = std::fs::File::open(&tape_path).unwrap();
    let (index, header) =
        hirsel_tape_format::Index::open(|at, bytes| file.read_at(bytes, at)).unwrap();
    assert_eq!(header.limit, ROOT_LIMIT);
    let outer: std::collections::BTreeMap<usize, u32> = index
        .paths()
        .filter_map(|(stream, name)| Some((name.strip_prefix("root.")?.parse().ok()?, stream)))
        .collect();
    assert_eq!(
        outer.keys().copied().collect::<Vec<_>>(),
        (0..OUTERS).collect::<Vec<_>>()
    );
    assert!(outer.len() as u64 > logical_max);
    let mut tape = std::fs::read(&tape_path).unwrap();
    for stream in outer.into_values() {
        tape = u4_patched(&tape, &header.tape_id, stream, u4_take_to_99(&[21]));
    }
    let edited = dir.path().join("diverged.tape");
    std::fs::write(&edited, tape).unwrap();

    let replayed = std::process::Command::new(&program)
        .args(["--hirsel-replay-recording", "same"])
        .arg(edited)
        .env("HIRSEL_UNTIED", "1")
        .env_remove("HIRSEL_RECORD")
        .env_remove("HIRSEL_XSTATS")
        .output()
        .unwrap();
    assert_eq!(replayed.status.code(), Some(1), "{replayed:?}");
    assert!(replayed.stdout.is_empty());
    let stderr = String::from_utf8(replayed.stderr).unwrap();
    let lines = stderr.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), OUTERS, "{stderr}");
    for (i, line) in lines.into_iter().enumerate() {
        assert!(
            line.starts_with(&format!("replay: Diverged(Take) at 2 in root.{i} "))
                && line.contains("no helper with ordinal 99"),
            "{line}"
        );
    }
}
