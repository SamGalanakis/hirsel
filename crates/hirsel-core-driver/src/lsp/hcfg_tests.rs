use super::{Client, capture, cli, driver, file_uri, has_rule, manifest, point, write};
use serde_json::json;
use std::fs;

#[test]
fn hcfg_queries_use_the_schema_and_unsaved_data_formatter() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = root.join("hirsel.hcfg");
    let mut client = Client::start(&driver(), &root, &root.join("cache"));
    client.request(1, "initialize", json!({}));
    assert!(client.response(1)["result"].is_object());
    let source = manifest("local/😀", "main");
    client.open(&path, &source);
    let at = |line: &str, needle: &str, advance: usize| {
        json!({
            "textDocument": {"uri": file_uri(&path)},
            "position": point(line, line.find(needle).unwrap() + advance, 0, "utf-16"),
        })
    };
    client.request(2, "textDocument/hover", at(&source, "version", 1));
    let hover = client.response(2)["result"]["contents"]["value"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        hover.contains("String") && hover.contains("Required"),
        "{hover}"
    );

    let keys = "pkg_v1.Package(name: \"local/app\", ";
    client.change(&path, 2, json!([{"text": keys}]));
    client.request(3, "textDocument/completion", at(keys, ", ", 2));
    let result = client.response(3);
    let labels = result["result"].as_array().unwrap();
    assert!(
        labels.iter().any(|item| item["label"] == "version"),
        "{result}"
    );
    assert!(
        labels.iter().all(|item| item["label"] != "name"),
        "{result}"
    );

    let cases = "pkg_v1.Package(charge: Ch";
    client.change(&path, 3, json!([{"text": cases}]));
    client.request(4, "textDocument/completion", at(cases, "Ch", 2));
    let result = client.response(4);
    assert_eq!(result["result"][0]["label"], "ChargeV1", "{result}");

    client.change(&path, 4, json!([{"text": "user_v1.Config(  )"}]));
    client.request(
        5,
        "textDocument/formatting",
        json!({"textDocument": {"uri": file_uri(&path)}}),
    );
    let result = client.response(5);
    assert_eq!(
        result["result"][0]["newText"], "user_v1.Config()\n",
        "{result}"
    );
    client.shutdown(6);
}

#[test]
fn hcfg_manifest_overlay_controls_workspace_checks_and_exact_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    let path = workspace.join("hirsel.hcfg");
    let program = workspace.join("main.hirsel");
    let source = "fn broken() -> u64 { true }\n";
    let valid = manifest("local/app", "main");
    write(&path, &valid);
    write(&program, source);
    cli(&driver(), &root, &workspace, &["lock"], 0);
    fs::remove_file(&path).unwrap();
    let before = capture(&workspace);

    let mut client = Client::start(&driver(), &root, &root.join("cache"));
    client.request(1, "initialize", json!({}));
    assert!(client.response(1)["result"].is_object());
    client.open(&path, &valid);
    client.open(&program, source);
    assert_eq!(client.diagnostics(&path, Some(1)), json!([]));
    assert!(has_rule(&client.diagnostics(&program, Some(1)), "LANG-11"));

    let other_source = valid.replace("\"main.hirsel\"", "\"absent.hirsel\"");
    client.change(&path, 2, json!([{"text": other_source}]));
    client.diagnostics(&path, Some(2));
    let checked = client.diagnostics(&program, Some(1));
    assert!(checked.to_string().contains("absent.hirsel"), "{checked}");
    assert!(!has_rule(&checked, "LANG-11"), "{checked}");

    for (version, diagnostic, contents, needle) in [
        (3, "unknown field", valid.replace("charge: ChargeV1", "extra: ChargeV1"), "extra: ChargeV1"),
        (4, "expected scalar literal", valid.replace("\"1.0.0\"", "true"), "true"),
        (5, "expected scalar literal", valid.replace("dependencies: []", "dependencies: [(\"dep\", Toolchain(package: \"local/dep\", major: 4294967296, authority: []))]"), "4294967296"),
    ] {
        client.change(&path, version, json!([{"text": contents}]));
        let diagnostics = client.diagnostics(&path, Some(version));
        let error = &diagnostics[0];
        assert!(error["message"].as_str().unwrap().contains(diagnostic), "{diagnostics}");
        let start = contents.find(needle).unwrap();
        assert_eq!(error["range"], json!({
            "start": point(&contents, start, 0, "utf-16"),
            "end": point(&contents, start + needle.len(), 0, "utf-16"),
        }));
    }

    let syntax = valid.replace("ChargeV1", "@");
    client.change(&path, 6, json!([{"text": syntax}]));
    let errors = client.diagnostics(&path, Some(6));
    let start = syntax.find('@').unwrap();
    assert!(
        errors
            .as_array()
            .unwrap()
            .iter()
            .any(|error| { error["range"]["start"] == point(&syntax, start, 0, "utf-16") }),
        "{errors}"
    );
    client.change(&path, 7, json!([{"text": valid}]));
    assert_eq!(client.diagnostics(&path, Some(7)), json!([]));
    assert!(has_rule(&client.diagnostics(&program, Some(1)), "LANG-11"));
    client.shutdown(2);
    assert_eq!(capture(&workspace), before);
}
