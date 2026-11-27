use super::file_uri;
use super::{manifest, write};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;

#[test]
fn image_definition_requires_available_source() {
    use super::super::cache::{Cache, Check};
    use super::super::diagnostics::{InputOrigins, Origin};
    use super::super::documents::{Document, Encoding, Text};
    use super::super::snapshot::EditorSnapshot;
    use super::super::workspace::EditorInputs;
    use crate::package_dir::PackageDir;
    use crate::package_lock::Mode;
    use crate::package_resolve::Resolution;
    use crate::package_store::PackageStore;
    use crate::package_workspace::Workspace;
    use crate::{CheckRequest, Driver, LibraryInput, Meter, PackageProduct};
    use hirsel_core_resolve::{CapturedSource, CorpusManifest, ModuleSource, PackageManifest};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    let directory = tempfile::tempdir().unwrap();
    let temporary = directory.path().canonicalize().unwrap();
    let lib_text = "/// Image declaration.\npub fn answer() -> u64 { 42u64 }\n";
    let core = crate::package("core");
    let image = PackageProduct::sources(
        CorpusManifest {
            core: core.clone(),
            packages: vec![PackageManifest {
                package: core.clone(),
                dependencies: vec![],
                toolchain: true,
                namespace: None,
                sources: vec![ModuleSource {
                    file: temporary.join("lib.hirsel"),
                    modules: vec!["lib".into()],
                    signatures: true,
                    foreign: None,
                    captured: Some(CapturedSource {
                        bytes: lib_text.into(),
                        name: Some("lib.hirsel".into()),
                        test: false,
                    }),
                }],
            }],
        },
        &temporary.join("image"),
        &mut Meter::new(false),
    )
    .unwrap()
    .image;

    let app = temporary.join("app");
    let path = app.join("main.hirsel");
    let text = "fn probe() -> u64 { lib.answer() }";
    write(&path, text);
    write(&app.join("hirsel.toml"), &manifest("local/app", "main"));
    let mode = Mode::Frozen;
    let workspace = Workspace::discover_for(&app, false).unwrap().unwrap();
    let resolved = Resolution::resolve(
        workspace,
        PackageStore::with_transport(None, mode.transport()),
        true,
    )
    .unwrap();
    let member = resolved.workspace.member.as_ref().unwrap();
    let mut driver = Driver::default();
    let mut cache = Cache::default();
    let mut build = || {
        let mut request = CheckRequest::from_app(
            PackageDir::from_resolution(&resolved, member, false).unwrap(),
            None,
            &temporary.join("check"),
        )
        .unwrap();
        request.library = LibraryInput::Image(Arc::clone(&image));
        request
            .inputs
            .packages
            .retain(|package| package.package != core);
        request.entry = None;
        let mut check = Check::default();
        let product = driver
            .check_for_editor(request, &mut cache, &mut check)
            .unwrap();
        assert!(product.accepted(), "{:?}", product.diagnostics);

        let document = Document::open(file_uri(&path), 1, text.to_owned());
        let mut snapshot = EditorSnapshot::new(EditorInputs {
            generation: 1,
            folders: None,
            overlay: BTreeMap::from([(path.clone(), document)]),
        });
        let origins = InputOrigins::from([(
            (member.compiler_id(), String::from("main.hirsel")),
            Origin {
                path: path.clone(),
                text: Text::new(text.into()),
            },
        )]);
        snapshot.retain(
            &product,
            &check,
            &origins,
            &BTreeSet::from([path.clone()]),
            &resolved,
            &temporary.join("sources"),
        );
        snapshot
    };
    let position =
        serde_json::from_value(json!({"line": 0, "character": text.find("answer").unwrap()}))
            .unwrap();
    let answered = build()
        .query(&path, position, true, true, Encoding::Utf16)
        .unwrap();
    let (target, at) = (
        &answered[0]["targetUri"],
        &answered[0]["targetSelectionRange"],
    );
    let target = super::super::documents::file_path(target.as_str().unwrap()).unwrap();
    assert_eq!(fs::read_to_string(target).unwrap(), lib_text);
    assert_eq!(at["start"], json!({"line": 1, "character": 7}));

    // Drop the source text row while leaving the trusted name and signature
    // indexes available. A definition request must never invent a location.
    let definition = image
        .resolve_name(&hirsel_core_ids::DefPath::from_full("core@1/lib/answer").unwrap())
        .unwrap()
        .unwrap();
    let (_, row) = image
        .directory()
        .unwrap()
        .sources
        .get(&definition.site.source.unwrap())
        .unwrap()
        .clone();
    fs::remove_file(image.store.path(row)).unwrap();
    let snapshot = build();
    assert!(
        snapshot
            .query(&path, position, true, true, Encoding::Utf16)
            .is_none()
    );
    let hover = snapshot
        .query(&path, position, false, true, Encoding::Utf16)
        .unwrap();
    assert!(
        hover["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("unknown"),
        "{hover}"
    );
}
