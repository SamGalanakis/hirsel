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
