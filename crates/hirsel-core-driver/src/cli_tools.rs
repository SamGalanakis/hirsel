//! User formatting and explanations call the canonical syntax/spec libraries.
use hirsel_core_syntax::{FormatAction, ParseMode, format_package};
use std::path::Path;

pub fn format(
    directory: &Path,
    check: bool,
    signatures: bool,
) -> Result<i32, Box<dyn std::error::Error>> {
    let report = format_package(
        directory,
        if signatures {
            ParseMode::Signatures
        } else {
            ParseMode::Source
        },
        if check {
            FormatAction::Check
        } else {
            FormatAction::Write
        },
    )?;
    if report.files.is_empty() {
        return Err(format!("no .hirsel sources in {}", directory.display()).into());
    }
    for path in &report.differing {
        println!("{}", path.display());
    }
    let mut resource = false;
    for refusal in &report.refused {
        for diagnostic in &refusal.diagnostics {
            eprintln!(
                "{}:{}..{}: {}: {}",
                refusal.path.display(),
                diagnostic.primary.start,
                diagnostic.primary.end,
                diagnostic.rule,
                diagnostic.message
            );
        }
        if let Some(error) = &refusal.resource_refusal {
            resource = true;
            eprintln!("{}: resource: {error}", refusal.path.display());
        }
    }
    Ok(if resource {
        2
    } else {
        i32::from(!report.refused.is_empty() || check && !report.differing.is_empty())
    })
}
pub fn explain(
    rule: Option<&str>,
    list: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if rule == Some("debug-lines") && !list {
        let statement = "--debug-lines on|off controls native line tables for build, run and test. Development defaults to on; release defaults to off on both engines. It changes native object and link cache identities. Failure-site symbols, current source tables and file:line:column failure reports are always enabled. A line-only edit recompiles no existing native object; with rows on, the link-time writer rebinds current debug coordinates.";
        if json {
            println!("{}", serde_json::json!({"option":"--debug-lines","default_dev":"on","default_release":"off","statement":statement}));
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
        let rule = hirsel_core_diag::rule(id)
            .ok_or_else(|| format!("unknown rule `{id}`; use explain --list"))?;
        if json {
            println!("{}", serde_json::to_string(rule)?);
        } else {
            println!(
                "{}: {}\n\n{}\n\n{}#{}",
                rule.id, rule.title, rule.statement, rule.spec_file, rule.anchor
            );
        }
    }
    Ok(())
}
