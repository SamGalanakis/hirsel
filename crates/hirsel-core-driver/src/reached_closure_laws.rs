/// FORM #109: rows are reused across instance bodies by name, which is sound only if
/// one name is one type. Before one canonical encoding a substituted `String` field
/// was `Inline(String)` in one body and `Seq(u8)` in another, so a holder's layout
/// came from whichever body published the name first. Every monomorphic definition
/// of LangArena, std and the Core corpus (M1 and M2b programs, front cases) is a root.
///
/// Every unit that carries a body defines it weak under its code identity and the
/// linker keeps one, so each code identity must name one Core IR body, anchors
/// included, across every unit of a product (`carried_law`). A callable adapter
/// anchored at the site that named its function, as before `generated`, broke it on
/// M2b's tx: `transfer$callable` differed between `bench` and `commit`.
fn check_reached_closures(part: usize) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let files = corpus_files;
    let mut m1: CorpusManifest = serde_json::from_slice(
        &std::fs::read(root.join("core/programs/m1/manifest.json")).unwrap(),
    )
    .unwrap();
    for s in m1.packages.iter_mut().flat_map(|p| &mut p.sources) {
        s.file = root.join(&s.file);
    }
    m1.packages.insert(
        0,
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap(),
    );
    let langarena = langarena_inputs(&root, &root.join("core/core"), &root.join("core/langarena"));
    let dir = tempfile::tempdir().unwrap();
    let mut requests: Vec<(String, CheckRequest)> = [("langarena", langarena), ("m1", m1)]
        .into_iter()
        .map(|(name, inputs)| {
            let store = dir.path().join(name);
            (
                name.to_string(),
                CheckRequest {
                    manifest: None,
                    inputs,
                    store,
                    library: LibraryInput::Sources,
                    entry: None,
                },
            )
        })
        .collect();
    let mut cases = files(&root.join("core/front/cases"), true);
    cases.sort_by(|left, right| left.file.cmp(&right.file));
    for (i, case) in cases.into_iter().enumerate() {
        let case_dir = dir.path().join(format!("case{i}"));
        std::fs::create_dir_all(&case_dir).unwrap();
        let source = std::fs::read_to_string(&case.file).unwrap();
        requests.push((case.file.display().to_string(), request(&source, &case_dir)));
    }
    requests.extend(m2b_requests(&root, dir.path()));
    let mut built = 0;
    for (name, request) in requests.into_iter().skip(part).step_by(CLOSURE_PARTS) {
        let mut carried = crate::carried_law::Carried::default();
        let mut added = std::collections::HashSet::new();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        let roots: Vec<_> = product
            .definitions
            .keys()
            .filter(|d| {
                product.demand_body(d).is_ok_and(|p| {
                    p.check.refusals.is_empty()
                        && p.incomplete.is_empty()
                        && p.check.function.is_some()
                        && p.check.template.is_none()
                })
            })
            .map(|d| hirsel_core_ir::Identity::plain(d.clone()))
            .collect();
        for root in roots {
            // A root whose reach includes an unfinished body is not a program.
            // Every closure a reached call names imports into its instance; a row of
            // the same name with another closure would stop the walk as an input.
            let mut set = crate::instances::InstanceSet::default();
            match product.instances_into([root], 1 << 20, &mut set) {
                Ok(reached) => {
                    built += 1;
                    for instance in reached {
                        if added.insert(instance.clone()) {
                            carried.add(set.get(&instance).ir().unwrap());
                        }
                    }
                }
                Err(InstanceStop::Incomplete { .. } | InstanceStop::OutsideReached(_)) => {}
                Err(stop) => panic!("{name}: {stop:?}"),
            }
        }
        if let Some(conflict) = carried.conflict.take() {
            panic!("{name}: {conflict}");
        }
    }
    eprintln!("roots built {built}");
}

/// Each M2b workload's program as core/tools/m2b.py builds it: core and std from
/// their installed sources, then the workload's package closure, with an entry's
/// `use` choosing a package by `id`.
fn m2b_requests(root: &Path, dir: &Path) -> Vec<(String, CheckRequest)> {
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("core/programs/m2b/manifest.json")).unwrap(),
    )
    .unwrap();
    let installed = |name: &str, dependencies: Vec<PackageId>| PackageManifest {
        namespace: None,
        package: package(name),
        sources: corpus_files(&root.join("core").join(name), false)
            .into_iter()
            .map(|s| hirsel_core_resolve::ModuleSource {
                foreign: None,
                captured: None,
                signatures: true,
                ..s
            })
            .collect(),
        dependencies,
        toolchain: false,
    };
    let packages = manifest["packages"].as_array().unwrap();
    let by_name = |name: &str| {
        packages.iter().find(|p| {
            p.get("id")
                .and_then(|i| i.as_str())
                .unwrap_or(p["package"]["name"].as_str().unwrap())
                == name
        })
    };
    let mut requests = Vec::new();
    for entry in manifest["entries"].as_array().unwrap() {
        let workload = entry["workload"].as_str().unwrap();
        let mut inputs = CorpusManifest {
            core: package("core"),
            packages: vec![
                installed("core", vec![]),
                installed("std", vec![package("core")]),
            ],
        };
        let mut pending = vec![entry["package"]["name"].as_str().unwrap().to_string()];
        let mut reached = std::collections::BTreeSet::new();
        while let Some(name) = pending.pop() {
            let name = entry
                .get("use")
                .and_then(|u| u.get(&name))
                .and_then(|n| n.as_str())
                .map_or(name, str::to_string);
            let Some(package) = by_name(&name) else {
                continue;
            };
            if !reached.insert(name) {
                continue;
            }
            for d in package["dependencies"].as_array().unwrap() {
                pending.push(d["name"].as_str().unwrap().to_string());
            }
        }
        for name in reached {
            let mut package = by_name(&name).unwrap().clone();
            package.as_object_mut().unwrap().remove("id");
            let mut package: PackageManifest = serde_json::from_value(package).unwrap();
            for source in &mut package.sources {
                source.file = root.join(&source.file);
            }
            inputs.packages.push(package);
        }
        requests.push((
            format!("m2b {workload}"),
            CheckRequest {
                manifest: None,
                inputs,
                store: dir.join(format!("m2b-{workload}")),
                library: LibraryInput::Sources,
                entry: None,
            },
        ));
    }
    requests
}

const CLOSURE_PARTS: usize = 4;

#[test]
fn every_reached_instance_imports_its_closures_0() {
    check_reached_closures(0);
}

#[test]
fn every_reached_instance_imports_its_closures_1() {
    check_reached_closures(1);
}

#[test]
fn every_reached_instance_imports_its_closures_2() {
    check_reached_closures(2);
}

#[test]
fn every_reached_instance_imports_its_closures_3() {
    check_reached_closures(3);
}
