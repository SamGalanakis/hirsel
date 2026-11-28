use super::*;

#[test]
fn package_edit_plans_match_applied_files() {
    let law = PackageLaw::new();
    law.fixture_json();
    law.app("app", "");
    copy(&law, "vendor/jx.hpkg", "app/json.hpkg");
    let snapshot = || {
        ["hirsel.hcfg", "hirsel.lock"].map(|file| fs::read(law.path(&format!("app/{file}"))).ok())
    };
    let run = |command, extra: &[&str], code| {
        let args: Vec<_> = extra.iter().copied().chain(["--json"]).collect();
        let (status, stdout, stderr) = result(
            invocation_command(&law, "app", command, &args)
                .output()
                .unwrap(),
        );
        assert_eq!(status, Some(code), "{stdout}\n{stderr}");
        let receipt: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(receipt["v"], 1);
        assert_eq!(receipt["command"], command);
        assert_eq!(receipt["exit"], code);
        assert!(receipt["next"].is_array());
        receipt
    };

    let check = |command, operand, version, root, old_root| {
        let before = snapshot();
        let preview = run(command, &[operand, "--plan"], 0);
        assert_eq!(preview["contract"], "plan");
        assert_eq!(preview["outcome"]["effect"], "none");
        let plan = &preview["result"]["plan"];
        assert_eq!(plan["bindings"][0]["after"]["root"], root);
        assert_eq!(plan["bindings"][0]["before"]["root"], old_root);
        assert_eq!(
            plan["bindings"][0]["after"]["authority"],
            serde_json::json!([])
        );
        assert_eq!(
            plan["locks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"] == "fixture.test/json@1")
                .unwrap()["after"]["version"],
            version
        );
        assert_eq!(
            plan["locks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"] == "fixture.test/json@1")
                .unwrap()["after"]["root"],
            root
        );
        assert_eq!(snapshot(), before);
        let applied = run(command, &[operand], 0);
        assert_eq!(applied["contract"], "submit");
        assert_eq!(applied["outcome"]["effect"], "committed");
        assert_eq!(applied["result"]["plan"], *plan);
        assert_eq!(
            applied["result"]["plan_digest"],
            preview["result"]["plan_digest"]
        );
        let after = snapshot();
        for (i, file) in ["hirsel.hcfg", "hirsel.lock"].iter().enumerate() {
            let change = plan["files"]
                .as_array()
                .unwrap()
                .iter()
                .find(|change| change["path"] == *file)
                .unwrap();
            use sha2::{Digest, Sha256};
            let digest = |bytes: &[u8]| {
                format!(
                    "sha256:{}",
                    hirsel_core_ids::hex::encode(Sha256::digest(bytes).as_slice())
                )
            };
            assert_eq!(
                change["before"],
                before[i]
                    .as_deref()
                    .map(digest)
                    .map_or(serde_json::Value::Null, serde_json::Value::String)
            );
            assert_eq!(change["after"], digest(after[i].as_deref().unwrap()));
        }
        preview
    };
    let added = check(
        "add",
        "json.hpkg",
        "1.0.0",
        PACKAGE_X,
        serde_json::Value::Null,
    );
    assert_eq!(
        added["result"]["plan"]["exports"]["fixture.test/json@1"]["json/version"]["change"],
        "added"
    );
    copy(&law, "vendor/jy.hpkg", "app/json.hpkg");
    check(
        "update",
        "json",
        "1.0.1",
        PACKAGE_Y,
        serde_json::json!(PACKAGE_X),
    );

    for (command, operand) in [("add", "json.hpkg"), ("update", "json")] {
        let before = snapshot();
        let invalid = run(command, &[operand, "--dry-run"], 2);
        assert_eq!(invalid["error"]["code"], "InvalidInput");
        assert_eq!(snapshot(), before);
    }
}
