hirsel_guard::install!();

use hirsel_core_driver::CheckRequest;
use hirsel_core_resolve::{ModuleSource, PackageManifest};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(command: &mut Command) -> Vec<u8> {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}: {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output.stdout
}

fn source(file: PathBuf, module: &str) -> ModuleSource {
    ModuleSource {
        file,
        modules: module.split('.').map(str::to_owned).collect(),
        foreign: None,
        signatures: false,
        captured: None,
    }
}

#[test]
fn sdk_manifest_round_trips_the_packed_consumer() {
    let package_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = package_dir.join("../..").canonicalize().unwrap();
    let work = tempfile::tempdir().unwrap();
    let store = work.path().join("store");
    let mut request = CheckRequest::libraries(&root.join("core"), &store).unwrap();
    let package = |name: &str| hirsel_core_ids::PackageId {
        name: name.into(),
        version: "1".into(),
    };
    let library = |name: &str, dependencies: &[&str]| {
        let dir = root.join("core/programs").join(name);
        let manifest: toml::Table = std::fs::read_to_string(dir.join("hirsel.toml"))
            .unwrap()
            .parse()
            .unwrap();
        PackageManifest {
            package: package(name),
            sources: manifest["sources"]
                .as_table()
                .unwrap()
                .iter()
                .map(|(file, module)| source(dir.join(file), module.as_str().unwrap()))
                .collect(),
            dependencies: dependencies.iter().map(|name| package(name)).collect(),
            namespace: None,
            toolchain: false,
        }
    };
    let mut runner = library("runner", &["core", "std", "store"]);
    runner.sources.push(source(
        root.join("sdk/client/tests/packed_consumer.hirsel"),
        "packed_consumer",
    ));
    request
        .inputs
        .packages
        .extend([library("store", &["core", "std"]), runner]);
    let inputs = work.path().join("inputs.json");
    std::fs::write(&inputs, serde_json::to_vec(&request.inputs).unwrap()).unwrap();

    let hirsel = package_dir
        .join(env!("CARGO_BIN_EXE_hirsel"))
        .canonicalize()
        .unwrap();
    let runtime = root.join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").unwrap());
    let executable = work.path().join("server");
    run(Command::new(&hirsel)
        .current_dir(&root)
        .args([
            "build",
            "--source-core",
            "--source-std",
            "--backend",
            "cranelift",
        ])
        .arg("--inputs")
        .arg(&inputs)
        .arg("--runtime")
        .arg(runtime)
        .args(["--entry", "runner@1/packed_consumer/main"])
        .args([
            "--grant", "Stdout", "--grant", "Clock", "--grant", "Random", "--grant", "Tasks",
        ])
        .arg("--store")
        .arg(&store)
        .arg("-o")
        .arg(&executable));

    let bindings = work.path().join("bindings");
    run(Command::new(&hirsel)
        .args(["sdk", "ts"])
        .arg(&executable)
        .arg("-o")
        .arg(&bindings));
    let bytes = run(Command::new(&hirsel).args(["sdk", "json"]).arg(&executable));
    assert_eq!(
        bytes,
        std::fs::read(bindings.join("manifest.json")).unwrap()
    );

    let manifest: Value = serde_json::from_slice(&bytes).unwrap();
    let mut round_trip = serde_json::to_vec(&manifest).unwrap();
    round_trip.push(b'\n');
    assert_eq!(bytes, round_trip);

    assert_eq!(manifest["version"], 1);
    assert_eq!(manifest["launch"]["n_place"], "16777216");
    let tables = manifest["tables"].as_array().unwrap();
    assert_eq!(tables.len(), 2);
    assert!(tables.iter().any(|table| table["name"] == "items"));
    let entries = manifest["entries"].as_array().unwrap();
    assert!(entries.iter().all(|entry| {
        entry["module"] == json!(["packed_consumer"])
            && entry["args_fp"].as_str().unwrap().parse::<u64>().is_ok()
            && entry["result_fp"].as_str().unwrap().parse::<u64>().is_ok()
    }));

    let unit = json!({"key": false, "root": [0], "definitions": []});
    let [reply, rows, room] = entries.as_slice() else {
        panic!("entries are packed_reply, packed_rows, packed_room: {entries:?}")
    };
    assert_eq!(reply["name"], "packed_reply");
    assert_eq!(reply["kind"], "compute");
    assert_eq!(reply["args"], unit);
    assert_eq!(reply["slots"], json!([]));
    assert_eq!(
        reply["result"],
        json!({
            "key": false,
            "root": [19, 0],
            "definitions": [{
                "name": "runner.packed_consumer.Reply",
                "arguments": [[5]],
                "kind": 1,
                "body": [
                    {"name": "Value", "removed": false, "fields": [[16, [16, [5]]], [6]]},
                    {"name": "Missing", "removed": false, "fields": []},
                    {"name": "Old", "removed": true, "fields": [[10], [14, [3]], [15, [6], [13]]]}
                ]
            }]
        })
    );
    assert_eq!(rows["name"], "packed_rows");
    assert_eq!(rows["kind"], "query");
    assert_eq!(rows["args"], unit);
    assert_eq!(rows["result"], unit);
    assert_eq!(
        rows["slots"],
        json!([{
            "name": "items",
            "kind": "table",
            "key": {"key": true, "root": [6], "definitions": []},
            "key_json": {"key": false, "root": [6], "definitions": []},
            "value": {
                "key": false,
                "root": [19, 0],
                "definitions": [{
                    "name": "runner.packed_consumer.Item",
                    "arguments": [],
                    "kind": 0,
                    "body": [
                        {"name": "title", "removed": false, "type": [13]},
                        {"name": "done", "removed": false, "type": [1]}
                    ]
                }]
            }
        }])
    );
    // docs/spec/sdk.md: a room's slots are its declared slots, as for a
    // query, and only a room has `room` with its state and message value
    // codecs and the bounds packed_consumer.hirsel passes to runner.room.
    assert_eq!(room["name"], "packed_room");
    assert_eq!(room["kind"], "room");
    assert_eq!(room["args"], unit);
    assert_eq!(room["slots"], rows["slots"]);
    assert!(reply.get("room").is_none() && rows.get("room").is_none());
    assert_eq!(
        room["room"],
        json!({
            "state": {
                "key": false,
                "root": [19, 0],
                "definitions": [{
                    "name": "runner.packed_consumer.Cursor",
                    "arguments": [],
                    "kind": 0,
                    "body": [
                        {"name": "x", "removed": false, "type": [5]},
                        {"name": "y", "removed": false, "type": [5]}
                    ]
                }]
            },
            "message": {
                "key": false,
                "root": [19, 0],
                "definitions": [{
                    "name": "runner.packed_consumer.Note",
                    "arguments": [],
                    "kind": 1,
                    "body": [
                        {"name": "Ping", "removed": false, "fields": [[6]]},
                        {"name": "Text", "removed": false, "fields": [[13]]}
                    ]
                }]
            },
            "peers": 4,
            "state_max": 8,
            "message_max": 64
        })
    );
}

#[test]
fn swift_sdk_writes_one_projection_and_refuses_target_dependent_entries() {
    use hirsel_core_ids::embed::binding_projection;
    use hirsel_core_layout::Target;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::os::unix::fs::symlink;

    let package_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = package_dir.join("../..").canonicalize().unwrap();
    let input = |name| {
        root.join(std::env::var_os(name).unwrap())
            .canonicalize()
            .unwrap()
    };
    let hirsel = package_dir
        .join(env!("CARGO_BIN_EXE_hirsel"))
        .canonicalize()
        .unwrap();
    let work = tempfile::tempdir().unwrap();
    let prefix = work.path().join("prefix");
    let kit = input("HIRSEL_SWIFT_LINK_KIT");
    let mac_embed = input("HIRSEL_SWIFT_MAC_EMBED");
    let mac_runtime = input("HIRSEL_SWIFT_MAC_RUNTIME");
    let ios = input("HIRSEL_SWIFT_IOS_EMBED");
    let apple_targets: Vec<_> = Target::ALL.into_iter().filter_map(Target::apple).collect();
    for apple in &apple_targets {
        let mut install = Command::new(&hirsel);
        install
            .current_dir(&root)
            .args(["core-product", "--install"])
            .arg(&prefix)
            .args(["--target", apple.triple, "--link-kit"])
            .arg(&kit);
        if apple.family == "macos" {
            install
                .arg("--runtime")
                .arg(&mac_runtime)
                .arg("--embed-runtime")
                .arg(&mac_embed);
        } else {
            install
                .arg("--embed-runtime")
                .arg(ios.join(apple.triple).join("libhirsel_core_rt.a"));
        }
        run(&mut install);
    }

    let workspace = work.path().join("workspace");
    let package = workspace.join("app");
    let libraries = workspace.join("libraries");
    let programs = libraries.join("programs");
    fs::create_dir_all(&package).unwrap();
    fs::create_dir_all(&programs).unwrap();
    for name in ["core", "std"] {
        symlink(root.join("core").join(name), libraries.join(name)).unwrap();
    }
    fs::write(
        workspace.join("hirsel.toml"),
        "[workspace]\nmembers = [\"store\", \"runner\", \"app\"]\n",
    )
    .unwrap();
    for name in ["runner", "store"] {
        let staged = workspace.join(name);
        copy_tree(&root.join("core/programs").join(name), &staged);
        symlink(staged, programs.join(name)).unwrap();
    }
    fs::write(
        package.join("hirsel.toml"),
        r#"[package]
name = "swift_law"
major = 1
charge = "charge-v1"

[dependencies]
std = { major = 1 }
store = { major = 1, path = "../store" }
runner = { major = 1, path = "../runner" }

[sources]
"main.hirsel" = "main"
"#,
    )
    .unwrap();
    let source = fs::read_to_string(root.join("sdk/client/tests/packed_consumer.hirsel")).unwrap();
    let path = package.join("main.hirsel");
    fs::write(&path, &source).unwrap();
    let cache = work.path().join("cache");
    let store = work.path().join("compiler-store");
    let out = work.path().join("out");
    let command = |output: &Path| {
        let mut command = Command::new(prefix.join("hirsel"));
        command
            .current_dir(&root)
            .env("XDG_CACHE_HOME", &cache)
            .args(["sdk", "swift"])
            .arg(&package)
            .arg("--libraries")
            .arg(&libraries)
            .arg("--store")
            .arg(&store)
            .args(["--limit", "1073741824", "-o"])
            .arg(output);
        command
    };
    run(&mut command(&out));

    let projection = fs::read(out.join("projection.json")).unwrap();
    let decoded: Value = serde_json::from_slice(&projection).unwrap();
    assert_eq!(decoded["version"], 1);
    assert_eq!(decoded.as_object().unwrap().len(), 3);
    assert!(decoded.get("launch").is_none());
    let entries = decoded["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert!(entries.iter().all(|e| e.get("revision").is_none()));
    assert_eq!(entries[0]["name"], "packed_reply");
    let enums = &entries[0]["result"]["definitions"][0]["body"];
    assert_eq!(enums[0]["fields"][0], json!([16, [16, [5]]]));

    let source_dir = out.join("Sources/SwiftLaw");
    let generated_entries = fs::read_to_string(source_dir.join("Entries.swift")).unwrap();
    let fingerprint: &[u8] = &Sha256::digest(&projection)[..];
    assert!(generated_entries.contains(&format!("let manifest_fp = Data({fingerprint:?})")));
    let codec = fs::read_to_string(source_dir.join("Codec.swift")).unwrap();
    assert!(codec.contains("HirselNestedOption<Optional<UInt32>>"));
    assert!(codec.contains("public indirect enum RoomEvent:"));
    let runtime = fs::read_to_string(source_dir.join("Runtime.swift")).unwrap();
    assert!(runtime.contains("clientBytes: UInt64 = 64 * 1024 * 1024"));
    assert!(runtime.contains("startTimeoutMs: UInt32 = 10_000"));
    assert!(runtime.contains(".bufferingOldest(buffer)"));

    let bundle = out.join("SwiftLawCore.xcframework");
    assert_eq!(
        fs::read_to_string(bundle.join("Info.plist")).unwrap(),
        include_str!("sdk_manifest/ExpectedInfo.plist")
    );
    for apple in apple_targets {
        let file = format!("manifests/{}.json", apple.xcframework_slice);
        let manifest = fs::read(out.join(file)).unwrap();
        let decoded: Value = serde_json::from_slice(&manifest).unwrap();
        assert_eq!(decoded["version"], 1);
        assert_eq!(decoded["launch"]["target"], apple.triple);
        assert_eq!(decoded["launch"]["digest"], json!([]));
        assert!(decoded["entries"].as_array().unwrap().iter().all(|entry| {
            entry["revision"]
                .as_array()
                .is_some_and(|bytes| bytes.len() == 32)
        }));
        assert_eq!(projection, binding_projection(&manifest).unwrap());
        let slice = bundle.join(apple.xcframework_slice);
        let archive = fs::read(slice.join("libswift_law.a")).unwrap();
        let embedded_manifest = manifest_in_archive(&archive);
        assert_eq!(manifest, embedded_manifest);
        let digest: &[u8] = &Sha256::digest(&archive)[..];
        assert!(generated_entries.contains(&format!("Data({digest:?})")));
        let header = fs::read_to_string(slice.join("Headers/hirsel_product.h")).unwrap();
        assert!(header.contains("(const volatile uint8_t *)"));
        let helper = header
            .lines()
            .find_map(|line| line.strip_prefix("static inline void "))
            .unwrap()
            .split_once('(')
            .unwrap()
            .0;
        assert!(generated_entries.contains(&format!("{helper}()")));
        let prefix = helper
            .strip_prefix("hirsel_embed_v1_require_anchor_")
            .unwrap();
        check_archive_exports(&archive, prefix);
        let module = fs::read_to_string(slice.join("Headers/module.modulemap")).unwrap();
        assert!(module.contains("link framework \"CoreFoundation\""));
        assert!(module.contains("link framework \"Security\""));
    }
    for (file, tests) in [
        (
            "CodecVectors.swift",
            &[
                "testMixedPartAppliesStateBeforeMessageAndLeave",
                "testFirstFullPartHasNoPeersEvent",
                "testNestedSomeNoneAndMalformedBytes",
            ][..],
        ),
        (
            "CancellationRaces.swift",
            &[
                "testCancellationHeldAtCRegistrationBoundary",
                "testEarlyOutcomeBeforeCReturnsWins",
                "testAbandonNotLiveLeavesDeliveryInCharge",
            ][..],
        ),
    ] {
        let text = fs::read_to_string(out.join("Tests/SwiftLawTests").join(file)).unwrap();
        assert!(tests.iter().all(|name| text.contains(name)));
    }

    let changed = source.replace(
        "    runner.compute(mut registry, code.describe(packed_reply), 1024u64, runner.DEFAULT_STEPS)?",
        "    if target.os == (Macos : target.Os) {\n        runner.compute(mut registry, code.describe(packed_reply), 1024u64, runner.DEFAULT_STEPS)?\n    }",
    );
    assert_ne!(changed, source);
    fs::write(path, changed).unwrap();
    let refused = work.path().join("refused");
    let result = command(&refused).output().unwrap();
    assert!(!result.status.success());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(
        error.contains("TargetDependentEntries(aarch64-apple-darwin, aarch64-apple-ios)"),
        "{error}"
    );
    assert!(!refused.exists());
}

fn check_archive_exports(archive: &[u8], prefix: &str) {
    use hirsel_core_ids::embed::EXPORTS_V1;
    use object::read::archive::ArchiveFile;
    use object::{Object, ObjectSymbol};

    let file = ArchiveFile::parse(archive).unwrap();
    let anchor = format!("_hirsel_embed_v1_anchor_{prefix}");
    let member = file
        .members()
        .map(Result::unwrap)
        .find(|member| member.name() == b"exports.o")
        .unwrap();
    let object = object::File::parse(member.data(archive).unwrap()).unwrap();
    let symbols = object
        .symbols()
        .filter(|symbol| symbol.is_definition())
        .map(|symbol| symbol.name().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(symbols.contains(anchor.as_str()));
    for name in EXPORTS_V1 {
        assert!(
            symbols.contains(format!("_{name}").as_str()),
            "missing {name}"
        );
    }
}

fn manifest_in_archive(archive: &[u8]) -> &[u8] {
    use object::read::archive::ArchiveFile;
    use object::{Object, ObjectSection};
    let file = ArchiveFile::parse(archive).unwrap();
    for member in file.members() {
        let member = member.unwrap();
        let object = object::File::parse(member.data(archive).unwrap()).unwrap();
        if let Some(section) = object.section_by_name("__hirsel_sdk") {
            return section.data().unwrap();
        }
    }
    panic!("archive has no SDK manifest")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
