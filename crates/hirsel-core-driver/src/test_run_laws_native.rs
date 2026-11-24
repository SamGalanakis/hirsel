fn model_law_package(tests: &str) -> PackageLaw {
    let law = PackageLaw::new();
    law.app(
        "app",
        "[dependencies]\nstd = { major = 1 }\n\n[tests]\nlimit = 67108864",
    );
    let fixture = std::fs::read_to_string(law.libraries.join("std/tests/model.hirsel")).unwrap();
    let fixture = fixture
        .replace("model.", "std.model.")
        .replace("reproduce: std.model.replay", "reproduce: model.replay");
    law.write("app/main.hirsel", format!("{fixture}\n{tests}"));
    law
}

#[test]
fn model_law_table_reports_handler_failure() {
    let law = model_law_package(
        r#"
@test
fn c9() -> Result[(), ()] {
    if explore_handler_timeout() { Ok(()) } else { Error(()) }
}
"#,
    );
    let text = PackageLaw::pass(law.run(&["test", "app", "--laws"]));
    let rows = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("model."));
    assert_eq!(
        rows.collect::<Vec<_>>(),
        ["model.explore  explore_handler_timeout#1  failed (Handler) (per build)",],
        "{text}"
    );
}

#[test]
fn model_law_table_reports_conformance() {
    let law = model_law_package(
        r#"
@test
fn t1() -> Result[(), ()] {
    let mut b = model_bounds()
    b.states = 10
    let _ = std.model.conforms(pipe(), b, pipe_boundary(), pipe_trace())
    std.model.conforms_check(pipe(), model_bounds(), pipe_boundary(), pipe_trace())
}

@test
fn t4() -> Result[(), ()] {
    if conforms_states_cap() { Ok(()) } else { Error(()) }
}
"#,
    );
    let text = PackageLaw::pass(law.run(&["test", "app", "--write-laws", "-j", "1"]));
    let rows = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("model."));
    assert_eq!(
        rows.collect::<Vec<_>>(),
        [
            "model.conforms  t1#1  conforms (4)",
            "model.conforms_check  t1#1  conforms (4)",
            "model.conforms  conforms_states_cap#1  incomplete (1; states=2)",
        ],
        "{text}"
    );
    let lock = std::fs::read_to_string(law.path("app/hirsel.laws")).unwrap();
    let rows = lock
        .lines()
        .skip(1)
        .map(|line| line.rsplit_once(" rows=").unwrap().1);
    assert_eq!(
        rows.collect::<Vec<_>>(),
        [
            "model.conforms#1:conforms:4;model.conforms_check#1:conforms:4",
            "model.conforms@app@1/main/conforms_states_cap#1:incomplete:1",
        ],
        "{lock}"
    );

    law.write("base.laws", lock);
    let source = std::fs::read_to_string(law.path("app/main.hirsel")).unwrap();
    law.write(
        "app/main.hirsel",
        source.replace("b.states = 10", "b.states = 2"),
    );
    let (status, text, stderr) =
        law.run(&["test", "app", "--laws-baseline", "base.laws", "--json"]);
    assert_eq!(status, Some(1), "{text}\n{stderr}");
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(
        report["tests"]
            .as_array()
            .unwrap()
            .iter()
            .all(|test| test["passed"] == true)
    );
    assert_eq!(
        report["law_regressions"],
        serde_json::json!(["app@1/main/t1 model.conforms#1: incomplete 1 below conforms 4"])
    );
}
