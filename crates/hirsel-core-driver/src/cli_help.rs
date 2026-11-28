use super::{COMMANDS, CommandSpec, HELP, Positionals};

pub(crate) fn named(name: &str) -> Result<&'static CommandSpec, String> {
    COMMANDS
        .iter()
        .find(|c| c.name == name)
        .ok_or_else(|| format!("unknown command `{name}`; use help"))
}

pub(super) fn children(
    command: &CommandSpec,
) -> impl Iterator<Item = (&'static str, &'static CommandSpec)> {
    COMMANDS.iter().filter_map(move |child| {
        let suffix = child.name.strip_prefix(command.name)?.strip_prefix(' ')?;
        (!suffix.contains(' ')).then_some((suffix, child))
    })
}

pub(super) fn is_help(name: &str) -> bool {
    HELP.names.contains(&name)
}

pub fn help(command: Option<&CommandSpec>) -> String {
    if let Some(command) = command {
        let mut result = format!(
            "Usage: hirsel {}\n\n{}\n\nOptions:\n",
            command.usage, command.description
        );
        command.schema.write_help(&mut result);
        if children(command).next().is_some() {
            result.push_str("\nSubcommands:\n");
            for (_, child) in children(command) {
                result.push_str(&format!("  {}\n    {}\n", child.usage, child.description));
            }
        }
        result
    } else {
        let mut result =
            "Usage: hirsel <command> [options]\n       hirsel --version\n\nCommands:\n".to_owned();
        for command in COMMANDS.iter().filter(|c| !c.name.contains(' ')) {
            result.push_str(&format!("  {:<16} {}\n", command.name, command.description));
        }
        result.push_str("\nUse help <command>, <command> --help, or <command> -h for options.\n");
        result
    }
}

pub(crate) fn describe(command: &CommandSpec) -> serde_json::Value {
    use super::types::ReadContract;
    use serde_json::json;
    let mut spec = command.schema.describe();
    spec["name"] = json!(command.name);
    spec["usage"] = json!(command.usage);
    spec["description"] = json!(command.description);
    spec["subcommands"] = json!(children(command).map(|(name, _)| name).collect::<Vec<_>>());
    let json_option = spec["options"].as_array().unwrap().iter().any(|option| {
        option["names"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "--json")
    });
    spec["formats"] = match command.name {
        "lsp" => json!(["lsp"]),
        "config export" => json!(["json"]),
        _ if json_option => json!(["text", "json"]),
        _ => json!(["text"]),
    };
    if let Some(contract) = command.contract {
        let (target, authority, predicate, receipt) = match contract {
            ReadContract::Deploy | ReadContract::Decision => (
                "host",
                "operator_uid",
                "input owned, decision required, or terminal outcome",
                true,
            ),
            ReadContract::Retirement => (
                "host",
                "operator_uid",
                "exact incarnation held for decision",
                true,
            ),
            ReadContract::Archive => (
                "archive",
                "read credential",
                "retained archive observed",
                false,
            ),
            ReadContract::Expiry => (
                "archive",
                "expiry credential",
                "retention floor planned",
                true,
            ),
            ReadContract::Restore => (
                "store",
                "directory grants",
                "exact restore boundary selected",
                true,
            ),
            ReadContract::PackageEdit => (
                "workspace",
                "local",
                "package edit planned, applied, refused or held",
                true,
            ),
            ReadContract::Help => ("none", "local", "command schema read", false),
            ReadContract::Workloads => ("host", "operator_uid", "group page read", false),
            ReadContract::Workload => ("host_or_dev", "operator_uid", "serving state read", false),
            ReadContract::Lifecycle => (
                "host",
                "operator_uid",
                "page read, interruption or cursor gap",
                false,
            ),
            ReadContract::DevStream => (
                "dev",
                "operator_uid",
                "edit attempts and session end",
                false,
            ),
            ReadContract::DevWait => (
                "dev",
                "operator_uid",
                "selected edit completes or is superseded",
                false,
            ),
            ReadContract::DevStop => ("dev", "operator_uid", "session stops", false),
            ReadContract::Rehearsal => ("store", "local", "migration preview completed", false),
            ReadContract::TapeCheck => {
                ("store", "local", "retained journal interval checked", false)
            }
            ReadContract::Execute => ("local_or_host_pin", "local", "guest or replay ended", false),
            ReadContract::TapeExport => ("file", "local", "private export written", false),
            ReadContract::Operation => ("host", "operator_uid", "catalog row read", true),
        };
        let mode = match contract {
            ReadContract::Deploy
            | ReadContract::Decision
            | ReadContract::Restore
            | ReadContract::PackageEdit => "submit",
            ReadContract::TapeExport
            | ReadContract::DevStream
            | ReadContract::Execute
            | ReadContract::Rehearsal
            | ReadContract::TapeCheck => "execute",
            ReadContract::DevWait | ReadContract::DevStop => "wait",
            ReadContract::Retirement | ReadContract::Expiry => "plan",
            _ => "read",
        };
        let excludes: &[&[&str]] = if command.name == "deploy" {
            &[&["DIR", "--recipe"], &["--source", "--recipe"]]
        } else {
            &[]
        };
        spec["result"] = json!({"type":"finite", "v":1, "contract":mode,
            "target_kind":target, "authority":authority, "completion":predicate,
            "receipt":receipt, "credential_bearing":false, "mutually_exclusive":excludes});
        if matches!(
            contract,
            ReadContract::Deploy | ReadContract::Decision | ReadContract::Operation
        ) {
            spec["result"]["--wait"] = json!({"contract":"wait","completion":"installed, failed, undecided, decision required, deadline or interrupt"});
        }
        if matches!(contract, ReadContract::Lifecycle | ReadContract::DevStream) {
            spec["result"]["type"] = json!("ndjson");
            spec["result"]["records"] = json!(["snapshot", "event", "gap", "end"]);
        }
        if matches!(contract, ReadContract::Workload) {
            spec["result"]["--ready"] = json!({"contract":"wait","completion":"fresh serving proof, known unready or indeterminate"});
        }
        if matches!(contract, ReadContract::TapeExport) {
            spec["result"]["credential_bearing"] = json!(true);
            spec["result"]["required"] = json!(["TAPE", "--include-credentials", "--out"]);
        }
        if matches!(contract, ReadContract::Deploy | ReadContract::Restore) {
            spec["result"]["--plan"] =
                json!({"contract":"plan","completion":"held plan","effect":"none"});
        }
        if matches!(contract, ReadContract::Decision) {
            spec["result"]["package_receipt"] = json!({"target_kind":"workspace", "authority":"local",
                "required":["@RECEIPT.json"],"options":["--decision","--json"],"contract":"submit"});
        }
        if matches!(contract, ReadContract::PackageEdit) {
            spec["result"]["--plan"] = json!({"contract":"plan","completion":"exact manifest and lock changes resolved","effect":"none"});
            spec["result"]["exit_codes"] = json!([0, 1, 2, 75]);
            spec["result"]["payload"] = json!({"variants":["Planned", "Applied", "Unchanged", "AwaitDecision", "Declined", "Refused", "Invalid", "Blocked"],
                "fields":["change", "plan", "plan_digest", "action", "intent", "committed", "pending", "evidence"]});
        }
    } else {
        spec["result"] = match command.name {
            "lsp" => json!({"type":"stream","protocol":"lsp","transport":"stdio"}),
            "host" | "launcher" => json!({"type":"stream","protocol":"process"}),
            "serve" => json!({"type":"stream","protocol":"process", "--stop":{"type":"finite"}}),
            "config export" => {
                json!({"type":"finite","contract":"read","required":["FILE","--json"],"wire":"config"})
            }
            "registry publish" => json!({"type":"finite","contract":"submit"}),
            "build" => {
                json!({"type":"finite","wire":"compiler","--emit recipe":{"type":"finite","v":1,"contract":"execute"}})
            }
            _ if matches!(command.schema.positionals(), Positionals::Namespace) => {
                json!({"type":"help","v":1})
            }
            _ => json!({"type":"finite"}),
        };
    }
    spec
}
