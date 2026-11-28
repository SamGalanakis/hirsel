use super::{COMMANDS, CommandSpec, HELP};

pub(crate) fn named(name: &str) -> Result<&'static CommandSpec, String> {
    COMMANDS
        .iter()
        .find(|c| c.name == name)
        .ok_or_else(|| format!("unknown command `{name}`; use help"))
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
        if matches!(
            command.name,
            "config" | "op" | "prof" | "lean" | "sdk" | "fleet" | "store" | "tape"
        ) {
            let prefix = format!("{} ", command.name);
            for subcommand in COMMANDS.iter().filter(|c| c.name.starts_with(&prefix)) {
                result.push_str(&format!(
                    "  {}\n    {}\n",
                    subcommand.usage, subcommand.description
                ));
            }
            return result;
        }
        command.schema.write_help(&mut result);
        if command.name == "host" {
            result.push_str("\nGroup recovery commands:\n");
            for child in COMMANDS.iter().filter(|c| c.name.starts_with("host ")) {
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
            ReadContract::Deploy | ReadContract::Decision | ReadContract::Restore => "submit",
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
    }
    spec
}
