use super::*;
use hirsel_core_ids::PackageId;
use hirsel_core_syntax::Symbols;

fn package(name: &str) -> PackageId {
    PackageId {
        name: name.into(),
        version: "1".into(),
    }
}

/// SEM-10: a scoped package's schema definition path keeps its `.` and `/` and carries
/// its host, so two scopes with one `NAME` never share a fingerprint path.
#[test]
fn sem_10_scoped_package_schema_paths_keep_the_whole_name() {
    let std = package("std");
    let path = |name: &str, module: &str| {
        let d = codec_derive(&package(name), std::slice::from_ref(&std), &[module.into()])
            .expect("a package over std derives");
        assert_eq!(d.codec, "std.codec.");
        d.path
    };
    assert_eq!(
        format!("{}.Row", path("hirsel.ai/sqlite", "sqlite")),
        "hirsel.ai/sqlite.sqlite.Row"
    );
    assert_eq!(path("a.test/json", "json"), "a.test/json.json");
    assert_ne!(path("a.test/json", "json"), path("b.test/json", "json"));
}

/// An `--inputs` manifest is outside input. The 2026-11-28 nightly's store-crash
/// manifest named its dependency by its `hirsel.hcfg` alias (`jwtbind`) instead of
/// its package (`hirsel.ai/jwtbind`); indexing it panicked instead of refusing.
#[test]
fn inputs_naming_an_unlisted_dependency_are_refused_not_indexed() {
    let manifest = |package: PackageId, dependencies: Vec<PackageId>| PackageManifest {
        package,
        sources: vec![],
        dependencies,
        namespace: None,
        toolchain: false,
    };
    let manifests = [
        manifest(package("core"), vec![]),
        manifest(package("hirsel.ai/jwtbind"), vec![package("core")]),
        manifest(
            package("hirsel.ai/store_crash"),
            vec![package("core"), package("jwtbind")],
        ),
    ];
    let mut symbols = Symbols::default();
    for m in &manifests {
        m.intern(&mut symbols);
    }
    let mut index = NameIndex::new(std::sync::Arc::new(symbols));
    let refusal = index.add_manifests(&manifests).unwrap_err();
    assert_eq!(
        refusal,
        UnboundDependency {
            package: package("hirsel.ai/store_crash"),
            dependency: package("jwtbind"),
        }
    );
}
