/// A C driver that appends `extra` to every command, as a wrapper does.
fn appending_driver(dir: &Path, name: &str, extra: &str) -> PathBuf {
    let quote = |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    let original = native_options(Backend::Cranelift, dir).link;
    let cc = dir.join(name);
    let script = format!(
        "#!/bin/sh\nexec {} \"$@\" {extra}\n",
        quote(original.driver())
    );
    crate::executable::write(&cc, script.as_bytes(), 0o755).unwrap();
    cc
}

/// Add an observer to a test link. Mach-O has no GNU --wrap, so give the
/// implementations in the private runtime archive the names its observer calls.
fn with_link_wrappers(
    dir: &Path,
    name: &str,
    runtime: &Path,
    observer: &Path,
    symbols: &[&str],
) -> (PathBuf, PathBuf) {
    let quote = |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    let (runtime, args) = match crate::host::HOST_TARGET.object_format() {
        hirsel_core_layout::ObjectFormat::Elf => {
            let flags = symbols.iter().map(|symbol| format!(" -Wl,--wrap={symbol}")).collect::<String>();
            (runtime.to_owned(), format!("{}{flags}", quote(observer)))
        }
        hirsel_core_layout::ObjectFormat::MachO => {
            let cc = native_options(Backend::Cranelift, dir).link;
            let objcopy = cc.driver().with_file_name("llvm-objcopy");
            let rename = |input: &Path, output: &Path, from: &str, to: &str| {
                let mut args = Command::new(&objcopy);
                for symbol in symbols {
                    args.arg(format!("--redefine-sym={from}{symbol}={to}{symbol}"));
                }
                command(args.arg(input).arg(output)).unwrap();
            };
            let wrapped = dir.join("wrapped-runtime.a");
            let aliases = dir.join("wrapped-observer.o");
            rename(runtime, &wrapped, "_", "___real_");
            rename(observer, &aliases, "___wrap_", "_");
            (wrapped, quote(&aliases))
        }
    };
    (runtime, appending_driver(dir, name, &args))
}

/// An object defining `symbol` as `value`.
fn marker_object(dir: &Path, path: &Path, symbol: &str, value: u32) {
    let original = native_options(Backend::Cranelift, dir).link;
    let c = dir.join("marker.c");
    std::fs::write(&c, format!("int {symbol} = {value};\n")).unwrap();
    command(
        Command::new(original.driver())
            .arg("-c")
            .arg(&c)
            .arg("-o")
            .arg(path),
    )
    .unwrap();
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
    let format = crate::host::HOST_TARGET.object_format();
    use hirsel_core_layout::ObjectFormat::{Elf, MachO};
    let data = dir.path().join("link-data");
    let script = dir.path().join("extra.ld");
    let list = dir.path().join("extra.list");
    let base = second.join("fixed.o");
    marker_object(dir.path(), &base, "hirsel_link_fixed", 0);
    marker_object(
        dir.path(),
        &second.join("marker.o"),
        "hirsel_link_marker",
        1,
    );
    let path_option = match format {
        Elf => {
            std::fs::write(&data, "V1 { global: *; };\n").unwrap();
            std::fs::write(&script, "INPUT(-l:marker.o)\n").unwrap();
            format!(
                "-Wl,--export-dynamic,--version-script={},{}",
                data.display(),
                script.display()
            )
        }
        MachO => {
            std::fs::write(&data, b"before").unwrap();
            std::fs::write(&list, format!("{}\n", base.display())).unwrap();
            format!(
                "-Wl,-sectcreate,__TEXT,__hirsel_law,{},-filelist,{},-lmarker.o",
                data.display(),
                list.display()
            )
        }
    };
    let cc = appending_driver(
        dir.path(),
        "cc",
        &format!(
            "-Wl,-L{},-L{} {path_option}",
            first.display(),
            second.display()
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
    let edited_data = match format {
        Elf => "V2 { global: main; local: *; };\n",
        MachO => "after",
    };
    std::fs::write(&data, edited_data).unwrap();
    same("file named by an option");
    marker_object(
        dir.path(),
        &second.join("marker.o"),
        "hirsel_link_marker",
        2,
    );
    same("library");
    marker_object(dir.path(), &first.join("marker.o"), "hirsel_link_marker", 3);
    same("earlier search directory");
    let other = second.join("other.o");
    marker_object(dir.path(), &other, "hirsel_link_other", 4);
    match format {
        Elf => std::fs::write(&script, "INPUT(-l:marker.o)\nINPUT(-l:other.o)\n").unwrap(),
        MachO => {
            std::fs::write(&list, format!("{}\n{}\n", base.display(), other.display())).unwrap()
        }
    }
    same("file naming other link inputs");

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

fn fixture_abi_section() -> &'static str {
    match crate::host::HOST_TARGET.object_format() {
        hirsel_core_layout::ObjectFormat::Elf => ".hirsel_abi",
        hirsel_core_layout::ObjectFormat::MachO => "__TEXT,__hirsel_abi",
    }
}
