use super::{Fixture, passed};
use hirsel_core_layout::{AppleRecord, Target};
use object::LittleEndian as LE;
use object::macho::{CPU_TYPE_ARM64, LC_VERSION_MIN_IPHONEOS, MH_OBJECT, MachHeader64};
use object::read::archive::ArchiveFile;
use object::read::macho::{LoadCommandVariant, MachHeader};
use object::{Object, ObjectSymbol};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Command};

const SOURCE: &str = include_str!("../archive_ios.hirsel");
const FOREIGN: &str = r#"
int hfl_touch(void) { return 7; }
__attribute__((used, section("__TEXT,__hirsel_abi")))
static const char abi[sizeof("hfl_touch stack () -> i32\n") - 1] = "hfl_touch stack () -> i32\n";
"#;

fn minimum(bytes: &[u8], apple: &AppleRecord, member: &str, prebuilt: bool) -> u32 {
    let header = MachHeader64::<LE>::parse(bytes, 0).unwrap();
    assert_eq!(header.cputype(LE), CPU_TYPE_ARM64, "{member}");
    assert_eq!(header.filetype(LE), MH_OBJECT, "{member}");
    let mut commands = header.load_commands(LE, bytes, 0).unwrap();
    let mut minos = None;
    while let Some(command) = commands.next().unwrap() {
        let version = match command.variant().unwrap() {
            LoadCommandVariant::BuildVersion(version, _) => {
                assert_eq!(version.platform.get(LE).0, apple.macho_platform, "{member}");
                version.minos.get(LE).0
            }
            LoadCommandVariant::VersionMin(version)
                if prebuilt
                    && command.cmd() == LC_VERSION_MIN_IPHONEOS
                    && apple.macho_platform == object::macho::PLATFORM_IOS.0 =>
            {
                version.version.get(LE).0
            }
            _ => continue,
        };
        assert!(minos.replace(version).is_none(), "{member}");
    }
    minos.unwrap_or_else(|| panic!("{member} has no LC_BUILD_VERSION"))
}

fn check_archive(archive: &Path, runtime: &Path, apple: &AppleRecord) {
    let bytes = fs::read(archive).unwrap();
    let runtime = fs::read(runtime).unwrap();
    let members = ArchiveFile::parse(&*bytes)
        .unwrap()
        .members()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let runtime_count = ArchiveFile::parse(&*runtime).unwrap().members().count();
    let program_end = members.len() - runtime_count;
    assert!(program_end > 2);
    let mut foreign = 0;
    let mut verifier = false;
    for (index, member) in members.into_iter().enumerate() {
        let label = std::str::from_utf8(member.name()).unwrap();
        let bytes = member.data(&*bytes).unwrap();
        let minos = minimum(bytes, apple, label, index >= program_end);
        let object = object::File::parse(bytes).unwrap();
        let is_foreign = object.symbols().any(|symbol| {
            symbol.is_definition() && symbol.name().is_ok_and(|name| name.ends_with("_hfl_touch"))
        });
        verifier |= object.symbols().any(|symbol| {
            symbol.is_undefined()
                && symbol
                    .name()
                    .is_ok_and(|name| name == "_SecTrustEvaluateWithError")
        });
        if is_foreign {
            foreign += 1;
            assert!(index < program_end, "{label}: missing foreign input");
            assert_eq!(minos, 17 << 16, "{label}");
        } else if index < program_end {
            assert_eq!(minos, apple.macho_minos, "{label}");
        } else {
            assert!(minos <= apple.macho_minos, "{label}");
        }
    }
    assert_eq!(foreign, 1);
    assert!(verifier, "the archive has no System verifier import");
}

#[test]
#[ignore = "uses the pinned iOS runtimes and link kit"]
fn ios_system_archives_carry_the_target_platform_and_floor() {
    let work = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(work.path());
    let runtime_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(std::env::var_os("HIRSEL_IOS_RUNTIMES").unwrap())
        .canonicalize()
        .unwrap();
    let prefix = work.path().join("prefix");
    let compiler = prefix.join("hirsel");
    for (target, foreign_triple) in [
        (Target::Aarch64Ios, "arm64-apple-ios17.0"),
        (Target::Aarch64IosSim, "arm64-apple-ios17.0-simulator"),
    ] {
        let apple = target.apple().unwrap();
        let runtime = runtime_dir
            .join(target.triple())
            .join("libhirsel_core_rt.a");
        passed(
            fixture
                .hirsel()
                .args(["core-product", "--install"])
                .arg(&prefix)
                .args(["--target", target.triple(), "--link-kit"])
                .arg(&fixture.kit)
                .arg("--embed-runtime")
                .arg(&runtime),
        );
        let source = fixture.program.join("laws.c");
        let object = fixture.program.join("laws.o");
        let foreign = fixture.program.join("laws.a");
        fs::write(&source, FOREIGN).unwrap();
        passed(
            Command::new(fixture.llvm.join("bin/clang"))
                .arg(format!("--target={foreign_triple}"))
                .args(["-O2", "-c"])
                .arg(source)
                .arg("-o")
                .arg(&object),
        );
        passed(
            Command::new(fixture.llvm.join("bin/llvm-ar"))
                .args(["--format=darwin", "rcsD"])
                .arg(&foreign)
                .arg(object),
        );
        let pin = hirsel_core_ids::hex::encode(&Sha256::digest(fs::read(foreign).unwrap()));
        fs::write(fixture.program.join("embed_place.hirsel"), SOURCE).unwrap();
        fs::write(
            fixture.program.join("laws.hirsel"),
            "pub opaque type Laws {}\n",
        )
        .unwrap();
        fs::write(
            fixture.program.join("native.hirsel"),
            "pub fn touch(f: Foreign[laws.Laws]) -> i32\n",
        )
        .unwrap();
        fs::write(
            fixture.program.join("hirsel.toml"),
            format!(
                r#"[package]
name = "local/embed_place"
version = "1.0.0"
charge = "charge-v1"

[dependencies]
std = {{ major = 1 }}
store = {{ package = "store", major = 1, path = "../store" }}
runner = {{ package = "runner", major = 1, path = "../runner" }}

[sources]
"embed_place.hirsel" = "main"
"laws.hirsel" = "laws"
"native.hirsel" = "native"

[foreign.native]
symbols = "hfl_"
archives = [{{ target = "{}", file = "laws.a", sha256 = "{pin}" }}]
"#,
                target.triple(),
            ),
        )
        .unwrap();
        let archive = work.path().join(format!("{}.a", target.triple()));
        passed(
            Command::new(&compiler)
                .current_dir(&fixture.work)
                .env("XDG_CACHE_HOME", &fixture.cache)
                .arg("build")
                .arg(&fixture.program)
                .args(["--emit", "staticlib", "--target", target.triple()])
                .arg("--libraries")
                .arg(&fixture.libraries)
                .args(["--grant", "Foreign=local/embed_place@1/laws/Laws"])
                .arg("--store")
                .arg(&fixture.store)
                .args(["--limit", "1073741824", "-o"])
                .arg(&archive),
        );
        check_archive(&archive, &runtime, apple);
    }
}
