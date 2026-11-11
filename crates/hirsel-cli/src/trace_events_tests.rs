use super::*;

fn args(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

#[test]
fn trace_events_parses_native_destination_and_keeps_application_arguments() {
    let command = parse_command(args(&[
        "run",
        "--native",
        "program.hir",
        "--trace-events=events.jsonl",
        "--",
        "--trace-events",
        "app.jsonl",
    ]))
    .unwrap();
    let Command::Run {
        native,
        trace_events,
        arguments,
        ..
    } = command
    else {
        panic!("native run")
    };
    assert!(native);
    assert_eq!(trace_events, Some("events.jsonl".into()));
    assert_eq!(arguments, Some(args(&["--trace-events", "app.jsonl"])));
    assert!(command_help("run").unwrap().contains("--trace-events"));
}

#[test]
fn trace_events_rejects_interpreter_missing_path_and_duplicate_selections() {
    for words in [
        vec!["run", "program.hir", "--trace-events", "events.jsonl"],
        vec!["run", "--native", "program.hir", "--trace-events"],
        vec![
            "run",
            "--native",
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

#[test]
fn a_served_native_head_builds_with_llvm_unless_cranelift_is_asked_for() {
    let head = |words: &[&str]| match parse_command(args(words)).unwrap() {
        Command::Serve { native, .. } => native,
        _ => panic!("serve"),
    };
    let host = ["program.hir", "--host", "host.json"];
    let with = |flags: &[&'static str]| [flags, &host[..]].concat();
    // LLVM is what the driver builds a place head with the release profile for.
    assert_eq!(
        head(&with(&["serve", "--native"])),
        Some(hirsel_native::Backend::Llvm)
    );
    assert_eq!(
        head(&with(&["serve", "--native", "--backend", "cranelift"])),
        Some(hirsel_native::Backend::Cranelift)
    );
    assert_eq!(head(&with(&["serve"])), None);
}
