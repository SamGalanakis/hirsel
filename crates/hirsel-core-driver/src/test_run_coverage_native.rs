// Coverage laws across returnable waits, run from the prog4 LLVM harness
// (`test_run_native.rs`).
const KNOWN_WAITS: &str = r#"
type Job { tasks: Tasks, n: u64 }
fn leaf(take n: u64) -> u64 { n + 10u64 }
fn worker(take job: Job) -> u64 waits {
    let mut out = 0u64
    if job.n == 9u64 { out = 1u64 }
    let h = task.spawn(job.tasks, bytes 16384, job.n, leaf)
    let got = result.unwrap(task.join(take h))
    if got > 5u64 { out + got } else { out + got + 100u64 }
}
@test
fn waits(tasks: Tasks) -> Result[(), ()] waits {
    let h = task.spawn(tasks, bytes 262144, Job(tasks: tasks, n: 2u64), worker)
    if result.unwrap(task.join(take h)) == 12u64 { Ok(()) } else { Error(()) }
}
"#;

/// Known arms across an owned wait: `worker` is a spawned task's callee that
/// joins a child, so a release build gives it a returnable variant whose
/// blocks are not its source's (a new block 0, the entry moved, the block
/// with the join split). Its arms by hand: `n == 9` only `no` (n is 2),
/// `got > 5` only `yes` (got is 12), and `waits`'s `== 12u64` only `yes`.
fn coverage_across_waits(backend: Backend, optimize: bool) -> BTreeMap<String, bool> {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default().check(prog4_request(KNOWN_WAITS, dir.path()), &mut Meter::new(false)).unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let test = DefPath::from_full("app@1/main/waits").unwrap();
    let mut options = NativeOptions { optimize, coverage: Some([package("app")].into()), ..native_options(backend, dir.path()) };
    options.grant_mask = 1 << crate::native::grant_kind("Tasks").unwrap();
    options.root_limit = 1 << 22;
    let outcome = product.run_test(&test, &options).unwrap_or_else(|stop| panic!("{backend:?} {optimize}: {stop}"));
    assert!(outcome.passed(), "{outcome:?}");
    let reached: BTreeMap<_, _> = coverage_reached(&outcome.coverage).into_iter().filter(|(key, _)| key.starts_with("app@1/")).collect();
    let mut expected = BTreeMap::new();
    // An item starts at its attribute; the task entry adapter of each
    // function that spawns has an entry site.
    let arms = [("leaf", "fn leaf(", &[][..]), ("worker", "fn worker(", &[("job.n == 9u64", false, true), ("got > 5u64", true, false)]), ("waits", "@test", &[("result.unwrap(task.join(take h)) == 12u64", true, false)])];
    for (name, start, arms) in arms {
        let item = &KNOWN_WAITS[KNOWN_WAITS.find(start).unwrap()..];
        expected.insert(format!("app@1/main/{name}#e"), true);
        if name != "leaf" {
            expected.insert(format!("app@1/main/{name}$task_entry0#e"), true);
        }
        for &(text, yes, no) in arms {
            let start = item.find(text).unwrap();
            let key = format!("app@1/main/{name}@{start}-{}.0:", start + text.len());
            expected.extend([(format!("{key}yes"), yes), (format!("{key}no"), no)]);
        }
    }
    assert_eq!(reached, expected, "{backend:?} {optimize}");
    reached
}

/// Engines agree across a returnable wait: Cranelift's reached sites are the
/// oracle for LLVM dev and release.
fn engines_agree_across_waits() {
    let cranelift = coverage_across_waits(Backend::Cranelift, false);
    assert_eq!(coverage_across_waits(Backend::Llvm, false), cranelift);
    assert_eq!(coverage_across_waits(Backend::Llvm, true), cranelift);
}

const CORE_LEAVES: &str = r#"
@test
fn leaves() -> Result[(), ()] {
    let s = string.slice("hex 7f", 4u64, 6u64)
    let b = string.as_bytes(s)
    let mut m: Map[u64, u64] = map.new()
    map.insert(mut m, 5u64, string.byte_len(s))
    match (int.ascii_digit(b[0], 16u32), map.get(m, 5u64)) {
        (Some(d), Some(n)) => if d == 7u8 && n == 2u64 { Ok(()) } else { Error(()) },
        _ => Error(()),
    }
}
"#;

/// The reached sites of `app` and `core` for CORE_LEAVES, which calls two
/// small plain core bodies, `int.ascii_digit` and `string.slice`, and the
/// generic `map.insert` and `map.get` of one key.
fn core_leaves_coverage(backend: Backend, optimize: bool) -> BTreeMap<String, bool> {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default().check(prog4_request(CORE_LEAVES, dir.path()), &mut Meter::new(false)).unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let test = DefPath::from_full("app@1/main/leaves").unwrap();
    let options = NativeOptions { optimize, coverage: Some([package("app"), package("core")].into()), ..native_options(backend, dir.path()) };
    let outcome = product.run_test(&test, &options).unwrap_or_else(|stop| panic!("{backend:?} {optimize}: {stop}"));
    assert!(outcome.passed(), "{outcome:?}");
    coverage_reached(&outcome.coverage)
}

/// Engines agree on plain core bodies: LLVM release keeps every site of
/// `core@1/int/ascii_digit` and `core@1/string/slice`, and reaches the
/// sites Cranelift reaches.
fn engines_agree_on_core_leaves() {
    let cranelift = core_leaves_coverage(Backend::Cranelift, false);
    for key in ["core@1/int/ascii_digit#e", "core@1/string/slice#e", "core@1/map/get#e"] {
        assert_eq!(cranelift.get(key), Some(&true), "{key}");
    }
    let release = core_leaves_coverage(Backend::Llvm, true);
    let differ: Vec<_> = cranelift.keys().chain(release.keys()).filter(|k| cranelift.get(*k) != release.get(*k)).collect();
    assert!(differ.is_empty(), "LLVM release differs from Cranelift at {differ:?}");
}


const WARM_LAW: &str = r#"
fn classify(text: View[String]) -> u8 {
    let raw = string.as_bytes(text)
    if list.len(raw) == 0u64 {
        0u8
    } else if raw[0] == 65u8 {
        1u8
    } else if raw[0] == 66u8 && list.len(raw) > 1u64 {
        2u8
    } else {
        3u8
    }
}
@test
fn law(e: Env) -> Result[(), Exit] {
    let k = match env.get(e, "WARM_INPUT") {
        Ok(Some(text)) => classify(text),
        _ => 9u8,
    }
    if k == 0u8 { Ok(()) } else { Error(Exit(status: k)) }
}
"#;

/// Warm feedback (the runtime's `coverage::warm`): one worker serves
/// every case, A/B/A then B/A/B, and each case's status and flags equal a
/// fresh child's for the same input, record by record. The worker's raw file
/// then holds the OR of every case's flags and a failed outcome, as one
/// ordinary run over those cases would.
fn warm_matches_fresh(backend: Backend, optimize: bool) {
    use std::io::{Read as _, Write as _};
    use std::process::{Command, Stdio};
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default().check(prog4_request(WARM_LAW, dir.path()), &mut Meter::new(false)).unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let test = DefPath::from_full("app@1/main/law").unwrap();
    let mut options = NativeOptions { optimize, coverage: Some([package("app")].into()), ..native_options(backend, dir.path()) };
    options.grant_mask = 1 << crate::native::grant_kind("Env").unwrap();
    options.env_patterns = hirsel_core_net::EnvPatterns::parse("WARM_INPUT");
    let executable = product.build(&test, &options).unwrap().executable;
    let raw = |name: &str| dir.path().join(name);
    let flags = |path: &Path| crate::test_run::records(&std::fs::read(path).unwrap()).unwrap();
    let fresh = |n: usize, input: Option<&str>| {
        let path = raw(&format!("fresh-{n}"));
        let mut command = Command::new(&executable);
        command.env_clear().env("HIRSEL_COVERAGE", &path);
        if let Some(input) = input {
            command.env("WARM_INPUT", input);
        }
        let status = command.status().unwrap().code().unwrap();
        (status, flags(&path))
    };

    let mut worker = Command::new(&executable)
        .env_clear()
        .env("HIRSEL_COVERAGE", raw("warm"))
        .env("HIRSEL_COVERAGE_WARM", "0,1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let (mut requests, mut replies) = (worker.stdin.take().unwrap(), worker.stdout.take().unwrap());
    let mut exchange = |request: &[u8], n: usize| {
        requests.write_all(request).unwrap();
        let mut bytes = vec![0u8; n];
        replies.read_exact(&mut bytes).unwrap();
        bytes
    };
    let word = |bytes: &[u8], at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let greeting = exchange(&[], 16);
    assert_eq!((&greeting[..8], word(&greeting, 8)), (&b"HIRSELWM"[..], 1));
    let count = word(&greeting, 12) as usize;
    let layout = exchange(&[], 40 * count);
    let records: Vec<_> = layout.chunks_exact(40).map(|r| (hirsel_core_ids::ContentHash(r[..32].try_into().unwrap()), word(r, 36) as usize)).collect();
    assert!(!records.is_empty(), "a coverage executable has records");
    let mut selection = (count as u32).to_le_bytes().to_vec();
    selection.extend((0..count as u32).flat_map(u32::to_le_bytes));
    exchange(&selection, 0);
    let width: usize = records.iter().map(|&(_, n)| n).sum();

    let case = |exchange: &mut dyn FnMut(&[u8], usize) -> Vec<u8>, entry: &str| {
        let mut request = 1u32.to_le_bytes().to_vec();
        request.extend((entry.len() as u32).to_le_bytes());
        request.extend(entry.as_bytes());
        let reply = exchange(&request, 8 + width);
        assert_eq!(word(&reply, 0), 1);
        (i32::from_le_bytes(reply[4..8].try_into().unwrap()), reply)
    };
    let heap_held = |exchange: &mut dyn FnMut(&[u8], usize) -> Vec<u8>| {
        let sample = exchange(&2u32.to_le_bytes(), 8 + 48);
        assert_eq!((word(&sample, 0), word(&sample, 4)), (2, 6));
        assert_eq!(u64::from_le_bytes(sample[48..56].try_into().unwrap()), 1, "threads");
        u64::from_le_bytes(sample[8..16].try_into().unwrap())
    };
    let inputs = [Some("A"), Some("Bq"), Some("A"), Some("Bq"), Some("A"), Some("Bq"), None, Some(""), Some("B"), Some("A")];
    let mut cumulative = BTreeMap::<hirsel_core_ids::ContentHash, Vec<u8>>::new();
    for (n, input) in inputs.into_iter().enumerate() {
        let entry = match input {
            Some(value) => format!("WARM_INPUT={value}\0"),
            None => "WARM_INPUT\0".to_owned(),
        };
        let (status, reply) = case(&mut exchange, &entry);
        let mut warm = BTreeMap::<_, Vec<u8>>::new();
        let mut at = 8;
        for &(id, n) in &records {
            let held = warm.entry(id).or_insert_with(|| vec![0; n]);
            for (held, flag) in held.iter_mut().zip(&reply[at..at + n]) {
                *held |= flag;
            }
            at += n;
        }
        assert_eq!((status, warm.clone()), fresh(n, input), "{backend:?} {optimize}: case {n} {input:?}");
        for (id, flags) in warm {
            let held = cumulative.entry(id).or_insert_with(|| vec![0; flags.len()]);
            held.iter_mut().zip(flags).for_each(|(held, flag)| *held |= flag);
        }
    }
    // Distinct inputs hold no memory past their case: a sample answers six
    // counters and one thread, and the held heap stays put.
    let distinct = |n: usize| format!("WARM_INPUT=Bq{n:062}\0");
    (0..200).for_each(|n| assert_eq!(case(&mut exchange, &distinct(n)).0, 2));
    let before = heap_held(&mut exchange);
    (200..2200).for_each(|n| assert_eq!(case(&mut exchange, &distinct(n)).0, 2));
    let after = heap_held(&mut exchange);
    assert!(after < before + 4096, "{backend:?} {optimize}: 2000 distinct cases held {} bytes", after - before);
    exchange(&3u32.to_le_bytes(), 0);
    drop(exchange);
    drop(requests);
    assert!(worker.wait().unwrap().success());
    let bytes = std::fs::read(raw("warm")).unwrap();
    assert_eq!(crate::test_run::header(&bytes).unwrap(), (None, false), "a case failed, so the run did");
    assert_eq!(flags(&raw("warm")), cumulative);
}

#[test]
#[ignore = "explicit native integration target"]
fn warm_feedback_matches_fresh_children() {
    for (backend, optimize) in [(Backend::Cranelift, false), (Backend::Llvm, false), (Backend::Llvm, true)] {
        warm_matches_fresh(backend, optimize);
    }
}

/// The Engines agree laws of release units: across returnable waits and on
/// plain and generic core bodies.
fn release_engines_agree() {
    engines_agree_across_waits();
    engines_agree_on_core_leaves();
}
