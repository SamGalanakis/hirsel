use crate::cli::{Action, parse};
use crate::cli_tools::scaffold;
use crate::inspect::{Inspection, doc_comment};
use crate::installation::{resolve_version, spec_url_for};
use hirsel_core_syntax::{ParseMode, Symbols, parse as parse_syntax};

#[test]
fn scaffold_in_existing_git_directory_preserves_existing_files_and_refuses_clobber() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::write(root.join("README.md"), "# Existing Repo\n").unwrap();

    scaffold(root, false).unwrap();

    assert!(root.join("hirsel.hcfg").is_file());
    assert!(root.join("main.hirsel").is_file());
    assert!(root.join("tests.hirsel").is_file());
    assert_eq!(
        std::fs::read_to_string(root.join("README.md")).unwrap(),
        "# Existing Repo\n"
    );
    let gitignore = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    assert!(gitignore.contains("program\n"));
    assert!(gitignore.contains(".hirsel-core/\n"));
    assert!(gitignore.contains(".hirsel-debug/\n"));
    let agents = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
    assert!(agents.contains("hirsel primer"));
    assert!(agents.contains("hirsel check --json"));
    assert!(agents.contains("hirsel explain <RULE>"));
    assert!(agents.contains("hirsel inspect --std --module <name> --interface"));

    let err = scaffold(root, false).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(err.to_string().contains("hirsel.hcfg"), "{err}");
}

#[test]
fn scaffold_keeps_existing_gitignore_and_agents_md_and_refuses_existing_main_without_partial_writes()
 {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join(".gitignore"), "custom-ignore\n").unwrap();
    std::fs::write(root.join("AGENTS.md"), "# Custom Agents\n").unwrap();

    scaffold(root, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join(".gitignore")).unwrap(),
        "custom-ignore\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("AGENTS.md")).unwrap(),
        "# Custom Agents\n"
    );

    let dir2 = tempfile::tempdir().unwrap();
    std::fs::write(dir2.path().join("main.hirsel"), "// keep me\n").unwrap();
    let err = scaffold(dir2.path(), false).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(err.to_string().contains("main.hirsel"), "{err}");
    assert!(!dir2.path().join("hirsel.hcfg").exists());
    assert!(!dir2.path().join(".gitignore").exists());
    assert!(!dir2.path().join("AGENTS.md").exists());
    assert_eq!(
        std::fs::read_to_string(dir2.path().join("main.hirsel")).unwrap(),
        "// keep me\n"
    );
}

#[test]
fn doc_comment_extracts_attached_triple_slash_lines_and_stops_at_blank_or_regular_comment() {
    let source = "\
// Module header comment
pub type Plain = u64

/// First line of doc.
///
/// Ex: `let x = documented(1)`
pub fn documented(x: u64) -> u64 { x }

/// Detached doc comment.

pub fn detached() -> u64 { 0 }

/// Attached to attributed type.
@compact(u32)
pub type Attributed { value: u64 }
";
    let mut symbols = Symbols::default();
    let parsed = parse_syntax(source, ParseMode::Source, &mut symbols);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let docs: Vec<_> = parsed.syntax.items().map(doc_comment).collect();
    assert_eq!(docs.len(), 4);
    assert_eq!(docs[0], None);
    assert_eq!(
        docs[1].as_deref(),
        Some("First line of doc.\n\nEx: `let x = documented(1)`")
    );
    assert_eq!(docs[2], None);
    assert_eq!(docs[3].as_deref(), Some("Attached to attributed type."));
}

#[test]
fn inspect_option_validation_handles_module_and_filter() {
    let parse_inspect = |args: &[&str]| {
        let Action::Invoke(crate::cli::Invocation::Compiler(mut inv)) =
            parse(args.iter().map(|s| (*s).to_owned())).unwrap()
        else {
            panic!("expected Invoke");
        };
        Inspection::read(&mut inv)
    };
    assert!(parse_inspect(&["inspect", "--std", "--module", "json", "--interface"]).is_ok());
    assert!(parse_inspect(&["inspect", "--std", "--filter", "parse"]).is_ok());
    assert!(parse_inspect(&["inspect", "--std", "--module", "json", "--filter", "parse"]).is_ok());
    assert!(
        parse_inspect(&["inspect", ".", "--unused", "--module", "main"])
            .err()
            .unwrap()
            .contains("--module requires --interface")
    );
    assert!(
        parse_inspect(&["inspect", ".", "--unused", "--filter", "main"])
            .err()
            .unwrap()
            .contains("--filter requires --interface")
    );
}

#[test]
fn version_and_spec_url_resolution_handles_release_tags_and_prereleases() {
    assert_eq!(resolve_version(None, "0.2.0-pre"), "0.2.0-pre");
    assert_eq!(resolve_version(Some(""), "0.2.0-pre"), "0.2.0-pre");
    assert_eq!(
        resolve_version(Some("v0.2.0-pre.2"), "0.2.0-pre"),
        "0.2.0-pre.2"
    );
    assert_eq!(resolve_version(Some("0.2.0"), "0.2.0-pre"), "0.2.0");

    assert_eq!(
        spec_url_for(None, "docs/spec/resolution.md", "lang-7"),
        "https://github.com/SamGalanakis/hirsel/blob/main/docs/spec/resolution.md#lang-7"
    );
    assert_eq!(
        spec_url_for(Some("v0.2.0-pre"), "docs/spec/resolution.md", "lang-7"),
        "https://github.com/SamGalanakis/hirsel/blob/v0.2.0-pre/docs/spec/resolution.md#lang-7"
    );
    assert_eq!(
        spec_url_for(Some("0.2.0"), "docs/spec/programs.md", "prog-2"),
        "https://github.com/SamGalanakis/hirsel/blob/v0.2.0/docs/spec/programs.md#prog-2"
    );
}

#[test]
fn command_schemas_keep_documented_flag_ownership_and_defaults() {
    use crate::cli::{CompilerOption, Invocation, ServeOption};
    use hirsel_prof::MemMode;

    let parse = |args: &[&str]| parse(args.iter().map(|arg| (*arg).to_owned()));
    let Action::Invoke(Invocation::Serve(serve)) = parse(&["serve", "--stop"]).unwrap() else {
        panic!("expected the serve request");
    };
    assert_eq!(serve.options, [ServeOption::Stop]);

    let Action::Invoke(Invocation::Compiler(build)) =
        parse(&["build", "--resident", "example"]).unwrap()
    else {
        panic!("expected the build request");
    };
    assert_eq!(build.options, [CompilerOption::Resident]);
    assert_eq!(build.positional.as_deref(), Some("example"));

    for (args, expected) in [
        (
            &["build", "--stop"][..],
            "unknown option `--stop` for build; use help build",
        ),
        (
            &["serve", "--resident"][..],
            "unknown option `--resident` for serve; use help serve",
        ),
        (&["tape", "replay", "--tape"][..], "--tape requires TAPE"),
    ] {
        let error = match parse(args) {
            Err(error) => error,
            Ok(_) => panic!("accepted {args:?}"),
        };
        assert_eq!(error, expected);
    }

    let Action::Invoke(Invocation::Compiler(profile)) =
        parse(&["prof", "run", "--mem", "--", "sampled"]).unwrap()
    else {
        panic!("expected the profile request");
    };
    assert_eq!(profile.arguments, ["sampled"]);
    assert_eq!(profile.options, [CompilerOption::ProfMem(MemMode::Sampled)]);

    let Action::Invoke(Invocation::Compiler(profile)) =
        parse(&["prof", "replay", "--tape", "recording.htape", "--mem=exact"]).unwrap()
    else {
        panic!("expected the profile replay request");
    };
    assert_eq!(
        profile.options,
        [
            CompilerOption::Tape("recording.htape".into()),
            CompilerOption::ProfMem(MemMode::Exact),
        ]
    );
}

#[test]
fn slice3_rejects_retired_commands_and_declares_their_replacements() {
    let parse = |args: &[&str]| crate::cli::parse(args.iter().map(|word| (*word).to_owned()));
    for args in [
        &["publish", "--help"][..],
        &["dry-run", "--help"],
        &["primer"],
        &["tapes", "check", "--help"],
        &["tape", "show", "--help"],
        &["replay", "--help"],
        &["dev", "--wait"],
        &["dev", "--stop"],
        &["dev", "--status"],
    ] {
        assert!(parse(args).is_err(), "accepted retired form {args:?}");
    }
    for args in [
        &["build", ".", "--emit", "recipe", "--code-store", "code"][..],
        &["store", "rehearse", "state", "--release", "program"],
        &[
            "tape",
            "check",
            "state",
            "--recorded",
            "old",
            "--release",
            "new",
        ],
        &[
            "tape",
            "export",
            "run.htape",
            "--include-credentials",
            "--out",
            "private.json",
        ],
        &["tape", "replay", ".", "--tape", "run.htape"],
        &["explain", "primer"],
    ] {
        assert!(parse(args).is_ok(), "refused replacement {args:?}");
    }
}

#[test]
fn released_command_schema_covers_parser() {
    use crate::cli::{Action, COMMANDS, describe};
    use std::collections::BTreeSet;

    let mut paths = BTreeSet::new();
    for command in COMMANDS {
        assert!(
            paths.insert(command.name),
            "duplicate command {}",
            command.name
        );
        let schema = describe(command);
        assert!(
            matches!(
                schema["result"]["type"].as_str(),
                Some("help" | "finite" | "stream" | "ndjson")
            ),
            "{}",
            command.name
        );
        let words: Vec<_> = command.name.split_ascii_whitespace().collect();
        let parse_command = |extra: &[&str]| {
            crate::cli::parse(
                words
                    .iter()
                    .copied()
                    .chain(extra.iter().copied())
                    .map(str::to_owned),
            )
        };
        for flag in ["--help", "-h"] {
            let Action::Help(Some(selected)) = parse_command(&[flag, "--json"]).unwrap() else {
                panic!("{} {flag} did not select help", command.name);
            };
            assert_eq!(selected.name, schema["name"]);
        }
        assert!(
            parse_command(&["--unregistered-option"]).is_err(),
            "{}",
            command.name
        );
        let mut aliases = BTreeSet::new();
        for option in schema["options"].as_array().unwrap() {
            for name in option["names"].as_array().unwrap() {
                let name = name.as_str().unwrap();
                assert!(aliases.insert(name), "duplicate {name} in {}", command.name);
                if ["-h", "--help"].contains(&name) {
                    continue;
                }
                let mut args = Vec::new();
                if schema["operands"].as_str().unwrap().starts_with("host/") {
                    args.push(
                        "host/00000000000000000000000000000000/00000000000000000000000000000000",
                    );
                }
                args.push(name);
                match option["syntax"].as_str().unwrap() {
                    "value" => {
                        assert!(
                            parse_command(&args).is_err(),
                            "{} {name} takes a value",
                            command.name
                        );
                        args.push("example");
                    }
                    "optional_equals" => {
                        assert!(matches!(parse_command(&args), Ok(Action::Invoke(_))));
                        let assigned = format!("{name}=1s");
                        *args.last_mut().unwrap() = &assigned;
                        assert!(matches!(parse_command(&args), Ok(Action::Invoke(_))));
                        continue;
                    }
                    "optional_value" => args.push("sampled"),
                    "switch" => {}
                    other => panic!("unknown flag syntax {other}"),
                }
                assert!(
                    matches!(parse_command(&args), Ok(Action::Invoke(_))),
                    "{} {args:?}",
                    command.name
                );
            }
        }
        let children = schema["subcommands"].as_array().unwrap();
        let prefix = format!("{} ", command.name);
        let expected: BTreeSet<_> = COMMANDS
            .iter()
            .filter_map(|child| child.name.strip_prefix(&prefix))
            .filter(|rest| !rest.contains(' '))
            .collect();
        assert_eq!(
            children
                .iter()
                .map(|child| child.as_str().unwrap())
                .collect::<BTreeSet<_>>(),
            expected
        );
        let operand_counts = match schema["operands"].as_str().unwrap() {
            "namespace" => {
                assert!(matches!(parse_command(&[]), Ok(Action::Help(Some(_)))));
                assert!(parse_command(&["unregistered-subcommand"]).is_err());
                continue;
            }
            form if form.starts_with("host/") => {
                assert!(parse_command(&[]).is_err());
                assert!(parse_command(&["fail"]).is_err());
                assert_eq!(
                    parse_command(&["@file.json"]).is_ok(),
                    form.contains("@RECEIPT")
                );
                continue;
            }
            "none" => [true, false, false, false],
            "one" | "guest_tail" => [true, true, false, false],
            "pair" | "sql" => [true, true, true, false],
            "many" => [true; 4],
            other => panic!("unknown operand form {other}"),
        };
        for (n, accepted) in operand_counts.into_iter().enumerate() {
            let operands = ["p", "q", "r"];
            assert_eq!(
                parse_command(&operands[..n]).is_ok(),
                accepted,
                "{} {n} operands",
                command.name
            );
        }
        let tail = parse_command(&["--", "--unregistered-option"]);
        assert_eq!(
            tail.is_ok(),
            schema["operands"] == "guest_tail",
            "{} guest delimiter",
            command.name
        );
    }
    for command in ["add", "update"] {
        assert!(crate::cli::parse([command, "input", "--dry-run"].map(str::to_owned)).is_err());
        let schema = describe(crate::cli::named(command).unwrap());
        assert_eq!(schema["result"]["type"], "finite");
        assert_eq!(
            schema["result"]["exit_codes"],
            serde_json::json!([0, 1, 2, 75])
        );
        assert_eq!(schema["result"]["--plan"]["contract"], "plan");
    }
}
