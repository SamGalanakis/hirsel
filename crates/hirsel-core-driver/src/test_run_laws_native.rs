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
