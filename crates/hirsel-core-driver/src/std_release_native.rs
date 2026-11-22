// The release std (std-reuse-1520 S1, S2; std-cutover-1569). Two laws over
// the separately lowered std every optimised LLVM build takes:
//
// Independence: a std side is a function of its units' content, toolchain and
// variant. The std product's own prebuild, and a program's build over another
// image of the same std lowering the std cold into its own parts store, hold
// equal key, record, part and lent bytes. A recipe assembled after its source
// and image are gone takes the side its build stored and links the same
// executable. A program checked over library sources whose one std function it
// reaches through other std roots was edited misses the side lowered before
// the edit, and does what the edited std says. Mutant: the std's roots less
// the definitions a program reaches, red on "the std's record".
//
// Semantic differential: a program assembled over the separately lowered std,
// calling it out of line (Parts) or importing and inlining its small bodies
// (Imported), does and reports what the same program's Cranelift development
// build, which holds every std body it reaches as its own object, does: its
// exit, output and failure report, its logical charge report, its failure
// under a root step limit, and how its recorded tape replays. Mutants: the
// std's site points left out of the program's source map, and imported bodies
// whose site rows are rebased to the program, each red on "fails plain
// Imported: what the program does and reports"; imported bodies that charge
// one step more than their parts, red on the same assertion (the printed step
// thresholds).

/// A program over small plain std bodies the program imports and inlines (the
/// std part holds each out of line): it prints the fewest steps under which three
/// std calls finish, which the bodies' step charges fix, then fails inside a
/// std function, whose site only the std's points place.
const STD_FAILS: (&str, &str) = (
    "fails",
    r#"pub fn main(out: Stdout) -> Result[(), Exit] {
    let s = "the quick brown fox jumps over the lazy dog"
    let mut n = 0u64
    let mut done = false
    while !done {
        let found = bound steps n {
            string.find(s, 0, "dog")
        }
        match found {
            Ok(_) => {
                done = true
            }
            Error(_) => {
                n += 1
            }
        }
    }
    let mut m = 0u64
    done = false
    while !done {
        let parsed = bound steps m {
            int.parse_uint("123456789012345678", 10u32)
        }
        match parsed {
            Ok(_) => {
                done = true
            }
            Error(_) => {
                m += 1
            }
        }
    }
    let mut k = 0u64
    done = false
    while !done {
        let finite = bound steps k {
            float.is_finite(float.from_bits64(k))
        }
        match finite {
            Ok(_) => {
                done = true
            }
            Error(_) => {
                k += 1
            }
        }
    }
    let mut line = string.new()
    let _ = string.push_uint(mut line, n, 10u32, 0, ' ')
    let _ = string.push_uint(mut line, m, 10u32, 1, ' ')
    let _ = string.push_uint(mut line, k, 10u32, 1, ' ')
    result.unwrap(stdout.write(out, line));
    let _ = char.digit('a', 1u32)
    Ok(())
}
"#,
);

/// Two tasks that wait once at an owned network wait, then take 1000 loop
/// steps: one spawned with no step bound current, one inside `bound steps
/// 100`. A task keeps the step context it was spawned under for life
/// (MEM-30), across a wait whose frames the release build represents, so the
/// first counts to 1000 and the second stops with OutOfTime: the program
/// prints "1000 OutOfTime".
const STD_WAITS: (&str, &str) = (
    "waits",
    r#"type SpinJob: send {
    socket: Socket,
}

fn spin(take job: SpinJob) -> u64 waits {
    let SpinJob(socket: take socket) = take job
    let (read, write) = net.split(take socket)
    let mut r = take read
    let _ = take write
    let mut buf: Bytes = list.with_capacity(1)
    match host.recv_bounded(mut r, mut buf, 1, Never) {
        Ok(1) => {},
        _ => return 0,
    }
    let mut n: u64 = 0
    while n < 1000 {
        n += 1
    }
    n
}

fn wake(take client: Socket) waits {
    let (read, write) = net.split(take client)
    let _ = take read
    let mut w = take write
    let _ = result.unwrap(host.send_pair(mut w, string.as_bytes("a"), string.as_bytes(""), Never))
}

pub fn main(n: Net, tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let (listening, addr) = result.unwrap(host.listen(n, Addr(ip: V4(2130706433), port: 0), 8))
    let mut listener = take listening
    let free_client = result.unwrap(host.connect(n, addr, Never))
    let (free_socket, _) = result.unwrap(host.accept(mut listener, Never))
    let bounded_client = result.unwrap(host.connect(n, addr, Never))
    let (bounded_socket, _) = result.unwrap(host.accept(mut listener, Never))
    let free = task.spawn(tasks, bytes 65536, SpinJob(socket: take free_socket), spin)
    let spawned = bound steps 100 {
        task.spawn(tasks, bytes 65536, SpinJob(socket: take bounded_socket), spin)
    }
    let bounded = result.unwrap(take spawned)
    wake(take free_client)
    wake(take bounded_client)
    let mut line = string.new()
    match task.join(take free) {
        Ok(v) => string.push_uint(mut line, v, 10u32, 0, ' '),
        Error(_) => {},
    }
    result.unwrap(stdout.write(out, line));
    let stopped = match task.join(take bounded) {
        Error(Failed(causes)) => {
            let mut kind = " other"
            for c in causes {
                kind = match c.kind {
                    OutOfTime => " OutOfTime",
                    _ => " other",
                }
                break
            }
            kind
        },
        _ => " finished",
    }
    result.unwrap(stdout.write(out, stopped));
    Ok(())
}
"#,
);

/// Channel receives in a represented task, which park with the stack given
/// back: owned items, then the close; and a cancel while parked.
const STD_RECEIVES: (&str, &str) = (
    "receives",
    r#"type Drain: send {
    socket: Socket,
    from: Receiver[String],
    acks: Sender[()],
}

fn opened(take socket: Socket) -> bool waits {
    let (read, write) = net.split(take socket)
    let mut r = take read
    let _ = take write
    let mut buf: Bytes = list.with_capacity(1)
    match host.recv_bounded(mut r, mut buf, 1, Never) {
        Ok(1) => true,
        _ => false,
    }
}

fn drain(take job: Drain) -> u64 waits {
    let Drain(socket: take socket, from: take from, acks: take acks) = take job
    if !opened(take socket) { return 0 }
    let mut total: u64 = 0
    loop {
        match channel.receive(from) {
            Ok(take s) => {
                total = total * 10 + list.len(string.as_bytes(s))
                if let Error(_) = channel.send(acks, ()) { return 0 }
            },
            Error(_) => return total,
        }
    }
}

fn stuck(take job: Drain) -> u64 waits {
    let Drain(socket: take socket, from: take from, acks: take acks) = take job
    if !opened(take socket) { return 0 }
    if let Error(_) = channel.send(acks, ()) { return 0 }
    match channel.receive(from) {
        Ok(_) => 1,
        Error(_) => 2,
    }
}

fn wake(take client: Socket) waits {
    let (read, write) = net.split(take client)
    let _ = take read
    let mut w = take write
    let _ = result.unwrap(host.send_pair(mut w, string.as_bytes("a"), string.as_bytes(""), Never))
}

pub fn main(n: Net, tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let (listening, addr) = result.unwrap(host.listen(n, Addr(ip: V4(2130706433), port: 0), 8))
    let mut listener = take listening
    let drain_client = result.unwrap(host.connect(n, addr, Never))
    let (drain_socket, _) = result.unwrap(host.accept(mut listener, Never))
    let stuck_client = result.unwrap(host.connect(n, addr, Never))
    let (stuck_socket, _) = result.unwrap(host.accept(mut listener, Never))
    let (to, from): (Sender[String], Receiver[String]) = channel.new(1)
    let (acks, acked): (Sender[()], Receiver[()]) = channel.new(1)
    let drained = task.spawn(tasks, bytes 65536, Drain(socket: take drain_socket, from: take from, acks: take acks), drain)
    wake(take drain_client)
    let mut k: u64 = 1
    while k <= 3 {
        let mut item = string.new()
        string.push_uint(mut item, k, 10u32, k, '0')
        result.unwrap(channel.send(to, take item))
        result.unwrap(channel.receive(acked))
        k += 1
    }
    let _ = take to
    let mut line = string.new()
    match task.join(take drained) {
        Ok(v) => string.push_uint(mut line, v, 10u32, 0, ' '),
        Error(_) => {},
    }
    result.unwrap(stdout.write(out, line));
    let (held, never): (Sender[String], Receiver[String]) = channel.new(1)
    let (stuck_acks, stuck_acked): (Sender[()], Receiver[()]) = channel.new(1)
    let mut parked = task.spawn(tasks, bytes 65536, Drain(socket: take stuck_socket, from: take never, acks: take stuck_acks), stuck)
    wake(take stuck_client)
    result.unwrap(channel.receive(stuck_acked))
    task.cancel(mut parked)
    let stopped = match task.join(take parked) {
        Error(Cancelled(take causes)) => { let _ = take causes; " Cancelled" },
        _ => " other",
    }
    let _ = take held
    result.unwrap(stdout.write(out, stopped));
    Ok(())
}
"#,
);

/// One check of `source` as project `project` over std imaged into `cache`.
fn std_checked(dir: &Path, project: &str, source: &str, cache: &str) -> Arc<CheckProduct> {
    let libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let project = dir.join(project);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(dir.join(cache)).unwrap();
    std::fs::write(project.join("main.hirsel"), source).unwrap();
    let request = CheckRequest::program(&project, &libraries, &project.join("store")).unwrap();
    let product = Driver::default()
        .check_reachable_with_product(request, &dir.join(cache), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    product
}

fn std_of(
    product: &CheckProduct,
    options: &NativeOptions,
    publication: &crate::native::recipe_build::Publication,
) -> Option<crate::native::std_release::StdSide> {
    publication.std_side(options, Some(product), None).unwrap()
}

/// What a run of a program shows: exit, stdout, stderr, charge report, tape.
type Shown = (Option<i32>, Vec<u8>, Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>);

/// How a program's build takes the std.
#[derive(Clone, Copy, Debug)]
enum StdBuild {
    /// The Cranelift development build: every std body it reaches is its own.
    Dev,
    /// Over the std's parts alone: each call reaches the out-of-line part.
    Parts,
    /// Over the std's parts, with its lent bodies imported for inlining.
    Imported,
}

/// Assemble `product`'s entry as `build` takes the std, and run it with a
/// fresh charge report and tape.
fn std_run(product: &CheckProduct, options: &NativeOptions, build: StdBuild, dir: &Path) -> Shown {
    use crate::native::recipe_build::Publication;
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let code = crate::code_store::CodeStore::new(dir.join("code"));
    let dev = NativeOptions {
        backend: Backend::Cranelift,
        optimize: false,
        ..options.clone()
    };
    let options = if matches!(build, StdBuild::Dev) { &dev } else { options };
    let reached = product.reachable(&entry, options.instance_limit).unwrap();
    let publication = product
        .publish_release(&entry, &reached, options, &code, 16 * 1024 * 1024)
        .unwrap();
    let std = match build {
        StdBuild::Dev => None,
        StdBuild::Parts => std_of(product, options, &publication)
            .map(|std| crate::native::std_release::StdSide { lent: Vec::new(), ..std }),
        StdBuild::Imported => std_of(product, options, &publication),
    };
    let built = Publication::open(&code, publication.root, &crate::request::local_admission_limits())
        .unwrap()
        .assemble_over(options, reached.counters, std)
        .unwrap();
    if let Some(report) = &options.logical_charge_report {
        let _ = std::fs::remove_file(report);
    }
    let tape = dir.join("run.tape");
    let _ = std::fs::remove_file(&tape);
    let mut command = Command::new(&built.executable);
    command.current_dir(dir).env_remove("HIRSEL_RECORD");
    if options.recording && options.replay.is_none() {
        // A fixed tape id, so the two builds' tapes differ only by build.
        command.env("HIRSEL_RECORD", &tape).env("HIRSEL_RECORD_ID", "00112233445566778899aabbccddeeff");
    }
    let run = command.output().unwrap();
    (
        run.status.code(),
        run.stdout,
        run.stderr,
        options.logical_charge_report.as_ref().map(|p| std::fs::read(p).expect("the run wrote its charge report")),
        // Both builds must agree on whether a run leaves a tape.
        options.recording.then(|| std::fs::read(&tape).ok()).flatten(),
    )
}

#[test]
#[ignore = "explicit native integration target"]
fn a_separate_std_is_the_std_products_alone() {
    use crate::native::recipe_build::Publication;
    let dir = tempfile::tempdir().unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let mut parts = Vec::new();
    // The first program's std comes from the std product's own prebuild in
    // its image; the second's image holds none, so its build lowers the std.
    for ((name, source), cache) in [(UNIT_PROGRAMS[1], "cache"), (UNIT_PROGRAMS[2], "cache-b")] {
        let product = std_checked(dir.path(), name, source, cache);
        let mut options = native_options(Backend::Llvm, &dir.path().join(name));
        options.optimize = true;
        options.instance_limit = crate::native::DEFAULT_INSTANCE_LIMIT;
        options.grant_mask = if source.contains("out: Stdout") { 1 << 4 } else { 0 };
        let image = product.image.as_ref().unwrap().store_root().join("release-std");
        if cache == "cache" {
            let runtime = crate::tool_inputs::runtime_identity(hirsel_core_ids::ContentHash::of_bytes(
                &std::fs::read(&options.runtime).unwrap(),
            ));
            product.prebuild_release_std(&options.target, &[(runtime, false)]).unwrap();
        }
        let code = crate::code_store::CodeStore::new(dir.path().join(name).join("code"));
        let reached = product.reachable(&entry, options.instance_limit).unwrap();
        let publication =
            product.publish_release(&entry, &reached, &options, &code, 16 * 1024 * 1024).unwrap();
        let std = std_of(&product, &options, &publication).expect("an optimised build's std");
        let store = Store::new(if cache == "cache" { &image } else { &options.parts }).unwrap();
        let record = store.get(std.key).unwrap().expect("the std record");
        // A build over the prebuild lowers none of the std; one whose image
        // holds none lowers the whole side itself, and says so.
        if cache == "cache" {
            assert_eq!(std.lowered, 0, "a build over the prebuilt std lowers none of it");
        } else {
            assert!(std.lowered > 0, "a build that lowers the std counts what it lowered");
        }
        let bytes: Vec<Vec<u8>> =
            std.objects.iter().map(|object| std::fs::read(&object.path).unwrap()).collect();
        assert!(!std.lent.is_empty(), "the std lends bodies");
        parts.push((std.key, record, bytes, std.lent));
        if cache == "cache-b" {
            // The recipe outlives its inputs: with the program's source and
            // image gone, it assembles over the side its build stored.
            let root = publication.root();
            let built = publication.assemble_with(&options, reached.counters, Some(&product), None).unwrap();
            let first = std::fs::read(&built.executable).unwrap();
            drop(product);
            std::fs::remove_dir_all(dir.path().join(cache)).unwrap();
            std::fs::remove_file(dir.path().join(name).join("main.hirsel")).unwrap();
            std::fs::remove_file(&built.executable).unwrap();
            let again = Publication::open(&code, root, &crate::request::local_admission_limits())
                .unwrap()
                .assemble_with(&options, Default::default(), None, None)
                .unwrap();
            assert!(std::fs::read(&again.executable).unwrap() == first, "the recipe's executable");
        }
    }
    assert_eq!(parts[0].0, parts[1].0, "the std key");
    assert!(parts[0].1 == parts[1].1, "the std's record");
    assert!(parts[0].2 == parts[1].2, "the std's parts");
    assert!(parts[0].3 == parts[1].3, "the std's lent bodies");

    // A std edit the program reaches only through other std roots misses:
    // with `list.find_byte` finding nothing, `string.split_once` finds no
    // separator and the program ends with its status 2 and writes nothing,
    // where over the unedited std it writes "right" and ends 0.
    let libraries = dir.path().join("libraries");
    copy_tree(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core"), &libraries);
    let (name, source) = UNIT_PROGRAMS[2];
    let project = dir.path().join("sources");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("main.hirsel"), source).unwrap();
    let mut options = native_options(Backend::Llvm, &project);
    options.optimize = true;
    options.instance_limit = crate::native::DEFAULT_INSTANCE_LIMIT;
    options.grant_mask = 1 << 4;
    let run = |label: &str| {
        let request = CheckRequest::program(&project, &libraries, &project.join(label)).unwrap();
        let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
        assert!(product.accepted() && product.image.is_none(), "{name} {label}");
        let code = crate::code_store::CodeStore::new(project.join(label).join("code"));
        let reached = product.reachable(&entry, options.instance_limit).unwrap();
        let publication =
            product.publish_release(&entry, &reached, &options, &code, 16 * 1024 * 1024).unwrap();
        let built = publication.assemble_with(&options, reached.counters, Some(&product), None).unwrap();
        let run = Command::new(&built.executable).current_dir(&project).output().unwrap();
        (run.status.code(), run.stdout)
    };
    assert_eq!(run("before"), (Some(0), b"right".to_vec()), "over the unedited std");
    let list = libraries.join("std/list.hirsel");
    let text = std::fs::read_to_string(&list).unwrap();
    let head = "pub fn find_byte(xs: Bytes, start: u64, end: u64, byte: u8) -> Option[u64] {\n";
    assert!(text.contains(head), "list.find_byte");
    std::fs::write(&list, text.replace(head, &format!("{head}    if end >= start {{\n        return None\n    }}\n"))).unwrap();
    assert_eq!(run("after"), (Some(2), Vec::new()), "over the edited std");
}

/// Each corpus program, built for development and over the separate std, under each
/// of `modes` (the options a mode changes from the base); the number of runs
/// whose tape both builds replayed.
fn std_differential(modes: &[&str]) -> usize {
    let dir = tempfile::tempdir().unwrap();
    let runtime = |name: &str| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(std::env::var_os(name).unwrap_or_else(|| panic!("{name}")))
    };
    let mut taped = 0;
    for (name, source) in UNIT_PROGRAMS.into_iter().chain([STD_FAILS, STD_WAITS, STD_RECEIVES]) {
        let product = std_checked(dir.path(), name, source, "cache");
        let project = dir.path().join(name);
        let mut base = native_options(Backend::Llvm, &dir.path().join("shared"));
        base.optimize = true;
        base.instance_limit = crate::native::DEFAULT_INSTANCE_LIMIT;
        base.debug_lines = true;
        // The grants the program's main takes.
        base.grant_mask = [("out: Stdout", "Stdout"), ("n: Net", "Net"), ("tasks: Tasks", "Tasks")]
            .into_iter()
            .filter(|(param, _)| source.contains(param))
            .fold(0, |mask, (_, kind)| mask | 1 << crate::native::grant_kind(kind).unwrap());
        if source.contains("n: Net") {
            base.net_rules = ["Connect 127.0.0.0/8 1-65535", "Listen 127.0.0.0/8 0-65535"]
                .map(|r| hirsel_core_net::GrantRule::parse(r).unwrap())
                .to_vec();
        }
        for &mode in modes {
            let options = match mode {
                "plain" => base.clone(),
                "charges" => NativeOptions {
                    logical_charge_report: Some(project.join("charges")),
                    runtime: runtime("HIRSEL_CORE_RT_OBSERVED_ARCHIVE"),
                    ..base.clone()
                },
                "limited" => NativeOptions { root_limit: 40, ..base.clone() },
                // The release unit, whose failures name their site rows.
                "release" => NativeOptions { debug_lines: false, ..base.clone() },
                "recorded" => NativeOptions {
                    recording: true,
                    runtime: runtime("HIRSEL_CORE_RT_RECORD_ARCHIVE"),
                    ..base.clone()
                },
                _ => unreachable!("{mode}"),
            };
            // The development build is the oracle: a call to the std's part,
            // and an inlined copy of a std body, does and reports what the
            // program's own copy of the std body does.
            let parts = std_run(&product, &options, StdBuild::Dev, &project);
            if (name, mode) == ("receives", "plain") {
                assert_eq!(
                    (parts.0, String::from_utf8_lossy(&parts.1)),
                    (Some(0), "123 Cancelled".into()),
                    "receives: {}",
                    String::from_utf8_lossy(&parts.2)
                );
            }
            if (name, mode) == ("waits", "plain") {
                assert_eq!(
                    (parts.0, String::from_utf8_lossy(&parts.1)),
                    (Some(0), "1000 OutOfTime".into()),
                    "waits: each task keeps its step context: {}",
                    String::from_utf8_lossy(&parts.2)
                );
            }
            let builds = [StdBuild::Dev, StdBuild::Parts, StdBuild::Imported];
            let mut tapes = vec![(StdBuild::Dev, parts.4.clone())];
            for build in [StdBuild::Parts, StdBuild::Imported] {
                let shown = std_run(&product, &options, build, &project);
                assert_eq!(
                    (parts.0, &parts.1, &parts.2),
                    (shown.0, &shown.1, &shown.2),
                    "{name} {mode} {build:?}: what the program does and reports"
                );
                assert_eq!(parts.3, shown.3, "{name} {mode} {build:?}: the charge report");
                assert_eq!(parts.4.is_some(), shown.4.is_some(), "{name} {mode} {build:?}: a tape");
                tapes.push((build, shown.4));
            }
            // A tape names its executable's launch image, so two builds' tapes
            // differ there; each must replay on every build as on its own.
            for (label, tape) in &tapes {
                let Some(tape) = tape else { continue };
                taped += 1;
                let path = project.join(format!("{label:?}.tape"));
                std::fs::write(&path, tape).unwrap();
                let replay = NativeOptions {
                    replay: Some(crate::ReplayPlan {
                        path: path.to_str().unwrap().into(),
                        diagnose: false,
                    }),
                    grant_mask: 0,
                    ..options.clone()
                };
                let on_parts = std_run(&product, &replay, StdBuild::Dev, &project);
                for build in builds {
                    let on = std_run(&product, &replay, build, &project);
                    assert_eq!(
                        (on_parts.0, &on_parts.1, &on_parts.2),
                        (on.0, &on.1, &on.2),
                        "{name}: the {label:?} build's tape replayed on {build:?}"
                    );
                }
                // A faithful replay ends 0 and repeats no live write, even of
                // a failed run (RL1).
                assert_eq!(
                    (on_parts.0, on_parts.1.is_empty()),
                    (Some(0), true),
                    "{name}: the {label:?} build's tape replays"
                );
            }
        }
    }
    taped
}

#[test]
#[ignore = "explicit native integration target"]
fn a_program_over_a_separate_std_does_what_it_does_whole() {
    std_differential(&["plain", "release", "charges", "limited"]);
}

/// Recording opens the tape's directory through the OS Dir provider (openat2),
/// so this body runs on the host.
#[test]
#[ignore = "explicit native integration target"]
fn a_program_over_a_separate_std_records_what_it_records_whole() {
    assert!(std_differential(&["recorded"]) > 0, "a recorded run wrote a tape");
}
