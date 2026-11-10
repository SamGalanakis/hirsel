use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use hirsel_intern::jobs::with_compiler_stack;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn explicit_launch_selects_source_std_without_registry_status() {
    with_compiler_stack(|| {
        let source = root().join("conformance/PROG/PROG-1-source-dependencies-run.hir");
        let manifest = source.with_extension("hir.json");
        let mut p = load_program(&manifest).unwrap();
        p.check_for_execution().unwrap();
        let std = p
            .engine
            .lower_to_tir(&DefPath(hirsel_syntax::ItemPath {
                package: hirsel_syntax::PackageId {
                    name: "std".into(),
                    version: "1".into(),
                },
                modules: vec!["probe".into()],
                name: "identity".into(),
            }))
            .unwrap();
        assert_eq!(std.value.path.path().0.package.name, "std");
        assert!(
            !hirsel_registry::BuiltinRegistry::new()
                .signatures()
                .any(|s| s.definition.path().0.package.name_is("std"))
        );
        assert!(matches!(
            run_path(&manifest, &mut Vec::new()).unwrap(),
            RunOutcome::Returned(hirsel_run_schema::Returned::Ok)
        ));
    });
}

#[test]
fn launch_metadata_errors_are_tool_errors() {
    let dir_scratch = tempfile::Builder::new()
        .prefix("hirsel-packages-")
        .tempdir()
        .unwrap();
    let dir = dir_scratch.path().to_path_buf();
    std::fs::write(dir.join("main.hir"), "pub fn main(t: Test) {}\n").unwrap();
    let base = serde_json::json!({"schema":1,"package":{"name":"app","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}});
    let path = dir.join("hirsel.json");
    for mutate in [0, 1, 2, 3, 4, 5, 6, 7, 8] {
        let mut manifest = base.clone();
        match mutate {
            0 => manifest["schema"] = 3.into(),
            1 => manifest["unknown"] = true.into(),
            2 => manifest["package"]["name"] = "not/a/name".into(),
            3 => manifest["package"] = serde_json::json!({"name":"core","version":"1"}),
            4 => {
                let source = manifest["sources"][0].clone();
                manifest["sources"].as_array_mut().unwrap().push(source);
            }
            5 => {
                manifest["dependencies"] = serde_json::json!([{"manifest":"hirsel.json","package":{"name":"app","version":"1"},"expose":[]}])
            }
            6 => manifest["package"]["version"] = "1-rc".into(),
            7 => manifest["package"]["name"] = "fn".into(),
            // Schema 2 selects through its catalog, not schema 1's edge form.
            _ => {
                manifest["schema"] = 2.into();
                manifest["dependencies"] = serde_json::json!([{"manifest":"hirsel.json","package":{"name":"app","version":"1"},"expose":[]}])
            }
        }
        std::fs::write(&path, manifest.to_string()).unwrap();
        assert!(
            matches!(load_program(&path), Err(ToolError::Runtime(_))),
            "mutation {mutate}"
        );
    }
    let duplicate = base.to_string().replacen("{", "{\"schema\":1,", 1);
    std::fs::write(&path, duplicate).unwrap();
    assert!(matches!(load_program(&path), Err(ToolError::Runtime(_))));
    std::fs::write(&path, base.to_string()).unwrap();
    assert!(load_program(&path).is_ok());
    std::fs::remove_dir_all(dir).unwrap();
}

/// PROG-1's source-dependency program, bundled and moved: a source outside the
/// program's directory, two exposed packages and a nested dependency on another
/// version of one of them. Without its std dependency: a bundle renames std's
/// sources, so the toolchain's precheck cannot serve it and the run would check
/// all of std from source for a manifest rewrite that treats std like any package.
#[test]
fn packaged_replay_moves_without_original_manifest_paths() {
    with_compiler_stack(|| {
        let fixtures = std::fs::canonicalize(root().join("packages/fixtures")).unwrap();
        let program = root().join("conformance/PROG/PROG-1-source-dependencies-run.hir");
        let text = std::fs::read_to_string(&program)
            .unwrap()
            .replace("std.identity(42)", "42");
        assert!(!text.contains("std."));
        let input_scratch = tempfile::Builder::new()
            .prefix("hirsel-package-input-")
            .tempdir()
            .unwrap();
        let input = input_scratch.path().to_path_buf();
        let source = input.join("source.hir");
        std::fs::write(&source, &text).unwrap();
        let dependency = |name: &str, head: &str, module: &[&str]| {
            serde_json::json!({
                "manifest": fixtures.join(format!("{name}@1/hirsel.json")),
                "package": {"name": name, "version": "1"},
                "expose": [{"head": head, "module": module}],
            })
        };
        let manifest = serde_json::json!({
            "schema": 1,
            "package": {"name": "application", "version": "1"},
            "entry": {"module": [], "name": "main"},
            "sources": [
                {"path": "source.hir", "module": []},
                {"path": fixtures.join("helper.hir"), "module": ["helper"]},
            ],
            "dependencies": [
                dependency("probe", "vendor", &["api"]),
                dependency("wrapper", "other", &[]),
            ],
        });
        std::fs::write(source.with_extension("hir.json"), manifest.to_string()).unwrap();
        let dir_scratch = tempfile::Builder::new()
            .prefix("hirsel-package-bundle-")
            .tempdir()
            .unwrap();
        let dir = dir_scratch.path().join("bundle");
        let moved = dir.with_extension("moved");
        assert!(bundle_program(&source, &text, &dir).unwrap());
        std::fs::remove_dir_all(&input).unwrap();
        std::fs::rename(&dir, &moved).unwrap();
        assert!(matches!(
            run_path(&moved.join("source.hir"), &mut Vec::new()).unwrap(),
            RunOutcome::Returned(hirsel_run_schema::Returned::Ok)
        ));
        std::fs::remove_dir_all(moved).unwrap();
    });
}

#[test]
fn short_and_qualified_std_calls_keep_the_same_definition_dependencies() {
    with_compiler_stack(|| {
        let path = root().join("conformance/LANG/LANG-4-std-short-run.hir");
        let source = std::fs::read_to_string(&path).unwrap();
        let qualified = source
            .replace("string.find(", "std@1/string.find(")
            .replace("string.starts_with(", "std@1/string.starts_with(")
            .replace("string.split(", "std@1/string.split(")
            .replace("bytes.replace(", "std@1/bytes.replace(");
        let mut dependencies = Vec::new();
        for text in [&source, &qualified] {
            let mut program = load_program_source(&path, Some(text)).unwrap();
            let resolution = program.engine.resolve_definition(&program.entry).unwrap();
            assert!(resolution.diagnostics.is_empty());
            let paths: std::collections::BTreeSet<_> = resolution
                .value
                .dependencies
                .iter()
                .map(ToString::to_string)
                .collect();
            assert!(paths.contains("std@1/string/find"));
            assert!(paths.contains("core@1/string/byte_len"));
            assert!(paths.contains("std@1/bytes/replace"));
            dependencies.push(paths);
        }
        assert_eq!(dependencies[0], dependencies[1]);
    });
}

#[cfg(unix)]
#[test]
fn dependency_directory_aliases_share_identity_without_merging_revisions() {
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        for (name, value) in [("first", 11), ("second", 29)] {
            let dependency = root.join(name);
            std::fs::create_dir(&dependency).unwrap();
            std::fs::write(
                dependency.join("value.hir"),
                format!("pub fn value() -> u64 {{ {value}u64 }}\n"),
            )
            .unwrap();
            std::fs::write(
                dependency.join("hirsel.json"),
                serde_json::json!({
                    "schema": 1, "package": {"name": "dep", "version": "1"},
                    "sources": [{"path": "value.hir", "module": []}], "dependencies": [],
                })
                .to_string(),
            )
            .unwrap();
        }
        std::os::unix::fs::symlink("first", root.join("alias")).unwrap();
        std::fs::write(
            root.join("main.hir"),
            "pub fn main() -> u64 { dep@1/value() }\n",
        )
        .unwrap();
        let manifest = root.join("hirsel.json");
        let write = |paths: &[&str], schema: u64| {
            let mut value = serde_json::json!({
                "schema": 1, "package": {"name": "app", "version": "1"},
                "sources": [{"path": "main.hir", "module": []}],
                "entry": {"module": [], "name": "main"},
                "dependencies": paths.iter().map(|path| serde_json::json!({
                    "manifest": format!("{path}/hirsel.json"),
                    "package": {"name": "dep", "version": "1"}, "expose": [],
                })).collect::<Vec<_>>(),
            });
            if schema == 2 {
                value["schema"] = 2.into();
                value["parameters"] = serde_json::json!({});
                value["dependency_catalog"] = paths
                    .iter()
                    .enumerate()
                    .map(|(i, _)| (format!("dep{i}"), value["dependencies"][i].clone()))
                    .map(|(id, mut descriptor)| {
                        descriptor.as_object_mut().unwrap().remove("expose");
                        (id, descriptor)
                    })
                    .collect::<serde_json::Map<_, _>>()
                    .into();
                value["dependencies"] = paths.iter().enumerate().map(|(i, _)| {
                    serde_json::json!({"use": format!("dep{i}"), "bindings": {}, "expose": []})
                }).collect::<Vec<_>>().into();
            }
            std::fs::write(&manifest, value.to_string()).unwrap();
        };
        for schema in [1, 2] {
            let mut identities = Vec::new();
            for path in ["first", "alias", "second"] {
                write(&[path], schema);
                let program = load_program(&manifest).unwrap();
                identities.push(program.engine.environment.package_manifest_hash);
            }
            assert_eq!(identities[0], identities[1], "one canonical dependency");
            assert_ne!(
                identities[0], identities[2],
                "distinct implementation revisions"
            );
            write(&["first", "alias"], schema);
            let program = load_program(&manifest).unwrap();
            assert_eq!(program.package_kinds.len(), 2);
            write(&["first", "second"], schema);
            let Err(ToolError::Runtime(message)) = load_program(&manifest) else {
                panic!("distinct manifests selected one package version");
            };
            assert!(
                message.contains("two implementation revisions selected for one package"),
                "{message}"
            );
        }
        let std = hirsel_engine::toolchain_std_manifest().unwrap();
        std::os::unix::fs::symlink(std.parent().unwrap(), root.join("std-alias")).unwrap();
        let mut identities = Vec::new();
        for path in [std, root.join("std-alias/hirsel.json")] {
            write(&["first"], 1);
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
            value["dependencies"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "manifest": path, "package": {"name": "std", "version": "1"}, "expose": [],
                }));
            std::fs::write(&manifest, value.to_string()).unwrap();
            identities.push(
                load_program(&manifest)
                    .unwrap()
                    .engine
                    .environment
                    .package_manifest_hash,
            );
        }
        assert_eq!(
            identities[0], identities[1],
            "one selected std installation"
        );
    });
}
