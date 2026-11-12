use super::*;
use hirsel_core_ir::{FuncId, eval};

fn request(source: &str, dir: &Path) -> CheckRequest {
    let source_file = dir.join("main.hirsel");
    std::fs::write(&source_file, source).unwrap();
    CheckRequest {
        inputs: CorpusManifest {
            core: package("core"),
            packages: vec![PackageManifest {
                package: package("app"),
                sources: vec![hirsel_core_resolve::ModuleSource {
                    file: source_file,
                    modules: vec!["main".into()],
                    signatures: false,
                }],
                dependencies: vec![],
            }],
        },
        store: dir.join("store"),
    }
}
#[test]
fn publishes_a_checked_body_and_disk_syntax() {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default()
        .check(
            request("fn answer() -> u64 { 6 * 7 }\n", dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let d = product.definitions.values().next().unwrap();
    let value = eval::run(&d.ir, d.check.function.unwrap(), &[], 1_000_000).outcome;
    assert_eq!(value, eval::Outcome::Returned(vec![42]));
    let store = Store::new(&dir.path().join("store")).unwrap();
    let root: Root = bincode::deserialize(&store.get(product.root).unwrap().unwrap()).unwrap();
    let file: FileProduct =
        bincode::deserialize(&store.get(root.files[0]).unwrap().unwrap()).unwrap();
    assert_eq!(file.syntax.text(), "fn answer() -> u64 { 6 * 7 }\n");
    let disk: DiskCheck =
        bincode::deserialize(&store.get(root.definitions[0].1).unwrap().unwrap()).unwrap();
    let ir = text::parse(&disk.ir).unwrap();
    assert_eq!(
        eval::run(&ir, FuncId(disk.function.unwrap()), &[], 1_000_000).outcome,
        value
    );
}
#[test]
fn refused_body_is_published_and_unreached_demand_is_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default()
        .check(
            request("fn bad() -> bool { let x = 7; true }\n", dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(!product.accepted());
    assert!(product.diagnostics.iter().any(|r| r.rule == "LANG-11"));
    assert!(
        product
            .definitions
            .values()
            .next()
            .unwrap()
            .check
            .function
            .is_none()
    );
    let absent = ModuleId {
        package: package("app"),
        modules: vec!["absent".into()],
    }
    .definition("missing");
    assert!(
        product
            .demand(&absent)
            .err()
            .unwrap()
            .to_string()
            .contains("outside reached")
    );
}
#[test]
fn damaged_disk_bytes_are_a_miss() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path()).unwrap();
    let key = store.put(b"published payload").unwrap();
    let good = std::fs::read(store.path(key)).unwrap();
    for offset in [0, 8, 12, 20, 52] {
        let mut damaged = good.clone();
        damaged[offset] ^= 1;
        std::fs::write(store.path(key), damaged).unwrap();
        assert!(store.get(key).unwrap().is_none());
    }
    std::fs::write(store.path(key), &good[..good.len() - 1]).unwrap();
    assert!(store.get(key).unwrap().is_none());
}

#[test]
fn unrelated_parse_failure_does_not_suppress_a_valid_body() {
    let dir = tempfile::tempdir().unwrap();
    let mut req = request("fn answer() -> u64 { 42 }\n", dir.path());
    let bad = dir.path().join("broken.hirsel");
    std::fs::write(&bad, "fn broken(\n").unwrap();
    req.inputs.packages[0]
        .sources
        .push(hirsel_core_resolve::ModuleSource {
            file: bad,
            modules: vec!["broken".into()],
            signatures: false,
        });
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert!(!product.accepted());
    let answer = ModuleId {
        package: package("app"),
        modules: vec!["main".into()],
    }
    .definition("answer");
    let d = product.demand(&answer).unwrap();
    assert_eq!(
        eval::run(&d.ir, d.check.function.unwrap(), &[], 1_000_000).outcome,
        eval::Outcome::Returned(vec![42])
    );
}
