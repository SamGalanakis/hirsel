/// The program of [`release_edit_is_the_cold_build`]: its exit is 24, and 30
/// once `build(6u64)` is `build(7u64)`.
const RELEASE_SUM: &str = "type N: shareable + send {\n    value: u64,\n    tail: Option[Arc[N]]\n}\nfn scale(x: u64) -> u64 { x }\nfn sum(node: Arc[N]) -> u64 {\n    scale(node.value) + match node.tail {\n        Some(t) => sum(t),\n        None => 0u64\n    }\n}\nfn build(last: u64) -> Option[Arc[N]] {\n    let mut root: Option[Arc[N]] = None\n    for i in 1u64 .. last {\n        let old = replace(mut root, None)\n        root = Some(arc.new(N(value: i, tail: take old)))\n    }\n    root\n}\npub fn main() -> Result[(), Exit] {\n    let a = match build(6u64) {\n        Some(r) => sum(r),\n        None => 0u64\n    }\n    let b = match build(3u64) {\n        Some(r) => sum(r),\n        None => 0u64\n    }\n    let mut s = string.new()\n    let _ = string.push_u64(mut s, a)\n    let _ = string.push_char(mut s, ' ')\n    let _ = string.push_u64(mut s, b)\n    let n = string.byte_len(s)\n    let mut acc = a + b\n    for i in 0u64 .. n {\n        let parsed: Result[i64, ParseError] = int.parse(string.slice(s, i, n))\n        acc = acc + match parsed {\n            Ok(v) => int.truncate(v),\n            Error(_) => 1u64\n        }\n    }\n    Error(exit_with(int.truncate(acc)))\n}\n";

/// [`RELEASE_SUM`] with two bodies, in partitions of their own, that call
/// each other and read the same constant: `ping(3)` is `pong(2) + 1` =
/// `ping(1) + 3` = `pong(0) + 4` = `2 * width("ab") + 4` = 8.
fn release_parts() -> String {
    RELEASE_SUM
        .replace("fn scale(", "cold fn scale(")
        .replace(
            "pub fn main()",
            "fn width(s: String) -> u64 { string.byte_len(s) }\nfn ping(n: u64) -> u64 {\n    if n == 0u64 { width(\"ab\") } else { pong(n - 1u64) + 1u64 }\n}\nfn pong(n: u64) -> u64 {\n    if n == 0u64 { width(\"ab\") * 2u64 } else { ping(n - 1u64) + 2u64 }\n}\npub fn main()",
        )
        .replace("let mut acc = a + b\n", "let mut acc = a + b + ping(3u64)\n")
}

/// A release unit lowered in partitions, each body nearly alone in its own
/// (a bound of one Core instruction), exits as the unit does whole, and an
/// edit's build, which reuses the partitions the edit leaves unchanged, is a
/// cold build of the edited source into an empty store: the same executable
/// bytes. The second edit makes `scale` fallible. The caller's unchanged Core
/// body must now call its status-returning symbol. A key of member bodies
/// alone reuses a call to the removed infallible symbol and fails to link.
/// The constant both partitions read is a content-named part of
/// each, which the link takes once, so its linkage does not reach the
/// executable. Exits hand-derived as in [`release_edit_is_the_cold_build`],
/// plus `ping(3)` ([`release_parts`]): 24 + 8 = 32, then 30 + 8 = 38.
/// With `scale(x) = x / x`, the sums are 6 and 2. Parsing the suffixes of
/// "6 2" adds 1 + 1 + 2, so the last exit is 6 + 2 + 4 + 8 = 20.
#[test]
#[ignore = "explicit native integration target"]
fn release_partitions_edit_is_the_cold_build() {
    let dir = tempfile::tempdir().unwrap();
    let core = reach_core(dir.path(), "core", 0);
    let program = release_parts();
    assert!(program.contains("ping(3u64)"), "the program calls ping");
    let edited = program.replace("build(6u64)", "build(7u64)");
    let fallible = edited.replace(
        "fn scale(x: u64) -> u64 { x }",
        "fn scale(x: u64) -> u64 { x / x }",
    );
    let mut options = native_options(Backend::Llvm, dir.path());
    options.optimize = true;
    let apart = |name: &str| NativeOptions {
        cas: dir.path().join(format!("{name}-cas")),
        output: dir.path().join(format!("{name}-program")),
        ..options.clone()
    };
    let (_, _, status) = reach_build(
        &record_check(&program, &core, dir.path(), "whole"),
        &apart("whole"),
    );
    assert_eq!(status, Some(32), "whole");
    super::release::RELEASE_PARTITION.set(Some(1));
    super::release::RELEASE_LOWERED.set(0);
    let (cold, _, cold_status) = reach_build(
        &record_check(&program, &core, dir.path(), "store"),
        &options,
    );
    let cold_lowered = super::release::RELEASE_LOWERED.replace(0);
    let (edit, edit_bytes, edit_status) =
        reach_build(&record_check(&edited, &core, dir.path(), "store"), &options);
    let edit_lowered = super::release::RELEASE_LOWERED.get();
    let (_, fresh_bytes, fresh_status) = reach_build(
        &record_check(&edited, &core, dir.path(), "fresh"),
        &apart("fresh"),
    );
    let (_, abi_bytes, abi_status) = reach_build(
        &record_check(&fallible, &core, dir.path(), "store"),
        &options,
    );
    let (_, abi_fresh, abi_fresh_status) = reach_build(
        &record_check(&fallible, &core, dir.path(), "abi-fresh"),
        &apart("abi-fresh"),
    );
    super::release::RELEASE_PARTITION.set(None);
    assert_eq!(
        (cold_status, edit_status, fresh_status),
        (Some(32), Some(38), Some(38))
    );
    assert!(
        edit.compiled >= 1 && edit.compiled < cold.compiled,
        "the edit regenerates only its partitions: {} of {} parts",
        edit.compiled,
        cold.compiled
    );
    assert!(
        edit_bytes == fresh_bytes,
        "the edited build is the cold build"
    );
    assert!(
        edit_lowered > 0 && edit_lowered < cold_lowered,
        "the edit lowers {edit_lowered} of {cold_lowered} partitions"
    );
    assert_eq!((abi_status, abi_fresh_status), (Some(20), Some(20)));
    assert!(
        abi_bytes == abi_fresh,
        "the edited ABI links like the cold build"
    );
}

/// The release build's edits in one store and parts cache are a cold
/// build of the same source: the same executable bytes. Each kind reuses
/// unchanged functions' cached post-O3 tails beside the edited ones: a body
/// edit, an edit to a helper its caller inlines, a signature change, and a
/// core (std-visible) body edit. With line tables on, the body edit also
/// shifts the unchanged recursive function and its callers down two lines.
/// Mutants caught: dropping the key's bitcode reuses a stale tail, and
/// skipping scope rebasing on restore loses debug rows. Rebuilding each
/// debug tail in a fresh context from its key also checks that the tail can
/// run without the rest of the module. Exits hand-derived
/// as in `release_part_alone_is_the_part_beside_other_misses`, with each
/// value scaled: a = scale(1 + .. + (LAST - 1)), b = scale(1 + 2), plus
/// the parsed suffixes of "{a} {b}" (1 each that does not parse), plus the
/// core's bump: 15 + 3 + 6 = 24; 21 + 3 + 6 = 30; x2: 42 + 6 + 9 = 57;
/// x3: 63 + 9 + 12 = 84; bump 1: 85.
#[test]
#[ignore = "explicit native integration target"]
fn release_edit_is_the_cold_build() {
    const P: &str = RELEASE_SUM;
    for debug_lines in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let [a, b] = [("core-a", 0), ("core-b", 1)].map(|(name, bump)| reach_core(dir.path(), name, bump));
        let body = P.replace("build(6u64)", "build(7u64)");
        let body = if debug_lines {
            body.replace("fn sum(", "\n\nfn sum(")
        } else {
            body
        };
        let inlined = body.replace("{ x }", "{ x * 2u64 }");
        let signature = inlined
            .replace("fn scale(x: u64) -> u64 { x * 2u64 }", "fn scale(x: u64, m: u64) -> u64 { x * m }")
            .replace("scale(node.value)", "scale(node.value, 3u64)");
        let steps = [
            ("cold", P, &a, 24),
            ("body", &body, &a, 30),
            ("inlined helper", &inlined, &a, 57),
            ("signature", &signature, &a, 84),
            ("std-visible", &signature, &b, 85),
        ];
        let mut options = native_options(Backend::Llvm, dir.path());
        options.optimize = true;
        options.debug_lines = debug_lines;
        for (n, (kind, app, core, exit)) in steps.into_iter().enumerate() {
            let check_tails = debug_lines && n == 0;
            if check_tails {
                release::TAIL_FIXTURES.with(|fixtures| *fixtures.borrow_mut() = Some(Vec::new()));
            }
            let (built, bytes, status) = reach_build(&record_check(app, core, dir.path(), "store"), &options);
            if check_tails {
                let fixtures = release::TAIL_FIXTURES.with(|fixtures| fixtures.take().unwrap());
                assert!(!fixtures.is_empty(), "debug release writes tail records");
                let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
                let tool = root.join(std::env::var_os("HIRSEL_TEST_TAIL_CONTEXT").expect(
                    "HIRSEL_TEST_TAIL_CONTEXT names the tail-context tool; //:core-driver-native-shard-1 supplies it",
                ));
                let llvm = hirsel_llvm_lazy::llvm_library().unwrap();
                let mut built_keys = 0;
                for fixture in fixtures {
                    let paths = [
                        ("tail-key", fixture.key),
                        ("tail-input", fixture.input),
                        ("tail-cached", fixture.cached),
                        ("tail-ir", fixture.ir.to_vec()),
                    ]
                        .map(|(name, data)| {
                            let path = dir.path().join(name);
                            std::fs::write(&path, data).unwrap();
                            path
                        });
                    let run = Command::new(&tool).args(paths)
                        .env("LD_LIBRARY_PATH", llvm.parent().unwrap())
                        .stderr(std::process::Stdio::inherit())
                        .output().unwrap();
                    assert!(
                        run.status.success(),
                        "the part's key is exact and the tail rebuilt using only the key equals the cached tail"
                    );
                    built_keys += usize::from(run.stdout == b"built key\n");
                }
                assert!(built_keys > 0, "some part read back has the key the build computed");
            }
            let (cold, cold_bytes, _) = reach_build(
                &record_check(app, core, dir.path(), &format!("fresh{n}")),
                &NativeOptions {
                    cas: dir.path().join(format!("fresh-cas{n}")),
                    output: dir.path().join(format!("fresh-program{n}")),
                    optimize: true,
                    debug_lines,
                    ..native_options(Backend::Llvm, dir.path())
                },
            );
            assert_eq!(status, Some(exit), "{kind} (debug lines {debug_lines}): exit");
            if n > 0 {
                assert!(built.compiled >= 1 && built.compiled < cold.compiled, "{kind} (debug lines {debug_lines}): {} of {} parts", built.compiled, cold.compiled);
            }
            assert!(bytes == cold_bytes, "{kind} (debug lines {debug_lines}): the edited build is the cold build");
        }
    }
}

/// Two generic describers of one generic function: each instance's
/// `code.describe(same)` has its own callable adapter (`$callable[instance:
/// <describer>]`), and both describe `same[u64]` under one descriptor symbol
/// (`Module::descriptor_symbol`). Lowered one body per partition, the
/// adapters sit in different partitions; each defined the shared descriptor
/// as its own and the link stopped on a duplicate `$static`. Exit 24 when
/// both values are the one described function and call it: hand-derived
/// from `same`, 3 == 3 and 3 != 4.
#[test]
#[ignore = "explicit native integration target"]
fn release_partitions_define_a_shared_descriptor_once() {
    const SHARED: &str = "fn same[T: key](a: T, b: T) -> bool { a == b }\nfn first[T: key]() -> StaticFn[fn(T, T) -> bool] { code.describe(same) }\nfn second[T: key]() -> StaticFn[fn(T, T) -> bool] { code.describe(same) }\npub fn main() -> Result[(), Exit] {\n    let a: StaticFn[fn(u64, u64) -> bool] = first()\n    let b: StaticFn[fn(u64, u64) -> bool] = second()\n    if a == b && (code.callable(a))(3, 3) && !(code.callable(b))(3, 4) {\n        Error(exit_with(24u8))\n    } else {\n        Error(exit_with(1u8))\n    }\n}\n";
    let dir = tempfile::tempdir().unwrap();
    let core = reach_core(dir.path(), "core", 0);
    let mut options = native_options(Backend::Llvm, dir.path());
    options.optimize = true;
    super::release::RELEASE_PARTITION.set(Some(1));
    let (_, _, status) = reach_build(&record_check(SHARED, &core, dir.path(), "store"), &options);
    super::release::RELEASE_PARTITION.set(None);
    assert_eq!(status, Some(24));
}
