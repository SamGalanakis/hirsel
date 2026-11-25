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
        foreign: false,
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
