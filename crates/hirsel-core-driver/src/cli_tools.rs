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

/// docs/PRIMER.md, which `hirsel primer` and `hirsel explain primer` print.
pub const PRIMER: &str = include_str!("../../../docs/PRIMER.md");

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
        let statement = "--debug-lines on|off controls native line tables for build, run and test. Line tables default to off on every profile and both engines; pass --debug-lines on to opt in. It changes native object and link cache identities. Failure-site symbols, current source tables and file:line:column failure reports are always enabled. A line-only edit recompiles no existing native object; with rows on, the link-time writer rebinds current debug coordinates.";
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
            println!(
                "{}: {}\n\n{}\n\n{}#{}",
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
            "listen" | "connect" | "accept" | "recv_bounded" | "send_pair"
        )
    {
        return Err(format!(
            "unknown rule `{name}`; the socket operation is `host.{operation}`. See `hirsel inspect --core --interface` or `hirsel explain NET-1`"
        ).into());
    }

    let module = match name {
        "Args" => "args",
        "Bytes" => "core",
        _ => name.strip_prefix("std.").unwrap_or(name),
    };
    let core = SOURCES.iter().any(|(path, _)| *path == module);
    let std = STD_SOURCES.iter().any(|(path, _)| *path == module);
    if !core && !std {
        return Err(format!(
            "unknown rule `{name}`; use `hirsel explain --list` for rule IDs or `hirsel inspect --core --interface` and `hirsel inspect --std --interface` for declarations"
        ).into());
    }
    let commands: Vec<_> = [
        (core, "hirsel inspect --core --interface"),
        (std, "hirsel inspect --std --interface"),
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
