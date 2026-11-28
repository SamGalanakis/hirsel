use super::*;
fn sorted_json<T: Serialize>(rows: T) -> Vec<String> {
    let value = serde_json::to_value(rows).unwrap();
    let mut rows = value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>();
    rows.sort();
    rows
}
fn compare(source: &CheckProduct, product: &CheckProduct) {
    assert_eq!(
        sorted_json(source.machine_diagnostics().unwrap()),
        sorted_json(product.machine_diagnostics().unwrap())
    );
    assert_eq!(
        sorted_json(source.machine_incomplete().unwrap()),
        sorted_json(product.machine_incomplete().unwrap())
    );
    for (path, body) in &source.definitions {
        if path.0.package == package("core") {
            continue;
        }
        let image = product.demand_body(path).unwrap();
        assert_eq!(
            hirsel_core_ir::text::print(body.ir().unwrap()),
            hirsel_core_ir::text::print(image.ir().unwrap()),
            "CoreIR for {path}"
        );
        assert_eq!(
            encode(&body.check.template).unwrap(),
            encode(&image.check.template).unwrap(),
            "template {path}"
        );
    }
}
fn run(inputs: CorpusManifest, entry: Option<DefPath>, dir: &Path, cache: &Path) {
    let expect_std = inputs.packages.iter().any(|p| p.package == package("std"));
    let mut request = CheckRequest {
        manifest: None,
        inputs,
        store: dir.join("source"),
        library: LibraryInput::Sources,
        entry: entry.clone(),
    };
    let source = Driver::default()
        .check(request.clone(), &mut Meter::new(false))
        .unwrap();
    request.store = dir.join("product");
    let product = Driver::default()
        .check_with_product(request, cache, &mut Meter::new(false))
        .unwrap();
    compare(&source, &product);
    if expect_std {
        assert!(product.image.is_some());
        assert!(product.files.iter().all(|f| {
            ![package("core"), package("std")]
                .contains(&product.index.module_path(f.module).package)
        }));
        assert!(
            product
                .rechecked
                .iter()
                .all(|p| ![package("core"), package("std")].contains(&p.0.package))
        );
        let entry = entry.unwrap();
        let a = source.reachable(&entry, 65_536).unwrap();
        let b = product.reachable(&entry, 65_536).unwrap();
        assert!(a.accepted() && b.accepted());
        assert_eq!(a.instances.as_ref().unwrap(), b.instances.as_ref().unwrap());
        for key in a.instances.as_ref().unwrap() {
            assert_eq!(
                hirsel_core_ir::text::print(a.materialized.get(key).ir().unwrap()),
                hirsel_core_ir::text::print(b.materialized.get(key).ir().unwrap()),
                "instance {key:?}"
            );
        }
    }
}
const FRONT_PARTS: usize = 4;

fn front_cases_agree_with_published_core(part: usize) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("products");
    fn walk(path: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "hirsel") {
                out.push(path);
            }
        }
    }
    let mut cases = vec![];
    walk(&root.join("core/front/cases"), &mut cases);
    cases.sort();
    let core = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let count = cases.len();
    assert!(count > 200);
    for (i, file) in cases
        .into_iter()
        .enumerate()
        .skip(part)
        .step_by(FRONT_PARTS)
    {
        let inputs = CorpusManifest {
            core: package("core"),
            packages: vec![
                core.clone(),
                PackageManifest {
                    namespace: None,
                    package: package("app"),
                    dependencies: vec![package("core")],
                    sources: vec![ModuleSource {
                        foreign: None,
                        captured: None,
                        file: file.clone(),
                        modules: vec!["case".into()],
                        signatures: false,
                    }],
                    toolchain: false,
                },
            ],
        };
        // Entry policy is the same one used by the front corpus.
        let text = std::fs::read_to_string(&file).unwrap();
        let entry = text
            .contains("fn main(")
            .then(|| DefPath::from_full("app@1/case/main").unwrap());
        run(inputs, entry, &dir.path().join(format!("case{i}")), &cache);
    }
}

#[test]
fn front_cases_0_agree_with_published_core() {
    front_cases_agree_with_published_core(0);
}

#[test]
fn front_cases_1_agree_with_published_core() {
    front_cases_agree_with_published_core(1);
}

#[test]
fn front_cases_2_agree_with_published_core() {
    front_cases_agree_with_published_core(2);
}

#[test]
fn front_cases_3_agree_with_published_core() {
    front_cases_agree_with_published_core(3);
}

#[test]
fn m1_closures_agree_with_published_core() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("products");
    let core = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("core/programs/m1/manifest.json")).unwrap(),
    )
    .unwrap();
    let mut inputs: CorpusManifest = serde_json::from_value(manifest.clone()).unwrap();
    for p in &mut inputs.packages {
        for s in &mut p.sources {
            s.file = root.join(&s.file);
        }
    }
    inputs.packages.insert(0, core);
    for (i, entry) in manifest["entries"].as_array().unwrap().iter().enumerate() {
        let name = entry["package"]["name"].as_str().unwrap();
        let mut selected = inputs.clone();
        selected
            .packages
            .retain(|p| ["core", "std", "m1", name].contains(&p.package.name.as_str()));
        let path = DefPath::from_full(&format!(
            "{name}@1/{}/{}",
            entry["module"].as_str().unwrap(),
            entry["function"].as_str().unwrap()
        ))
        .unwrap();
        run(
            selected,
            Some(path),
            &dir.path().join(format!("m1{i}")),
            &cache,
        );
    }
}
