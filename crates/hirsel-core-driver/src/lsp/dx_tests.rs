use super::{Client, capture, cli, driver, file_uri, folder, has_rule, manifest, point, write};
use serde_json::json;
use std::fs;

#[test]
fn detached_buffers_use_the_toolchain_without_a_disk_file() {
    let driver = driver();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    let path = root.join("scratch.hirsel");
    let cache = root.join("cache");
    fs::create_dir(&workspace).unwrap();
    let before = capture(&root);
    let body = "fn collect() -> u64 { let _ = std.string.starts_with(\"hirsel\", \"hir\"); let count = 1u64; count }";
    let bad = "fn broken() -> u64 { true }";
    let source = format!("{body}\n{bad}\n");
    let mut client = Client::start(&driver, &root, &cache);
    client.request(
        1,
        "initialize",
        json!({"workspaceFolders": [folder(&workspace)]}),
    );
    assert!(client.response(1)["result"].is_object());
    client.open(&path, &source);
    let position = point(body, body.rfind("count").unwrap(), 0, "utf-16");
    let mut query = json!({"textDocument": {"uri": file_uri(&path)}, "position": position});
    client.request(2, "textDocument/hover", query.clone());
    let hover = client.response(2);
    assert!(hover["result"].to_string().contains("u64"), "{hover}");
    client.request(3, "textDocument/completion", query.clone());
    let completion = client.response(3);
    assert!(
        completion["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["label"] == "count"),
        "{completion}"
    );
    let use_std = point(body, body.find("starts_with").unwrap(), 0, "utf-16");
    client.request(
        4,
        "textDocument/definition",
        json!({
            "textDocument": {"uri": file_uri(&path)}, "position": use_std
        }),
    );
    let definition = client.response(4);
    let target = definition["result"][0]["uri"].as_str().unwrap();
    let target = super::super::documents::file_path(target).unwrap();
    assert!(
        fs::read_to_string(target)
            .unwrap()
            .contains("fn starts_with")
    );
    query["newName"] = json!("length");
    client.request(5, "textDocument/rename", query);
    let rename = client.response(5);
    let edits = &rename["result"]["documentChanges"][0]["edits"];
    assert_eq!(edits.as_array().map(Vec::len), Some(2), "{rename}");
    assert_eq!(edits[0]["newText"], "length");
    client.shutdown(6);

    let mut client = Client::start(&driver, &root, &cache);
    client.request(1, "initialize", json!({}));
    assert!(client.response(1)["result"].is_object());
    client.open(&path, &source);
    let diagnostics = client.diagnostics(&path, Some(1));
    assert!(has_rule(&diagnostics, "LANG-11"), "{diagnostics}");
    assert_eq!(diagnostics.as_array().unwrap().len(), 1, "{diagnostics}");
    client.shutdown(2);
    assert_eq!(capture(&root), before);
}

#[test]
fn unlocked_package_explains_how_to_restore_analysis_in_the_open_file() {
    let driver = driver();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = root.join("main.hirsel");
    let manifest_path = root.join("hirsel.toml");
    let source = "fn broken() -> u64 { true }\n";
    write(&manifest_path, &manifest("local/app", "main"));
    write(&path, source);
    let before = capture(&root);
    let mut client = Client::start(&driver, &root, &root.join("cache"));
    client.request(1, "initialize", json!({}));
    assert!(client.response(1)["result"].is_object());
    client.open(&path, source);
    let diagnostics = client.diagnostics(&path, Some(1));
    assert!(
        diagnostics.to_string().contains("hirsel lock"),
        "{diagnostics}"
    );
    client.shutdown(2);
    assert_eq!(capture(&root), before);
}

#[test]
fn watching_manifest_and_lock_republishes_open_buffer_diagnostics() {
    let driver = driver();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    let path = workspace.join("main.hirsel");
    let manifest_path = workspace.join("hirsel.toml");
    let lock = workspace.join("hirsel.lock");
    let initial = manifest("local/app", "main");
    let source = "fn broken() -> u64 { true }\n";
    write(&manifest_path, &initial);
    write(&path, source);
    cli(&driver, &root, &workspace, &["lock"], 0);
    let mut client = Client::start(&driver, &root, &root.join("cache"));
    client.request(
        1,
        "initialize",
        json!({"workspaceFolders": [folder(&workspace)]}),
    );
    assert!(client.response(1)["result"].is_object());
    client.open(&path, source);
    assert!(has_rule(&client.diagnostics(&path, Some(1)), "LANG-11"));

    write(
        &manifest_path,
        &initial.replace("local/app", "local/changed"),
    );
    client.watch(&manifest_path, 2);
    let diagnostics = client.diagnostics(&path, Some(1));
    assert!(
        diagnostics.to_string().contains("hirsel lock"),
        "{diagnostics}"
    );

    cli(&driver, &root, &workspace, &["lock"], 0);
    client.watch(&lock, 2);
    assert!(has_rule(&client.diagnostics(&path, Some(1)), "LANG-11"));

    let contents = fs::read_to_string(&lock).unwrap();
    fs::remove_file(&lock).unwrap();
    client.watch(&lock, 3);
    let diagnostics = client.diagnostics(&path, Some(1));
    assert!(
        diagnostics.to_string().contains("hirsel lock"),
        "{diagnostics}"
    );
    assert!(!lock.exists());

    write(&lock, &contents);
    client.watch(&lock, 1);
    assert!(has_rule(&client.diagnostics(&path, Some(1)), "LANG-11"));
    client.shutdown(2);
}
