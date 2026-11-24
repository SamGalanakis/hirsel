use super::{Describe, Error, Exported, Schema, stub_paths};
use serde_json::{Value, json};

/// Serialize the Describe schemas in the documented SDK manifest format.
pub fn manifest_json(describe: &Describe) -> Result<String, Error> {
    render(describe, &stub_paths(describe)?)
}

pub(super) fn render(describe: &Describe, paths: &[Vec<String>]) -> Result<String, Error> {
    let entries = describe
        .exported
        .iter()
        .zip(paths)
        .map(|(entry, path)| entry_json(entry, path))
        .collect::<Result<Vec<_>, _>>()?;
    let manifest = json!({
        "version": 1,
        "launch": {
            "digest": describe.digest,
            "target": describe.target,
            "n_place": describe.n_place.to_string(),
            "revision": describe.revision,
        },
        "tables": describe.tables,
        "entries": entries,
    });
    let mut text = serde_json::to_string(&manifest)?;
    text.push('\n');
    Ok(text)
}

fn entry_json(entry: &Exported, path: &[String]) -> Result<Value, Error> {
    let (name, module) = path.split_last().expect("stub path has a module");
    let kind = match entry.kind {
        0 => "compute",
        1 => "transact",
        2 => "query",
        3 => "granted",
        4 => "room",
        _ => return Err("Malformed: entry kind".into()),
    };
    let mut slots = Vec::new();
    let mut room = None;
    let result = if entry.kind == 2 || entry.kind == 4 {
        let query = if entry.kind == 4 {
            let schemas = entry.room_schemas()?;
            room = Some(json!({
                "state": schemas.state,
                "message": schemas.message,
                "peers": schemas.peers,
                "state_max": schemas.state_max,
                "message_max": schemas.message_max,
            }));
            schemas.query
        } else {
            entry.query_schemas()?
        };
        for (declared, slot) in entry.slots.iter().zip(query.slots) {
            let kind = match declared.kind {
                0 => "table",
                1 => "cell",
                2 => "log",
                _ => unreachable!("query schemas validate the slot kind"),
            };
            slots.push(json!({
                "name": slot.name,
                "kind": kind,
                "key": slot.key,
                "key_json": Schema::read(&declared.key_json)?,
                "value": slot.row,
            }));
        }
        query.value
    } else {
        Schema::read(&entry.result)?
    };
    let mut value = json!({
        "module": module,
        "name": name,
        "kind": kind,
        "identity": entry.identity,
        "revision": entry.revision,
        "args_fp": entry.args_fp.to_string(),
        "result_fp": entry.result_fp.to_string(),
        "args": Schema::read(&entry.args)?,
        "result": result,
        "slots": slots,
    });
    if let Some(room) = room {
        value["room"] = room;
    }
    Ok(value)
}
