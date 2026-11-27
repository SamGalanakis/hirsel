use super::*;
use hirsel_core_ids::PackageId;

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
