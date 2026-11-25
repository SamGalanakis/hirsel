hirsel_guard::install!();

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn hirsel_bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(env!("CARGO_BIN_EXE_hirsel"))
        .canonicalize()
        .unwrap()
}

fn run_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(hirsel_bin())
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn hirsel_new_dot_in_existing_git_dir_creates_agents_gitignore_and_refuses_reinit() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir(repo.join(".git")).unwrap();
    std::fs::write(repo.join("README.md"), "# Demo\n").unwrap();

    let created = run_in(repo, &["new", "."]);
    assert!(
        created.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&created),
        stderr(&created)
    );
    let out = stdout(&created);
    assert!(out.contains("Created application at ."), "{out}");
    assert!(out.contains("Next steps:"), "{out}");
    assert!(out.contains("hirsel check"), "{out}");
    assert!(out.contains("hirsel run"), "{out}");
    assert!(out.contains("hirsel test"), "{out}");
    assert!(!out.contains("cd ."), "{out}");

    assert!(repo.join("hirsel.toml").is_file());
    assert!(repo.join("hirsel.lock").is_file());
    assert!(repo.join("main.hirsel").is_file());
    assert!(repo.join("tests.hirsel").is_file());
    assert!(repo.join(".gitignore").is_file());
    assert!(repo.join("AGENTS.md").is_file());
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "# Demo\n"
    );

    let refused = run_in(repo, &["new", "."]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(
        stderr(&refused).contains("hirsel.toml"),
        "stderr:\n{}",
        stderr(&refused)
    );

    let subdir = run_in(repo, &["new", "subapp"]);
    assert!(
        subdir.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&subdir),
        stderr(&subdir)
    );
    assert!(stdout(&subdir).contains("cd subapp"), "{}", stdout(&subdir));
}

#[test]
fn hirsel_inspect_std_module_json_and_filter_return_docs_and_examples() {
    let work = tempfile::tempdir().unwrap();

    let text_out = run_in(
        work.path(),
        &["inspect", "--std", "--module", "json", "--interface"],
    );
    assert!(text_out.status.success(), "stderr:\n{}", stderr(&text_out));
    let text = stdout(&text_out);
    assert!(text.contains("std@1/json/parse\tpub fn parse("), "{text}");
    assert!(text.contains("  /// Parses s into one document"), "{text}");
    assert!(text.contains("  /// Ex: `region r {"), "{text}");
    assert!(
        !text.contains("std@1/http/"),
        "module filter leaked http declarations"
    );

    let json_out = run_in(
        work.path(),
        &[
            "inspect",
            "--std",
            "--module",
            "std.json",
            "--interface",
            "--json",
        ],
    );
    assert!(json_out.status.success(), "stderr:\n{}", stderr(&json_out));
    let parsed: Value = serde_json::from_str(&stdout(&json_out)).unwrap();
    let modules = parsed["modules"].as_array().unwrap();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0]["path"], serde_json::json!(["json"]));
    let items = modules[0]["items"].as_array().unwrap();
    let parse_item = items.iter().find(|i| i["name"] == "parse").unwrap();
    let parse_doc = parse_item["doc"].as_str().unwrap();
    assert!(
        parse_doc.contains("Parses s into one document"),
        "{parse_doc}"
    );
    assert!(parse_doc.contains("Ex: `region r {"), "{parse_doc}");
    let json_type = items.iter().find(|i| i["name"] == "Json").unwrap();
    assert!(json_type.get("doc").is_none(), "{json_type}");

    let filtered_out = run_in(
        work.path(),
        &[
            "inspect",
            "--std",
            "--module",
            "json",
            "--filter",
            "128-container",
            "--interface",
            "--json",
        ],
    );
    assert!(
        filtered_out.status.success(),
        "stderr:\n{}",
        stderr(&filtered_out)
    );
    let filtered: Value = serde_json::from_str(&stdout(&filtered_out)).unwrap();
    let filtered_items = filtered["modules"][0]["items"].as_array().unwrap();
    assert_eq!(filtered_items.len(), 1);
    assert_eq!(filtered_items[0]["name"], "size");

    let missing = run_in(
        work.path(),
        &[
            "inspect",
            "--std",
            "--module",
            "nonexistent_mod",
            "--interface",
        ],
    );
    assert_eq!(missing.status.code(), Some(2));
    assert!(
        stderr(&missing).contains("unknown module `nonexistent_mod`"),
        "{}",
        stderr(&missing)
    );
}

#[test]
fn hirsel_explain_module_hint_and_rule_canonical_url() {
    let work = tempfile::tempdir().unwrap();

    let json_mod = run_in(work.path(), &["explain", "json"]);
    assert!(json_mod.status.success(), "stderr:\n{}", stderr(&json_mod));
    let text = stdout(&json_mod);
    assert!(
        text.contains("hirsel inspect --std --module json --interface"),
        "{text}"
    );

    let json_mod_structured = run_in(work.path(), &["explain", "json", "--json"]);
    assert!(
        json_mod_structured.status.success(),
        "stderr:\n{}",
        stderr(&json_mod_structured)
    );
    let payload: Value = serde_json::from_str(&stdout(&json_mod_structured)).unwrap();
    assert_eq!(payload["module"], "json");
    assert_eq!(
        payload["commands"],
        serde_json::json!(["hirsel inspect --std --module json --interface"])
    );

    let rule_out = run_in(work.path(), &["explain", "LANG-7"]);
    assert!(rule_out.status.success(), "stderr:\n{}", stderr(&rule_out));
    let rule_text = stdout(&rule_out);
    assert!(rule_text.contains("docs/spec/"), "{rule_text}");
    assert!(
        rule_text.contains("https://github.com/SamGalanakis/hirsel/blob/"),
        "{rule_text}"
    );
}

#[test]
fn hirsel_version_and_env_report_full_version_and_docs_url() {
    let work = tempfile::tempdir().unwrap();

    let ver = run_in(work.path(), &["--version"]);
    assert!(ver.status.success(), "stderr:\n{}", stderr(&ver));
    let out = stdout(&ver);
    let lines: Vec<_> = out.lines().collect();
    assert!(lines.len() >= 3, "{out}");
    assert_eq!(lines[0], "hirsel 0.1.0-pre");
    assert!(lines[1].starts_with("compiler "), "{out}");
    assert_eq!(lines[2], "docs https://hirsel.ai");

    let env_json = run_in(work.path(), &["env", "--json"]);
    assert!(env_json.status.success(), "stderr:\n{}", stderr(&env_json));
    let report: Value = serde_json::from_str(&stdout(&env_json)).unwrap();
    assert_eq!(report["version"], "0.1.0-pre");
    assert_eq!(report["docs"], "https://hirsel.ai");
}
