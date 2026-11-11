//! A repeated Precompute reuses its installation only when the root holds
//! exactly what a fresh Precompute of the current inputs would publish, under
//! V's own registration of those products. Any changed input or damaged state
//! rebuilds; a hit writes nothing.
use hirsel_driver::CompilerRequestsExt;
use hirsel_front_phases::COMPILER;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CHILD: &str = "HIRSEL_PRECOMPUTE_REUSE_737";
const SOURCE: &str = r#"fn make()->Result[Body[fn()->u8 uses {}],String]{Ok(codegen.body("fn()->u8 uses {} {7u8}"))}
pub fn main()->u8 uses {} = comptime make()
pub fn second()->u64{9u64}
"#;
const MANIFEST: &str = r#"{"schema":1,"package":{"name":"reuse_737","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#;

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                out.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

fn installation(root: &Path) -> Option<hirsel_artifact_store::Association> {
    hirsel_artifact_store::Store::new(root.to_owned()).lookup_owned(hirsel_artifact_store::Address(
        hirsel_syntax::ContentHash::of_bytes(b"hirsel.installed-package.v4"),
    ))
}

fn flip(path: &Path) -> Vec<u8> {
    let original = std::fs::read(path).unwrap();
    let mut damaged = original.clone();
    let middle = damaged.len() / 2;
    damaged[middle] ^= 0x40;
    std::fs::write(path, damaged).unwrap();
    original
}

struct Fixture {
    directory: tempfile::TempDir,
}
impl Fixture {
    fn manifest(&self) -> PathBuf {
        self.directory.path().join("pkg/hirsel.json")
    }
    fn state(&self) -> PathBuf {
        self.directory.path().join("state")
    }
    fn root(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }
    /// One Precompute in a fresh process; returns how many V runs it made.
    fn precompute(&self, mode: &str, root: &Path) -> u64 {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "precompute_reuse_737::precompute_reuses_only_an_identical_registered_installation",
                "--nocapture",
            ])
            .env(CHILD, mode)
            .env("HIRSEL_REUSE_737_MANIFEST", self.manifest())
            .env("HIRSEL_REUSE_737_ROOT", root)
            .env("XDG_STATE_HOME", self.state())
            .env("XDG_CACHE_HOME", self.directory.path().join("cache"))
            .env("HIRSEL_RESULTS", "none@1")
            .env_remove("HIRSEL_VERIFY_CACHE")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{mode}: {output:?}");
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("V runs: "))
            .unwrap_or_else(|| panic!("{mode}: {stdout}"))
            .parse()
            .unwrap()
    }
}

fn child(mode: &str) {
    use hirsel_intern::timings::{self, Count};
    let manifest = PathBuf::from(std::env::var_os("HIRSEL_REUSE_737_MANIFEST").unwrap());
    let root = PathBuf::from(std::env::var_os("HIRSEL_REUSE_737_ROOT").unwrap());
    let compiler = match mode {
        "identity" => {
            hirsel_engine::Compiler::new(hirsel_front_phases::FrontPhases::shared, [7; 32])
        }
        "limits" => COMPILER.with_comptime_limits(hirsel_engine::ComptimeLimits {
            budget: hirsel_engine::DEFAULT_COMPTIME_BUDGET - 1,
            ..Default::default()
        }),
        _ => COMPILER,
    };
    timings::enable();
    hirsel_intern::jobs::with_compiler_stack(|| {
        compiler.precompute_package(&manifest, &root).unwrap()
    });
    let runs = timings::report()
        .counts
        .into_iter()
        .find(|(count, _)| *count == Count::VRuns)
        .unwrap()
        .1;
    println!("V runs: {runs}");
}

#[test]
fn precompute_reuses_only_an_identical_registered_installation() {
    if let Some(mode) = std::env::var_os(CHILD) {
        return child(mode.to_str().unwrap());
    }
    let f = Fixture {
        directory: tempfile::tempdir().unwrap(),
    };
    std::fs::create_dir(f.directory.path().join("pkg")).unwrap();
    let source = f.directory.path().join("pkg/main.hir");
    std::fs::write(&source, SOURCE).unwrap();
    std::fs::write(f.manifest(), MANIFEST).unwrap();
    let a = f.root("a");
    let miss = |mode: &str, what: &str| {
        assert!(f.precompute(mode, &a) > 0, "{what} must rebuild and run V");
    };
    let hit = |what: &str| {
        let (products, state) = (files(&a), files(&f.state()));
        assert_eq!(f.precompute("default", &a), 0, "{what}: rerun must reuse");
        assert!(
            files(&a) == products,
            "{what}: a reuse must not write the root"
        );
        assert!(
            files(&f.state()) == state,
            "{what}: a reuse must not write V state"
        );
    };
    miss("default", "cold");
    hit("unchanged inputs");

    // Damage located by the store's own audit (independent of the reuse
    // check): the header first, then a record. Each is located in the root as it stands before that damage.
    let classify = |needle: &str| {
        files(&a.join("packs")).into_keys().find(|pack| {
            let original = flip(pack);
            let error = hirsel_engine_store::InstalledPackage::verify_store(&a).err();
            std::fs::write(pack, original).unwrap();
            error.is_some_and(|error| error.contains(needle))
        })
    };
    for (needle, what) in [
        ("corrupt installed package", "damaged header"),
        ("partial or corrupt package product", "damaged record"),
    ] {
        flip(&classify(needle).unwrap_or_else(|| panic!("a pack holding the {what}")));
        miss("default", what);
        hit(&format!("after rebuilding a {what}"));
    }

    // V's registration is the authority: missing or unloadable, the root rebuilds.
    let registrations: Vec<_> = files(&f.state())
        .into_keys()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "registration")
        })
        .collect();
    assert_eq!(registrations.len(), 1);
    flip(&registrations[0]);
    miss("default", "unloadable registration");
    hit("after re-registering");
    for registration in files(&f.state()).into_keys() {
        if registration
            .extension()
            .is_some_and(|extension| extension == "registration")
        {
            std::fs::remove_file(registration).unwrap();
        }
    }
    miss("default", "missing registration");
    hit("after registering");

    miss("identity", "another compiler identity");
    miss("default", "the original compiler after another identity");
    miss("limits", "other comptime limits");
    miss("default", "the default comptime limits after others");
    hit("restored settings");

    std::fs::write(f.manifest(), format!("{MANIFEST}\n")).unwrap();
    miss("default", "a changed manifest");
    hit("the changed manifest");

    // The edit run equals a cold Precompute of the edited tree.
    std::fs::write(&source, SOURCE.replace("9u64", "8u64")).unwrap();
    let before = installation(&a).unwrap();
    miss("default", "one changed source byte");
    let b = f.root("b");
    miss_into(&f, &b);
    let (edited, cold) = (installation(&a).unwrap(), installation(&b).unwrap());
    assert_ne!(edited, before);
    assert_eq!(edited, cold, "the edit run publishes the cold installation");
    hit("the edited source");
}

fn miss_into(f: &Fixture, root: &Path) {
    assert!(f.precompute("default", root) > 0, "cold must run V");
}
