use super::*;

fn args(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

#[test]
fn trace_events_parses_native_destination_and_keeps_application_arguments() {
    let command = parse_command(args(&[
        "run",
        "program.hir",
        "--trace-events=events.jsonl",
        "--",
        "--trace-events",
        "app.jsonl",
    ]))
    .unwrap();
    let Command::Run {
        trace_events,
        arguments,
        ..
    } = command
    else {
        panic!("native run")
    };
    assert_eq!(trace_events, Some("events.jsonl".into()));
    assert_eq!(arguments, Some(args(&["--trace-events", "app.jsonl"])));
    assert!(command_help("run").unwrap().contains("--trace-events"));
}

#[test]
fn trace_events_rejects_missing_path_and_duplicate_selections() {
    for words in [
        vec!["run", "program.hir", "--trace-events"],
        vec![
            "run",
            "program.hir",
            "--trace-events",
            "first",
            "--trace-events",
            "second",
        ],
    ] {
        assert!(parse_command(args(&words)).is_err(), "{words:?}");
    }
}
