//! User formatting and explanations call the canonical syntax/spec libraries.
use crate::diagnostics::render_span;
use hirsel_core_syntax::{
    Diagnostic, FormatAction, ParseMode, ResourceRefusal, Symbols, format_files, format_package,
    format_verified, parse_bytes,
};
use std::io::{self, Read, Write};
use std::path::Path;

pub fn format(path: &Path, action: FormatAction, mode: ParseMode) -> io::Result<i32> {
    let report = if path.metadata()?.is_dir() {
        format_package(path, mode, action)?
    } else {
        format_files(&[(path.to_owned(), mode)], action)?
    };
    if report.files.is_empty() {
        return Err(io::Error::other(format!(
            "no .hirsel sources in {}",
            path.display()
        )));
    }
    for path in &report.differing {
        println!("{}", path.display());
    }
    let mut resource = false;
    for refusal in &report.refused {
        let bytes = std::fs::read(&refusal.path)?;
        print_format_refusal(
            &refusal.path,
            &bytes,
            &refusal.diagnostics,
            refusal.resource_refusal.as_ref(),
        );
        resource |= refusal.resource_refusal.is_some();
    }
    Ok(if resource {
        2
    } else {
        i32::from(
            !report.refused.is_empty()
                || action == FormatAction::Check && !report.differing.is_empty(),
        )
    })
}

pub fn format_stdin(action: FormatAction, mode: ParseMode) -> io::Result<i32> {
    let mut bytes = Vec::new();
    io::stdin().lock().read_to_end(&mut bytes)?;
    let mut symbols = Symbols::default();
    let parsed = parse_bytes(&bytes, mode, &mut symbols);
    let Some(output) = format_verified(&parsed, mode, &mut symbols)? else {
        print_format_refusal(
            Path::new("<stdin>"),
            &bytes,
            &parsed.diagnostics,
            parsed.resource_refusal.as_ref(),
        );
        return Ok(if parsed.resource_refusal.is_some() {
            2
        } else {
            1
        });
    };
    if action == FormatAction::Check {
        return Ok(i32::from(output.as_bytes() != bytes));
    }
    io::stdout().lock().write_all(output.as_bytes())?;
    Ok(0)
}

fn print_format_refusal(
    path: &Path,
    bytes: &[u8],
    diagnostics: &[Diagnostic],
    resource: Option<&ResourceRefusal>,
) {
    for diagnostic in diagnostics {
        let site = render_span(path, bytes, diagnostic.primary);
        eprintln!("{site}: {}: {}", diagnostic.rule, diagnostic.message);
    }
    if let Some(error) = resource {
        let site = render_span(path, bytes, error.site());
        eprintln!("{site}: resource: {error}");
    }
}

/// docs/PRIMER.md, which `hirsel explain primer` prints.
const PRIMER: &str = include_str!("../../../docs/PRIMER.md");

pub fn explain(
    rule: Option<&str>,
    list: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if rule == Some("primer") && !list {
        if json {
            println!("{}", serde_json::json!({"name": "primer", "text": PRIMER}));
        } else {
            print!("{PRIMER}");
        }
        return Ok(());
    }
    if rule == Some("debug-lines") && !list {
        let statement = "--debug-lines on|off controls native line tables for build, run and test. Line tables default to off on every profile and both engines; pass --debug-lines on to opt in. Object emission is the same with either setting; the choice changes the executable link cache identity. Failure-site symbols, current source tables and file:line:column failure reports are always enabled. A line-only edit recompiles no existing native object; with rows on, the link-time writer rebinds current debug coordinates.";
        if json {
            println!(
                "{}",
                serde_json::json!({"option":"--debug-lines","default_dev":"off","default_release":"off","statement":statement})
            );
        } else {
            println!("{statement}");
        }
        return Ok(());
    }
    if list {
        if rule.is_some() {
            return Err("explain --list takes no rule".into());
        }
        if json {
            println!("{}", serde_json::to_string(hirsel_core_diag::rules())?);
        } else {
            for rule in hirsel_core_diag::rules() {
                println!("{}: {}", rule.id, rule.title);
            }
        }
    } else {
        let id = rule.ok_or("explain requires a rule or --list")?;
        let Some(rule) = hirsel_core_diag::rule(id) else {
            return explain_library(id, json);
        };
        let example = hirsel_core_diag::example(rule.id);
        if json {
            #[derive(serde::Serialize)]
            struct Explanation {
                #[serde(flatten)]
                rule: &'static hirsel_core_diag::Rule,
                #[serde(skip_serializing_if = "Option::is_none")]
                example: Option<&'static hirsel_core_diag::Example>,
            }
            println!("{}", serde_json::to_string(&Explanation { rule, example })?);
        } else {
            let spec_url = crate::installation::spec_url(rule.spec_file, rule.anchor);
            println!(
                "{}: {}\n\n{}\n\n{}#{}\n{spec_url}",
                rule.id, rule.title, rule.statement, rule.spec_file, rule.anchor
            );
            if let Some(example) = example {
                println!(
                    "\nOne example ({})\n\n{}",
                    example.path,
                    example.source.trim_end()
                );
                println!("\nAsserted refusals\n\n{}", example.assertions.join("\n"));
            }
        }
    }
    Ok(())
}

fn explain_library(name: &str, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    use crate::product::bundled::{SOURCES, STD_SOURCES};

    if let Some(operation) = name.strip_prefix("net.")
        && matches!(
            operation,
            "listen" | "connect" | "accept" | "recv_bounded" | "recv_ready" | "send_pair"
        )
    {
        return Err(format!(
            "unknown rule `{name}`; the socket operation is `host.{operation}`. See `hirsel inspect --core --module host --interface` or `hirsel explain NET-1`"
        ).into());
    }

    let normalized = match name {
        "Args" => "args".to_owned(),
        "Bytes" => "core".to_owned(),
        _ => name
            .strip_prefix("std.")
            .or_else(|| name.strip_prefix("std/"))
            .or_else(|| name.strip_prefix("core."))
            .or_else(|| name.strip_prefix("core/"))
            .unwrap_or(name)
            .replace('.', "/"),
    };
    let module = normalized.as_str();
    let prefix = format!("{module}/");
    let core = SOURCES
        .iter()
        .any(|(path, _)| *path == module || path.starts_with(&prefix));
    let std = STD_SOURCES
        .iter()
        .any(|(path, _)| *path == module || path.starts_with(&prefix));
    if !core && !std {
        return Err(format!(
            "unknown rule `{name}`; use `hirsel explain --list` for rule IDs or `hirsel inspect --core --interface` and `hirsel inspect --std --module <name> --interface` (or `--filter <text>`) for declarations"
        ).into());
    }
    let commands: Vec<String> = [
        (
            core,
            format!("hirsel inspect --core --module {module} --interface"),
        ),
        (
            std,
            format!("hirsel inspect --std --module {module} --interface"),
        ),
    ]
    .into_iter()
    .filter_map(|(present, command)| present.then_some(command))
    .collect();
    let rule = match module {
        "net" => Some("NET-1"),
        "args" => Some("PROG-3"),
        _ => None,
    };
    if json {
        println!(
            "{}",
            serde_json::json!({"module": module, "commands": commands, "rule": rule})
        );
    } else {
        println!("Declarations for `{name}`");
        for command in commands {
            println!("  {command}");
        }
        if let Some(rule) = rule {
            println!("  hirsel explain {rule}");
        }
    }
    Ok(())
}

const SCAFFOLD_GITIGNORE: &str = "program\n.hirsel-core/\n.hirsel-debug/\n";

const SCAFFOLD_AGENTS_MD: &str = "\
# Hirsel project

Hirsel is a new language—do not guess Rust, Zig, or Go syntax.

## Before editing

- Run `hirsel primer` for the full language primer matching this installed binary (trust it over web docs).
- Never guess library names or signatures; query them with:
  - `hirsel inspect --std --module <name> --interface` (or `--filter <text>`)
  - `hirsel inspect --core --interface`

## Edit and verify loop

1. Check with `hirsel check --json` (returns rule IDs, spans, causal sites, and machine-applicable `fixes`).
2. Read the exact rule with `hirsel explain <RULE>` (or `hirsel explain --list`); fix to the rule, not the message wording.
3. Format with `hirsel fmt .`, test with `hirsel test`, and run with `hirsel run` (pass `--grant KIND` / `--dir PATH` for capabilities).

## Key language rules

- No imports, methods, traits, or turbofish: call modules by path (`list.push(mut xs, 1)`).
- Repeat `mut` and `take` modes at call sites; unmarked parameters are views.
- Annotate `let` bindings of literals or generic calls (`let n: u64 = 0`); never shadow names.
- Pass capabilities (`Stdout`, `Clock`, `Dir`, `Net`) as parameters; mark functions `waits` iff they call something that waits.
";

/// Scaffold a Hirsel package into a new or existing directory without clobbering existing files.
pub fn scaffold(directory: &Path, library: bool) -> std::io::Result<()> {
    use std::io::Write;
    match std::fs::metadata(directory) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} already exists and is not a directory",
                    directory.display()
                ),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir_all(directory)?;
        }
        Err(error) => return Err(error),
    }
    let main = if library {
        "pub fn main() -> Result[(), Exit] {\n    if lib.answer() == 42 {\n        Ok(())\n    } else {\n        Error(Exit(status: 1))\n    }\n}\n"
    } else {
        "pub fn main() -> Result[(), Exit] {\n    Ok(())\n}\n\npub fn twice(n: u64) -> u64 {\n    n + n\n}\n"
    };
    let lib = "pub fn answer() -> u64 {\n    42\n}\n";
    let test = if library {
        "@test\nfn test_answer() -> Result[(), ()] {\n    if lib.answer() == 42 {\n        Ok(())\n    } else {\n        Error(())\n    }\n}\n"
    } else {
        "@test\nfn test_twice() -> Result[(), ()] {\n    if main.twice(21) == 42 {\n        Ok(())\n    } else {\n        Error(())\n    }\n}\n"
    };
    let sources = ["main.hirsel"]
        .into_iter()
        .chain(library.then_some("lib.hirsel"));
    let manifest = crate::package_dir::scaffold_manifest(sources);
    let required: Vec<(&str, &str)> = [
        (crate::package_dir::MANIFEST, manifest.as_str()),
        ("main.hirsel", main),
        ("tests.hirsel", test),
    ]
    .into_iter()
    .chain(library.then_some(("lib.hirsel", lib)))
    .collect();
    for (name, _) in &required {
        if directory.join(name).exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "refusing to overwrite existing `{name}` in {}",
                    directory.display()
                ),
            ));
        }
    }
    for (name, source) in required {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(name))?
            .write_all(source.as_bytes())?;
    }
    for (name, content) in [
        (".gitignore", SCAFFOLD_GITIGNORE),
        ("AGENTS.md", SCAFFOLD_AGENTS_MD),
    ] {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(name))
        {
            Ok(mut file) => file.write_all(content.as_bytes())?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(crate) fn print_scaffold_summary(directory: &str, library: bool) {
    println!(
        "Created {} at {directory}",
        if library {
            "library with example main"
        } else {
            "application"
        }
    );
    println!("\nNext steps:");
    if directory != "." && directory != "./" {
        println!("  cd {directory}");
    }
    println!("  hirsel check");
    println!("  hirsel run");
    println!("  hirsel test");
}

#[cfg(test)]
#[path = "cli_dx_tests.rs"]
mod cli_dx_tests;
