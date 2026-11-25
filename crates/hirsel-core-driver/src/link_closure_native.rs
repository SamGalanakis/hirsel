/// A C driver that appends `extra` to every command, as a wrapper does.
fn appending_driver(dir: &Path, name: &str, extra: &str) -> PathBuf {
    let quote = |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    let original = native_options(Backend::Cranelift, dir).link;
    let cc = dir.join(name);
    let script = format!("#!/bin/sh\nexec {} \"$@\" {extra}\n", quote(original.driver()));
    crate::executable::write(&cc, script.as_bytes(), 0o755).unwrap();
    cc
}

/// An object defining `symbol` as `value`.
fn marker_object(dir: &Path, path: &Path, symbol: &str, value: u32) {
    let original = native_options(Backend::Cranelift, dir).link;
    let c = dir.join("marker.c");
    std::fs::write(&c, format!("int {symbol} = {value};\n")).unwrap();
    command(Command::new(original.driver()).arg("-c").arg(&c).arg("-o").arg(path)).unwrap();
}

/// After each change to a file the link reads, a build against the primed
/// store links what a build against an empty store links (review 1043 F1).
/// Each step names the route the changed bytes reach ld.lld by.
#[test]
#[ignore = "explicit native integration target"]
fn a_file_the_link_reads_keys_the_executable() {
    let dir = tempfile::tempdir().unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let source = "pub fn main() -> Result[(), Exit] { Error(Exit(status: 7)) }\n";
    let product = Driver::default()
        .check(source_request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let version = dir.path().join("version.map");
    let script = dir.path().join("extra.ld");
    std::fs::write(&version, "V1 { global: *; };\n").unwrap();
    std::fs::write(&script, "INPUT(-l:marker.o)\n").unwrap();
    marker_object(dir.path(), &second.join("marker.o"), "hirsel_link_marker", 1);
    let cc = appending_driver(
        dir.path(),
        "cc",
        &format!(
            "-Wl,--export-dynamic,--version-script={},-L{},-L{},{}",
            version.display(),
            first.display(),
            second.display(),
            script.display()
        ),
    );
    let mut options = native_options(Backend::Cranelift, dir.path());
    options.link = Link::Driver(cc);
    let primed = product.build(&entry, &options).unwrap();
    run_clean(&primed.executable, 7);
    let mut fresh = 0;
    let mut same = |route: &str| {
        let primed = product.build(&entry, &options).unwrap();
        let primed = std::fs::read(&primed.executable).unwrap();
        fresh += 1;
        let mut empty = native_options(Backend::Cranelift, dir.path());
        empty.link = options.link.clone();
        empty.cas = dir.path().join(format!("fresh-{fresh}"));
        empty.output = dir.path().join(format!("fresh-program-{fresh}"));
        let built = product.build(&entry, &empty).unwrap();
        run_clean(&built.executable, 7);
        assert!(
            primed == std::fs::read(&built.executable).unwrap(),
            "{route}: the primed store linked another executable than an empty one"
        );
    };
    // A flag's attached operand.
    std::fs::write(&version, "V2 { global: main; local: *; };\n").unwrap();
    same("version script");
    // A positional linker script's library, found in the second directory.
    marker_object(dir.path(), &second.join("marker.o"), "hirsel_link_marker", 2);
    same("library a script names");
    // A file appearing where the search tried before its match.
    marker_object(dir.path(), &first.join("marker.o"), "hirsel_link_marker", 3);
    same("earlier search directory");
    // The script itself.
    std::fs::write(&script, "INPUT(-l:marker.o)\nINPUT(-l:other.o)\n").unwrap();
    marker_object(dir.path(), &second.join("other.o"), "hirsel_link_other", 4);
    same("linker script");

    // A response file's words are not argv the table can parse: refused.
    let words = dir.path().join("words.rsp");
    std::fs::write(&words, "--export-dynamic\n").unwrap();
    options.link = Link::Driver(appending_driver(
        dir.path(),
        "rsp-cc",
        &format!("-Wl,@{}", words.display()),
    ));
    let refused = match product.build(&entry, &options) {
        Ok(_) => panic!("a link through a response file was not refused"),
        Err(error) => error.to_string(),
    };
    assert!(refused.contains("response file"), "{refused}");
}

/// Two byte-equal C drivers at two places are one toolchain: the second link
/// is reused, and its executable is the first's (review 1043 F2).
#[test]
#[ignore = "explicit native integration target"]
fn a_driver_copy_is_the_same_toolchain() {
    let dir = tempfile::tempdir().unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let source = "pub fn main() -> Result[(), Exit] { Error(Exit(status: 7)) }\n";
    let product = Driver::default()
        .check(source_request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let mut options = native_options(Backend::Cranelift, dir.path());
    let mut built = |place: &str| {
        let place = dir.path().join(place).join("bin");
        std::fs::create_dir_all(&place).unwrap();
        options.link = Link::Driver(appending_driver(&place, "clang", ""));
        let built = product.build(&entry, &options).unwrap();
        run_clean(&built.executable, 7);
        (built.linked, built.identity, std::fs::read(&built.executable).unwrap())
    };
    let (_, first_image, first) = built("driver-one");
    let (linked, image, second) = built("driver-two");
    assert_eq!(image, first_image, "a driver copy changed the executable key");
    assert!(!linked, "a driver copy relinked");
    assert!(first == second, "a driver copy linked other bytes");
}
