// Package walk laws L4, L5 and L12 (packages.md section 13), split from test_run_native.rs.

#[test]
fn package_l4_symlink_escape() {
    let law = PackageLaw::new();
    law.manifest("p", "fixture.test/p", "1.0.0", "[sources]\n\"a.hirsel\" = \"a\"\n");
    law.write("outside.hirsel", PACKAGE_MAIN);
    std::os::unix::fs::symlink("../outside.hirsel", law.path("p/a.hirsel")).unwrap();
    for args in [&["check", "p"][..], &["pack", "p", "--output", "p.hpkg"][..]] {
        PackageLaw::refuse(law.run(args), "p/hirsel.toml: sources: \"a.hirsel\" resolves outside the package");
        assert!(!law.path("p/hirsel.lock").exists());
        assert!(!law.path("p.hpkg").exists());
    }
    std::fs::remove_file(law.path("p/a.hirsel")).unwrap();
    law.write("p/real/a.hirsel", PACKAGE_MAIN);
    std::os::unix::fs::symlink("real/a.hirsel", law.path("p/a.hirsel")).unwrap();
    PackageLaw::pass(law.run(&["check", "p"]));
    let linked = PackageLaw::pass(law.run(&["pack", "p", "--output", "p.hpkg"]));
    law.manifest("copy", "fixture.test/p", "1.0.0", "[sources]\n\"a.hirsel\" = \"a\"\n");
    law.write("copy/a.hirsel", PACKAGE_MAIN);
    assert_eq!(linked, PackageLaw::pass(law.run(&["pack", "copy", "--output", "copy.hpkg"])));
}

#[test]
fn package_l5_test_files_do_not_enter_content() {
    let law = PackageLaw::new();
    law.library("jx", "fixture.test/json", "1.0.0", PACKAGE_X_SOURCE,
        "[tests]\nlimit = 67108864\n[tests.sources]\n\"json_laws.hirsel\" = \"json\"\n");
    law.write("jx/json_laws.hirsel",
        "@test\nfn version_law() -> Result[(), ()] { if version() == 1u64 { Ok(()) } else { Error(()) } }\n");
    assert_eq!(PackageLaw::pass(law.run(&["pack", "jx", "--output", "jx.hpkg"])), format!("fixture.test/json@1 1.0.0 {PACKAGE_X}\n"));
    let stdout = PackageLaw::pass(law.run(&["test", "jx", "--json"]));
    let results: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let tests = results["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 1);
    assert_eq!(tests[0]["definition"], "fixture.test/json@1/json/version_law");
    assert_eq!(tests[0]["source"], "executed");
    assert_eq!(tests[0]["passed"], true);
}

#[test]
fn package_l12_walks_never_leave_their_root() {
    let law = PackageLaw::new();
    law.write("W/hirsel.toml", "[workspace]\nmembers = [\"app\", \"lib\"]\n");
    law.app("W/app", "");
    law.library("elsewhere/lib", "local/lib", "1.0.0", PACKAGE_MAIN, "");
    std::os::unix::fs::symlink("../elsewhere/lib", law.path("W/lib")).unwrap();
    PackageLaw::refuse(law.run(&["lock", "W"]), "W/hirsel.toml: workspace.members: \"lib\" resolves outside the workspace");
    assert!(!law.path("W/hirsel.lock").exists());

    law.manifest("p", "fixture.test/p", "1.0.0", "[sources]\n\"a.hirsel\" = \"a\"\n");
    law.write("p/real.hirsel", PACKAGE_MAIN);
    std::os::unix::fs::symlink("..", law.path("p/up")).unwrap();
    std::os::unix::fs::symlink("up/p/real.hirsel", law.path("p/a.hirsel")).unwrap();
    PackageLaw::refuse(law.run(&["check", "p"]), "p/hirsel.toml: sources: \"a.hirsel\" resolves outside the package");
    assert!(!law.path("p/hirsel.lock").exists());
}


fn package_entry(store: &str, root: &str) -> String {
    let hex = root.strip_prefix("sha256:").unwrap();
    format!("{store}/{}/{}", &hex[..2], &hex[2..])
}

fn package_frame(entry: &[u8]) -> Vec<u8> {
    // RFC 8878: a single segment with a one-byte size and one raw last block.
    let size = u8::try_from(entry.len()).unwrap();
    let mut frame = vec![0x28, 0xb5, 0x2f, 0xfd, 0x20, size];
    frame.extend_from_slice(&((u32::from(size) << 3) | 1).to_le_bytes()[..3]);
    frame.extend_from_slice(entry);
    frame
}

fn package_bundle(entries: &[&[u8]]) -> Vec<u8> {
    let frames: Vec<_> = entries.iter().map(|entry| package_frame(entry)).collect();
    let frames: Vec<_> = frames.iter().map(Vec::as_slice).collect();
    package_frames(&frames)
}

fn package_frames(frames: &[&[u8]]) -> Vec<u8> {
    fn varint(mut number: usize, output: &mut Vec<u8>) {
        while number >= 128 {
            output.push(((number % 128) | 128) as u8);
            number /= 128;
        }
        output.push(number as u8);
    }
    let tag = b"hirsel.package-archive.v2";
    let mut bytes = vec![tag.len() as u8];
    bytes.extend_from_slice(tag);
    varint(frames.len(), &mut bytes);
    for frame in frames {
        varint(frame.len(), &mut bytes);
        bytes.extend_from_slice(frame);
    }
    bytes
}

#[test]
fn package_bad_compressed_entry() {
    let law = PackageLaw::new();
    law.fixture_json();
    law.app("app", &format!(
        "[dependencies]\njson = {{ package = \"fixture.test/json\", major = 1, root = \"{PACKAGE_X}\", archive = \"json.hpkg\" }}\n"
    ));
    let content = law.encoded(PACKAGE_X);
    let frame = package_frame(&content);
    let mut wrong_size = frame.clone();
    wrong_size[5] += 1;
    let mut too_much = frame.clone();
    too_much[5] -= 1;
    let mut bad_checksum = frame.clone();
    bad_checksum[4] |= 4;
    bad_checksum.extend_from_slice(&[0; 4]);
    let dictionary = [&frame[..4], &[0x21, 1], &frame[5..]].concat();
    for (compressed, rule) in [
        ([frame.as_slice(), frame.as_slice()].concat(), "not one Zstandard frame"),
        (vec![0x50, 0x2a, 0x4d, 0x18, 0, 0, 0, 0], "not one Zstandard frame"),
        (vec![0x28, 0xb5, 0x2f, 0xfd, 0, 0, 1, 0, 0], "no content size"),
        (vec![0x28, 0xb5, 0x2f, 0xfd, 0xa0, 1, 0, 0, 4], "content size 67108865 over 64 MiB"),
        (vec![0x28, 0xb5, 0x2f, 0xfd, 0xa0, 1, 0, 128, 0], "window 8388609 over 8 MiB"),
        (dictionary, "needs a dictionary"),
        (bad_checksum, "checksum mismatch"),
        (wrong_size, "inflates to 88 bytes, not 89"),
        (too_much, "inflates to 88 bytes, not 87"),
    ] {
        law.write("app/json.hpkg", package_frames(&[&compressed]));
        PackageLaw::refuse(law.run(&["fetch", "app"]), &format!(
            "fixture.test/json@1: archive:json.hpkg is not a package archive: bad compressed entry 0: {rule}"
        ));
        law.empty_store();
        assert!(!law.path("app/hirsel.lock").exists());
    }
    law.write("app/json.hpkg", package_frames(&[&frame, b"wrong magic"]));
    PackageLaw::refuse(
        law.run(&["fetch", "app"]),
        "fixture.test/json@1: archive:json.hpkg is not a package archive: bad compressed entry 1: not one Zstandard frame",
    );
    law.empty_store();

    // The identical bytes in an unsharded path cannot satisfy the pin.
    law.write(&format!("packages/{}", &PACKAGE_X[7..]), &content);
    law.write("app/json.hpkg", package_frames(&[&frame]));
    PackageLaw::pass(law.run(&["fetch", "app"]));
    let stored = law.path(&package_entry("packages", PACKAGE_X));
    assert_eq!(std::fs::read(&stored).unwrap(), content);
    let identity = crate::package_files::FileId::of(&stored.metadata().unwrap());
    PackageLaw::pass(law.run(&["fetch", "app"]));
    assert!(crate::package_files::FileId::of(&stored.metadata().unwrap()) == identity);
}

#[test]
fn package_l42_frozen_makes_no_connection() {
    match crate::host::HOST_TARGET.os() {
        hirsel_core_layout::Os::Linux => {}
        hirsel_core_layout::Os::Macos | hirsel_core_layout::Os::Ios => return,
    }
    const IN_NAMESPACE: &str = "HIRSEL_PACKAGES_L42_NAMESPACE";
    if std::env::var_os(IN_NAMESPACE).is_none() {
        let trace_dir = tempfile::tempdir().unwrap();
        let trace = trace_dir.path().join("network");
        let result = std::process::Command::new("unshare")
            .args(["--user", "--net", "--", "strace", "-f", "-qq", "-e", "trace=network", "-o"])
            .arg(&trace)
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "native::tests::package_l42_frozen_makes_no_connection", "--nocapture"])
            .env(IN_NAMESPACE, "1")
            .env("TMPDIR", trace_dir.path())
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        let trace = std::fs::read_to_string(trace).unwrap();
        let phase = |name: &str| {
            let marker = format!("sun_path=\"l42/{name}\"");
            let start = trace.find(&marker).unwrap();
            trace[start..].split("\n").skip(1)
                .take_while(|line| !line.contains("sun_path=\"l42/"))
                .collect::<Vec<_>>()
        };
        for name in ["frozen", "resident-frozen"] {
            let network: Vec<_> = phase(name).into_iter()
                .filter(|line| line.contains("AF_INET") || line.contains("nscd/socket")
                    || line.contains("io.systemd.Resolve"))
                .collect();
            assert!(network.is_empty(), "{name}: {network:?}");
        }
        for name in ["ordinary", "resident-ordinary"] {
            assert!(phase(name).iter().any(|line| line.contains("connect(") && line.contains("AF_INET")),
                "{name}: {}", phase(name).join("\n"));
        }
        return;
    }

    let law = PackageLaw::new();
    law.fixture_json();
    law.app("app", &format!(
        "[dependencies]\njson = {{ package = \"fixture.test/json\", major = 1, root = \"{PACKAGE_X}\", archive = \"json.hpkg\" }}\n"
    ));
    law.write("app/json.hpkg", std::fs::read(law.path("vendor/jx.hpkg")).unwrap());
    PackageLaw::pass(law.run(&["fetch", "app"]));
    std::fs::remove_file(law.path("app/json.hpkg")).unwrap();
    for (name, url) in [("config", "cache.invalid"), ("warm-config", "warm.invalid")] {
        law.write(&format!("{name}/hirsel/config.toml"), format!(
            "[cache.remote]\nurl = \"https://{url}\"\ntrusted = [\"{}\"]\n[cache.remote.transfer]\nrecord-bytes = 65536\nblob-bytes = 65536\nwork-bytes = 65536\n",
            "01".repeat(32)
        ));
    }
    let runtime = tempfile::Builder::new().prefix("l42-").tempdir_in("/tmp").unwrap();
    let shared = |command: &mut std::process::Command| {
        command.env("XDG_RUNTIME_DIR", runtime.path());
    };
    let build = |name: &str, frozen: bool, config: &str, resident: bool| {
        // The Unix-domain call delimitates trace phases. IP sockets are forbidden
        // under --frozen, but Unix-domain compiler IPC is allowed.
        let _ = std::os::unix::net::UnixStream::connect(format!("l42/{name}"));
        let mut args = vec!["build", "app", "-o", name];
        if frozen {
            args.push("--frozen");
        }
        let mut command = law.command(&args, "packages");
        shared(&mut command);
        command.env("XDG_CACHE_HOME", law.path(&format!("cache-{name}")))
            .env("XDG_CONFIG_HOME", law.path(config))
            .env("HIRSEL_TRACE_REQUEST_PHASES", "1")
            // A numeric proxy makes the attempt observable without working DNS.
            .env_remove("https_proxy").env_remove("all_proxy").env_remove("ALL_PROXY")
            .env_remove("NO_PROXY").env_remove("no_proxy")
            .env("HTTPS_PROXY", "http://192.0.2.1:9");
        let answer = PackageLaw::output(command);
        assert_eq!(answer.2.contains("hirsel-request-phases"), resident, "{}", answer.2);
        PackageLaw::pass(answer);
    };
    build("frozen", true, "config", false);
    build("ordinary", false, "config", false);
    let server = start_resident(runtime.path(), law.dir.path(), shared);
    let served = std::panic::catch_unwind(|| {
        build("resident-ordinary", false, "warm-config", true);
        build("resident-frozen", true, "config", true);
    });
    stop_resident(runtime.path(), law.dir.path(), server);
    if let Err(panic) = served {
        std::panic::resume_unwind(panic);
    }
}

/// L41. Make a published package, use it with sufficient ceilings, then narrow
/// the row while keeping its archive, dependency image, and request records.
#[test]
fn package_l41_authority_ceiling() {
    let law = PackageLaw::new();
    let source = r#"
pub fn ping(n: Net, addr: Addr) -> Result[Socket, NetError] waits { host.connect(n, addr, Never) }
pub fn open(d: Dir) -> Result[HostFile, IoError] waits { std.file.open(d, "x", Read) }
pub fn fake() -> (Clock, ClockHand) { provide.clock(0, 0) }
pub fn spawn(e: Exec, l: Launch) -> Result[Spawned, ExecError] waits { exec.spawn(e, l) }
pub fn run[T](take x: T) {}
pub fn twice(f: fn(Stdout) -> u64 waits, out: Stdout) -> u64 waits { f(out) + f(out) }
"#;
    law.library("lib", "fixture.test/effects", "1.0.0", source, "[dependencies]\nstd = { major = 1 }\n");
    let packed = law.fixture("lib", "lib.hpkg");
    let root = packed.split_whitespace().last().unwrap();
    law.write("w/hirsel.toml", "[workspace]\nmembers = [\"app\", \"middle\", \"consumer\"]\n");
    law.write("w/lib.hpkg", std::fs::read(law.path("lib.hpkg")).unwrap());
    let row = |list: &str| format!("effects = {{ package = \"fixture.test/effects\", major = 1, root = \"{root}\", archive = \"../lib.hpkg\"{list} }}\n");
    law.library("generic", "fixture.test/gen", "1.0.0",
        "pub fn stamp[T](x: T, c: Clock) -> u64 { host.wall_ms(c) }", "");
    let generic = law.fixture("generic", "w/gen.hpkg");
    let generic_root = generic.split_whitespace().last().unwrap();
    let wide = ", authority = [\"Dir\", \"Net\", \"Clock\", \"Exec\"]";
    law.library("w/middle", "fixture.test/middle", "1.0.0", "pub fn idle() {}\n", &format!("[dependencies]\n{}", row(wide)));
    let consumer = |mask: &str| law.library("w/consumer", "fixture.test/consumer", "1.0.0", PACKAGE_MAIN, &format!(
        "[dependencies]\napp = {{ package = \"fixture.test/app\", major = 1, path = \"../app\", authority = [{mask}] }}\n"));
    let kinds = "\"Dir\", \"Net\", \"Clock\", \"Exec\"";
    let all = format!("{kinds}, \"Stdout\"");
    consumer(&all);
    law.write("w/app/helpers.hirsel", "pub fn reads(e: Env) -> u64 { env.len(e) }");
    let app = |list: &str| {
        let clock = if list.is_empty() { "" } else { ", authority = [\"Clock\"]" };
        law.library("w/app", "fixture.test/app", "1.0.0", r#"
fn writer(o: Stdout) -> u64 waits { let _ = stdout.write(o, "x")
    1u64 }
resource type Probe { out: Stdout } drop(p) { let _ = stdout.is_terminal(p.out) }
pub fn consume(out: Stdout) { effects.run(Probe(out: out)) }
pub fn use_lib(c: Clock, out: Stdout) -> u64 waits { gen.stamp(1u64, c) + effects.twice(fn(o: Stdout) -> u64 waits { writer(o) }, out) }
pub fn main() -> Result[(), Exit] { Ok(()) }
"#, &format!("[program]\nmodule = \"app\"\nfunction = \"main\"\nlimit = 67108864\n[tests]\nlimit = 67108864\n[tests.sources]\n\"helpers.hirsel\" = \"helpers\"\n[dependencies]\ngen = {{ package = \"fixture.test/gen\", major = 1, root = \"{generic_root}\", archive = \"../gen.hpkg\"{clock} }}\nmiddle = {{ package = \"fixture.test/middle\", major = 1, path = \"../middle\"{wide} }}\n{}", row(list)));
    };
    app(wide);
    PackageLaw::pass(law.run(&["check", "w/app"]));
    PackageLaw::pass(law.run(&["build", "w/app", "--output", "wide"]));
    // The caller supplies both a closure and the type's drop implementation.
    // Its own ceiling accounts for Stdout even though both dependency ceilings pass.
    consumer(kinds);
    let (status, _, error) = law.run(&["check", "w/consumer"]);
    assert_eq!(status, Some(2), "{error}");
    assert!(error.contains("dependencies.app: fixture.test/app@1 reaches Stdout"), "{error}");
    consumer(&all);
    app("");
    for command in [&["check", "w/app"][..], &["build", "w/app", "--output", "narrow"][..], &["pack", "w/app", "--output", "forbidden.hpkg"][..]] {
        let (status, _, stderr) = law.run(command);
        assert_eq!(status, Some(2), "{stderr}");
        for want in ["app/hirsel.toml: dependencies.effects: fixture.test/effects@1 reaches Dir, Net, Clock, Exec",
            "/effects/open -> std@1/file/open -> Open", "/effects/ping -> Connect", "/effects/fake -> ProvideClock", "ExecSpawn",
            "app/hirsel.toml: dependencies.gen: fixture.test/gen@1 reaches Clock", "/gen/stamp -> WallMs"] {
            assert!(stderr.contains(want), "missing {want} in {stderr}");
        }
    }
    assert!(!law.path("forbidden.hpkg").exists());
    assert!(!law.path("narrow").exists());

    for (name, contained) in [("native", false), ("contained", true)] {
        let foreign = if contained {
            law.write("contained/empty.wasm", b"\0asm\x01\0\0\0");
            "containment = \"wasm\"\nmodule = { file = \"empty.wasm\", sha256 = \"93a44bbb96c751218e4c00d479e4c14358122a389acca16205b1e4d0dc5f9476\" }"
        } else {
            "archives = []"
        };
        law.manifest(name, &format!("fixture.test/{name}"), "1.0.0", &format!(
            "[sources]\n\"api.hirsel\" = \"api\"\n\"bridge.hirsel\" = \"bridge\"\n[foreign.bridge]\nsymbols = \"aa_\"\n{foreign}\n"));
        law.write(&format!("{name}/api.hirsel"), "pub opaque type Api {}\npub fn ping(f: Foreign[Api]) -> u64 { bridge.ping(f) }");
        law.write(&format!("{name}/bridge.hirsel"), "pub fn ping(f: Foreign[api.Api]) -> u64");
        std::fs::create_dir_all(law.path("ffi")).unwrap();
        let pack = law.fixture(name, &format!("ffi/{name}.hpkg"));
        let root = pack.split_whitespace().last().unwrap();
        law.app("ffi", &format!("[dependencies]\nlib = {{ package = \"fixture.test/{name}\", major = 1, root = \"{root}\", archive = \"{name}.hpkg\" }}\n"));
        let result = law.run(&["check", "ffi"]);
        if contained {
            PackageLaw::pass(result);
        } else {
            assert_eq!(result.0, Some(2), "{}", result.2);
            assert!(result.2.contains("fixture.test/native@1 reaches Foreign"), "{}", result.2);
        }
    }
}
