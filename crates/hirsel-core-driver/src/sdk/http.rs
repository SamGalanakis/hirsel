use super::{Error, Reader};

pub struct HttpDefault {
    pub definition: u64,
    pub field: String,
    pub payload: Vec<u8>,
}

pub struct HttpField {
    pub name: String,
    pub source: &'static str,
}

pub struct HttpBinding {
    pub op: String,
    pub method: &'static str,
    pub path: String,
    pub entry: Vec<u8>,
    pub fields: Vec<HttpField>,
    pub mode: &'static str,
    pub ok_status: u16,
    pub error_status: u16,
    pub body_max: u64,
    pub arg_max: u64,
    pub deadline_ms: u64,
    pub steps: u64,
    pub defaults: Vec<HttpDefault>,
}

impl HttpBinding {
    pub(super) fn read(r: &mut Reader<'_>) -> Result<Self, Error> {
        let op = r.name()?;
        let method = enum_tag(r, &["GET", "POST", "PUT", "PATCH", "DELETE"], "method")?;
        let path = r.name()?;
        let entry = r.bytes()?;
        let mut fields = Vec::new();
        for _ in 0..r.count()? {
            fields.push(HttpField {
                name: r.name()?,
                source: enum_tag(r, &["path", "query", "body"], "source")?,
            });
        }
        let mode = enum_tag(r, &["read", "subscribe", "stream", "keyed"], "mode")?;
        let ok_status = r.half()?;
        let error_status = r.half()?;
        let body_max = r.word()?;
        let arg_max = r.word()?;
        let deadline_ms = r.word()?;
        let steps = r.word()?;
        let mut defaults = Vec::new();
        for _ in 0..r.count()? {
            defaults.push(HttpDefault {
                definition: r.word()?,
                field: r.name()?,
                payload: r.bytes()?,
            });
        }
        Ok(Self {
            op,
            method,
            path,
            entry,
            fields,
            mode,
            ok_status,
            error_status,
            body_max,
            arg_max,
            deadline_ms,
            steps,
            defaults,
        })
    }
}

fn enum_tag(r: &mut Reader<'_>, names: &[&'static str], kind: &str) -> Result<&'static str, Error> {
    names
        .get(r.count()?)
        .copied()
        .ok_or_else(|| format!("Malformed: HTTP binding {kind}").into())
}

pub(super) fn bind_error(r: &mut Reader<'_>) -> Result<String, Error> {
    let op = r.name()?;
    let check = match r.count()? {
        0 => "NoEntry".to_owned(),
        1 => "Mode".to_owned(),
        2 => "Op".to_owned(),
        3 => "DuplicateOp".to_owned(),
        4 => "Fields".to_owned(),
        5 => "Path".to_owned(),
        6 => format!("Shape({}, {})", r.byte()?, r.byte()?),
        7 => "Status".to_owned(),
        8 => format!("StreamStatus({:?})", r.name()?),
        9 => "Route".to_owned(),
        10 => "MetadataBytes".to_owned(),
        _ => return Err("Malformed: HTTP binding check".into()),
    };
    Ok(format!("InvalidBinding({op:?}, {check})"))
}

pub(super) fn generate(
    description: &super::Describe,
    files: &mut std::collections::BTreeMap<String, String>,
) -> Result<(), Error> {
    use serde_json::json;
    use std::fmt::Write;

    if description.bindings.is_empty() {
        return Ok(());
    }
    let entries = description
        .exported
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let mut key = entry.identity.clone();
            key.extend(entry.args_fp.to_le_bytes());
            key.extend(entry.result_fp.to_le_bytes());
            (key, (i, entry))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut js = String::from(include_str!("../sdk_http.js"));
    js.push_str("\nimport * as codec from './codec.js';\n");
    if description
        .bindings
        .iter()
        .any(|binding| binding.mode == "subscribe")
    {
        js.push_str("import {queryCodec} from './entries.js';\n");
    }
    js.push_str("import {want} from './entries.js';\nconst bindings = {\n");
    let mut types = String::from(
        "import type * as codec from './codec.js';\n         import type {DecodedRow} from './entries.js';\n         import type {Change, Client, Clock, HttpReply, HttpRequest, Outcome, Subscription, SubscriptionEnd} from './client.js';\n         export type HttpFailure = {tag: 'Rejected' | 'Unavailable' | 'HttpStatus'} | {tag: 'Refused' | 'Fault'; code: string};\n         export type HttpResult<T> = HttpReply & {value?: T; error?: HttpFailure};\n         export type HttpRow<S, K, V> = {slot: S; table: string; key: K} & ({present: true; value: V} | {present: false});\n         export interface HttpBatch<V, R> {version: bigint; complete: number[]; groups: Array<{sub: number; value?: V; rows: R[]}>; outcomes: unknown[]}\n         export interface HttpOptions {url: string; fetch?: typeof fetch; credential?: string; clock?: Clock}\n         export type HttpSubscriptionEnd = {tag: 'Ended'; reason: SubscriptionEnd} | {tag: 'Refused'; status: number; body: string} | {tag: 'Aborted'};\n         export interface HttpSubscription<V, Slots extends object, Keys extends Record<keyof Slots, unknown>> extends Subscription<V, Slots, Keys> {readonly done: Promise<HttpSubscriptionEnd>}\n         export interface HttpSubscribeOptions<V, Slots extends object, Keys extends Record<keyof Slots, unknown>> {\n           signal?: AbortSignal;\n           onChange?: (sub: HttpSubscription<V, Slots, Keys>, change: Change<V, Slots>) => void;\n           onEnd?: (reason: SubscriptionEnd) => void;\n         }\n         export const httpBindings: {\n",
    );
    let mut calls = Vec::new();
    for binding in &description.bindings {
        let &(index, entry) = entries
            .get(&binding.entry)
            .ok_or("Malformed: HTTP binding references an unknown entry")?;
        if entry.kind == 4 {
            return Err("Malformed: an HTTP binding names a room entry".into());
        }
        let args = super::Schema::read(&entry.args)?;
        let result = super::Schema::read(if entry.kind == 2 {
            &entry.value
        } else {
            &entry.result
        })?;
        let projections = entry
            .slots
            .iter()
            .map(|slot| {
                let key = super::Schema::read(&slot.key_json)?;
                let value = super::Schema::read(&slot.value)?;
                Ok(json!({
                    "slot": slot.name,
                    "key": [key.root, json!(key.definitions)],
                    "value": [value.root, json!(value.definitions)],
                }))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let params = json!({
            "op": binding.op,
            "path": binding.path,
            "mode": binding.mode,
            "method": binding.method,
            "fields": binding.fields.iter().map(|f| [&f.name, f.source]).collect::<Vec<_>>(),
            "args": [json!(args.root), json!(args.definitions)],
            "result": [json!(result.root), json!(result.definitions)],
            "query": entry.kind == 2,
            "projections": projections,
            "okStatus": binding.ok_status,
            "errorStatus": binding.error_status,
        });
        let name = json!(binding.op);
        writeln!(
            types,
            "  {name}: {{request(args: codec.Args{index}): HttpRequest}};"
        )?;
        if binding.mode == "subscribe" {
            let mut names = Vec::new();
            let mut keys = Vec::new();
            let mut rows = Vec::new();
            let mut slot_types = Vec::new();
            let mut key_types = Vec::new();
            for (slot_index, slot) in entry.slots.iter().enumerate() {
                let slot_name = json!(slot.name);
                let key = format!("codec.Key{index}_{slot_index}");
                let row = format!("codec.RowValue{index}_{slot_index}");
                slot_types.push(format!(
                    "{slot_name}: DecodedRow<{slot_name}, {key}, {row}>"
                ));
                key_types.push(format!("{slot_name}: {key}"));
                names.push(&slot.name);
                keys.push(key);
                rows.push(row);
            }
            let value = format!("codec.Value{index}");
            let names = json!(names);
            let keys = keys.join(",");
            let rows = rows.join(",");
            writeln!(
                js,
                "  [{name}]: makeHttpBinding({{...{params},want:want[{index}],value:{value},keys:[{keys}],rows:[{rows}],codec:queryCodec({value},{names},[{keys}],[{rows}],0)}}),"
            )?;
            let generics = format!(
                "{value}, {{ {} }}, {{ {} }}",
                slot_types.join("; "),
                key_types.join("; ")
            );
            calls.push(format!(
                "  {name}: (args: codec.Args{index}, options?: HttpSubscribeOptions<{generics}>) => HttpSubscription<{generics}>;\n"
            ));
            continue;
        }
        writeln!(
            js,
            "  [{name}]: makeHttpBinding({{...{params},want:want[{index}]}}),"
        )?;
        let output = match binding.mode {
            "stream" => format!(
                "HttpResult<codec.Result{index}> | Exclude<Outcome<never>, {{tag: 'Returned'}}>"
            ),
            _ if entry.kind == 2 => {
                let rows = entry.slots.iter().enumerate().map(|(slot_index, slot)| {
                    format!("HttpRow<{}, codec.Key{index}_{slot_index}, codec.RowValue{index}_{slot_index}>", json!(slot.name))
                }).collect::<Vec<_>>();
                let row = if rows.is_empty() {
                    "never".to_owned()
                } else {
                    rows.join(" | ")
                };
                format!("HttpResult<HttpBatch<codec.Value{index}, {row}>>")
            }
            _ => format!("HttpResult<codec.Result{index}>"),
        };
        calls.push(format!(
            "  {name}: (args: codec.Args{index}) => Promise<{output}>;\n"
        ));
    }
    js.push_str(
        "};\nexport const httpBindings = Object.freeze(bindings);\n         export function bindHttp(client, options) {\n           return Object.fromEntries(Object.entries(bindings).map(([name, binding]) => [name, (args, subscription) => binding.call(client, options, args, subscription)]));\n         }\n",
    );
    types.push_str("};\nexport function bindHttp(client: Client, options: HttpOptions): {\n");
    for call in calls {
        types.push_str(&call);
    }
    types.push_str("};\n");
    files.insert("http.js".into(), js);
    files.insert("http.d.ts".into(), types);
    Ok(())
}
