//! `hirsel call`: an ordinary wire client for one entry of a running place.
//!
//! Entry types come from the server executable's Describe. The Frame layout
//! comes from std.codec's schema of the bundled runner's Frame, the same
//! generated codec the TypeScript SDK uses. Arguments are typed before any
//! connection is made. The specification is docs/spec/sdk.md, "Calling an
//! entry from the terminal".
use crate::cli::{CallOption as Opt, Request};
use crate::sdk::{Describe, Exported, Schema, stub_paths};
use hirsel_core_ids::hex;
use serde_json::{Map, Number, Value, json};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use websocket::{MESSAGE_LIMIT, MESSAGE_LIMIT_MIB, UPGRADE_HEAD_LIMIT, upgrade_head_end};

type Error = Box<dyn std::error::Error>;

pub(crate) mod websocket;
const KINDS: [&str; 5] = ["compute", "transact", "query", "granted", "room"];

pub struct Options {
    pub server: PathBuf,
    pub url: Option<String>,
    pub n_place: u64,
    pub place_timeout: Duration,
    pub timeout: Duration,
    stream: Option<(u64, u64)>,
    /// The entry name and its flags, after `--`.
    pub call: Vec<String>,
}

fn seconds(option: &str, text: &str) -> Result<Duration, Error> {
    let seconds: f64 = text
        .parse()
        .map_err(|_| format!("{option} expects seconds, got `{text}`"))?;
    let duration = Duration::try_from_secs_f64(seconds)
        .map_err(|_| format!("{option} must be finite and positive"))?;
    if duration.is_zero() {
        return Err(format!("{option} must be positive").into());
    }
    Ok(duration)
}

impl Options {
    pub fn read(invocation: Request<Opt>) -> Result<Self, Error> {
        let server = invocation
            .positional
            .ok_or("call requires the server executable; use help call")?
            .into();
        let mut options = Self {
            server,
            url: None,
            n_place: 16 * 1024 * 1024,
            place_timeout: Duration::from_secs(10),
            timeout: Duration::from_secs(10),
            stream: None,
            call: invocation.arguments,
        };
        for option in invocation.options {
            match option {
                Opt::Url(text) => options.url = Some(text),
                Opt::NPlace(text) => {
                    options.n_place = text
                        .parse()
                        .map_err(|_| format!("--n-place expects BYTES, got `{text}`"))?
                }
                Opt::PlaceTimeout(text) => {
                    options.place_timeout = seconds("--place-timeout", &text)?
                }
                Opt::CallTimeout(text) => options.timeout = seconds("--timeout", &text)?,
                Opt::CallStream(text) => {
                    let bytes: [u8; 16] = hex::decode(&text)
                        .and_then(|bytes| bytes.try_into().ok())
                        .ok_or("--stream expects exactly 32 hexadecimal digits")?;
                    let (t, n) = bytes.split_at(8);
                    options.stream = Some((
                        u64::from_be_bytes(t.try_into()?),
                        u64::from_be_bytes(n.try_into()?),
                    ));
                }
            }
        }
        Ok(options)
    }
}

pub fn run(options: Options) -> Result<i32, Error> {
    run_with(options, crate::sdk_command::describe)
}

fn run_with(
    options: Options,
    describe: impl FnOnce(&Path, u64, Duration) -> Result<Vec<u8>, Error>,
) -> Result<i32, Error> {
    if !options.server.is_file() {
        return Err(format!(
            "call requires the built server executable; {} is not a file",
            options.server.display()
        )
        .into());
    }
    let description = Describe::read(&describe(
        &options.server,
        options.n_place,
        options.place_timeout,
    )?)?;
    let entries = named(&description)?;
    let mut call = options.call.iter();
    let name = match call.next().map(String::as_str) {
        None | Some("--help" | "-h") => {
            print!("{}", listing(&description, &entries)?);
            return Ok(0);
        }
        Some(name) => name,
    };
    let entry = entries
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, e)| *e)
        .ok_or_else(|| {
            format!(
                "{} has no entry `{name}`; use -- --help",
                options.server.display()
            )
        })?;
    let flags: Vec<String> = call.cloned().collect();
    let args = Schema::read(&entry.args)?;
    if flags.iter().any(|f| f == "--help" || f == "-h") {
        print!("{}", entry_help(name, entry, &args)?);
        return Ok(0);
    }
    if let kind @ ("query" | "room") = KINDS[entry.kind as usize] {
        let refusal = format!("`{name}` is a {kind} entry; hirsel call does not subscribe");
        return Err(format!("{refusal} (unsupported)").into());
    }
    let result = Schema::read(&entry.result)?;
    // Every argument is typed before connecting.
    let argument = arguments(&args, &flags)?;
    let url = Url::parse(
        options
            .url
            .as_deref()
            .ok_or("call requires --url ws://HOST:PORT/PATH")?,
    )?;
    let frame = Schema::read(crate::sdk_command::FRAME_SCHEMA)?;
    let outcome = exchange(&url, &frame, &description, name, entry, argument, &options)?;
    let returned = outcome["tag"] == "Returned";
    let shown = if returned {
        let bytes = hex::decode(outcome["value"].as_str().unwrap_or_default())
            .ok_or("Malformed: Returned bytes")?;
        json!({"tag": "Returned", "value": decode(&result, &bytes)?})
    } else {
        outcome
    };
    println!("{shown}");
    Ok(if returned { 0 } else { 1 })
}

/// Each entry under its terminal name, `MODULE/NAME` (docs/spec/sdk.md).
fn named(description: &Describe) -> Result<Vec<(String, &Exported)>, Error> {
    Ok(stub_paths(description)?
        .into_iter()
        .zip(&description.exported)
        .map(|(path, entry)| {
            let (name, module) = path.split_last().expect("stub path has a module");
            (format!("{}/{name}", module.join(".")), entry)
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Types and help text

fn short(name: &str) -> &str {
    name.rsplit(['/', '.']).next().unwrap_or(name)
}

fn type_name(t: &Value, defs: &[Value]) -> String {
    let op = t[0].as_u64().unwrap_or(u64::MAX);
    let names = [
        "()", "bool", "char", "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "f32", "f64",
        "String",
    ];
    match op {
        0..=13 => names[op as usize].into(),
        20 => "Never".into(),
        14 if t[1] == json!([3]) => "Bytes".into(),
        14 => format!("List[{}]", type_name(&t[1], defs)),
        15 => format!(
            "Map[{}, {}]",
            type_name(&t[1], defs),
            type_name(&t[2], defs)
        ),
        16 => format!("Option[{}]", type_name(&t[1], defs)),
        17 => format!(
            "Result[{}, {}]",
            type_name(&t[1], defs),
            type_name(&t[2], defs)
        ),
        18 => format!(
            "({})",
            t[1].as_array()
                .map(|parts| parts.iter().map(|p| type_name(p, defs)).collect::<Vec<_>>())
                .unwrap_or_default()
                .join(", ")
        ),
        _ => {
            let index = t[1].as_u64().unwrap_or(u64::MAX) as usize;
            defs.get(index)
                .and_then(|d| d["name"].as_str())
                .map_or_else(|| "?".into(), |n| short(n).to_owned())
        }
    }
}

fn definition_text(d: &Value, defs: &[Value]) -> String {
    let body = &d["body"];
    let live = |item: &&Value| item["removed"] != true;
    match d["kind"].as_u64() {
        Some(0) => format!(
            "{{ {} }}",
            body.as_array()
                .into_iter()
                .flatten()
                .filter(live)
                .map(|f| format!(
                    "{}: {}",
                    f["name"].as_str().unwrap_or("?"),
                    type_name(&f["type"], defs)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Some(1) => body
            .as_array()
            .into_iter()
            .flatten()
            .filter(live)
            .map(|v| {
                let fields: Vec<String> = v["fields"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|f| type_name(f, defs))
                    .collect();
                let name = v["name"].as_str().unwrap_or("?");
                if fields.is_empty() {
                    name.to_owned()
                } else {
                    format!("{name}({})", fields.join(", "))
                }
            })
            .collect::<Vec<_>>()
            .join(" | "),
        _ => format!("via {}", type_name(body, defs)),
    }
}

/// The record whose fields become flags, or None when the argument is one `--arg`.
fn record_fields(schema: &Schema) -> Option<Vec<&Value>> {
    let def = resolve_ref(&schema.root, &schema.definitions)?;
    (def["kind"] == 0).then(|| {
        def["body"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|f| f["removed"] != true)
            .collect()
    })
}

fn resolve_ref<'a>(t: &Value, defs: &'a [Value]) -> Option<&'a Value> {
    (t[0] == 19).then(|| defs.get(t[1].as_u64()? as usize))?
}

fn flag_lines(schema: &Schema) -> Vec<String> {
    let defs = &schema.definitions;
    match record_fields(schema) {
        Some(fields) => fields
            .iter()
            .filter(|f| f["type"] != json!([0]))
            .map(|f| {
                format!(
                    "--{} {}",
                    f["name"].as_str().unwrap_or("?"),
                    type_name(&f["type"], defs)
                )
            })
            .collect(),
        None if schema.root == json!([0]) => Vec::new(),
        None => vec![format!("--arg {}", type_name(&schema.root, defs))],
    }
}

fn listing(description: &Describe, entries: &[(String, &Exported)]) -> Result<String, Error> {
    let mut out = format!(
        "Entries (target {}, n_place {}):\n",
        description.target, description.n_place
    );
    for (name, entry) in entries {
        let args = Schema::read(&entry.args)?;
        let (result, _) = result_types(entry)?;
        let flags = flag_lines(&args).join(" ");
        out.push_str(&format!(
            "  {name} ({}) {}{}-> {}\n",
            KINDS[entry.kind as usize],
            flags,
            if flags.is_empty() { "" } else { " " },
            result
        ));
    }
    out.push_str("\nUse -- MODULE/NAME --help for one entry's flags and types.\n");
    Ok(out)
}

fn result_types(entry: &Exported) -> Result<(String, Vec<Schema>), Error> {
    let name = |schema: &Schema| type_name(&schema.root, &schema.definitions);
    let result = match entry.kind {
        2 => return Ok(entry.query_schemas()?.describe_types(name)),
        4 => return Ok(entry.room_schemas()?.describe_types(name)),
        _ => Schema::read(&entry.result)?,
    };
    Ok((name(&result), vec![result]))
}

fn entry_help(name: &str, entry: &Exported, args: &Schema) -> Result<String, Error> {
    let mut out = format!("{name} ({})\n", KINDS[entry.kind as usize]);
    let flags = flag_lines(args);
    if flags.is_empty() {
        out.push_str("  (no flags)\n");
    }
    for flag in flags {
        out.push_str(&format!("  {flag}\n"));
    }
    let (result, result_schemas) = result_types(entry)?;
    out.push_str(&format!("returns {result}\n"));
    for schema in std::iter::once(args).chain(&result_schemas) {
        for d in &schema.definitions {
            out.push_str(&format!(
                "  {} = {}\n",
                short(d["name"].as_str().unwrap_or("?")),
                definition_text(d, &schema.definitions)
            ));
        }
    }
    out.push_str("Composite values are JSON; see docs/spec/sdk.md, \"Calling an entry from the terminal\".\n");
    Ok(out)
}

// ---------------------------------------------------------------------------
// Flags to a typed argument

fn primitive(t: &Value, defs: &[Value]) -> bool {
    match resolve_ref(t, defs) {
        Some(d) if d["kind"] == 2 => primitive(&d["body"], defs),
        Some(_) => false,
        None => t[0].as_u64().is_some_and(|op| (1..=13).contains(&op)) || *t == json!([14, [3]]),
    }
}

/// One flag value: plain text for primitives, JSON for composite types.
fn flag_value(flag: &str, text: &str, t: &Value, defs: &[Value]) -> Result<Value, String> {
    let wanted = type_name(t, defs);
    let refuse = |why: String| format!("{flag} expects {wanted}: {why}");
    let t = match resolve_ref(t, defs) {
        Some(d) if d["kind"] == 2 => &d["body"],
        _ => t,
    };
    if !primitive(t, defs) {
        let value: Value =
            serde_json::from_str(text).map_err(|e| refuse(format!("not JSON ({e})")))?;
        return Ok(value);
    }
    let op = t[0].as_u64().unwrap_or_default();
    Ok(match op {
        1 => match text {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => return Err(refuse(format!("`{text}` is not true or false"))),
        },
        3..=10 => {
            let n: i128 = text
                .parse()
                .map_err(|_| refuse(format!("`{text}` is not a decimal integer")))?;
            let (min, max) = int_range(op);
            if n < min || n > max {
                return Err(refuse(format!("{text} is outside {min}..={max}")));
            }
            if n < 0 {
                Value::Number(Number::from(n as i64))
            } else {
                Value::Number(Number::from(n as u64))
            }
        }
        11 | 12 => match text {
            "NaN" | "inf" | "-inf" => Value::String(text.into()),
            _ => {
                let x: f64 = text
                    .parse()
                    .ok()
                    .filter(|x: &f64| x.is_finite())
                    .ok_or_else(|| {
                        refuse(format!(
                            "`{text}` is not a finite decimal, NaN, inf or -inf"
                        ))
                    })?;
                Value::Number(Number::from_f64(x).ok_or_else(|| refuse(text.into()))?)
            }
        },
        _ => Value::String(text.into()),
    })
}

fn int_range(op: u64) -> (i128, i128) {
    let (bits, signed) = match op {
        3 => (8, false),
        4 => (16, false),
        5 => (32, false),
        6 => (64, false),
        7 => (8, true),
        8 => (16, true),
        9 => (32, true),
        _ => (64, true),
    };
    if signed {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    }
}

/// Type every flag against the entry's argument schema and encode the value.
pub fn arguments(schema: &Schema, flags: &[String]) -> Result<Vec<u8>, Error> {
    let defs = &schema.definitions;
    let mut given: Vec<(&str, &str)> = Vec::new();
    let mut rest = flags.iter();
    while let Some(flag) = rest.next() {
        let name = flag
            .strip_prefix("--")
            .filter(|n| !n.is_empty())
            .ok_or_else(|| format!("unexpected argument `{flag}`; flags are --NAME VALUE"))?;
        let value = rest
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        if given.iter().any(|(n, _)| *n == name) {
            return Err(format!("{flag} is given twice").into());
        }
        given.push((name, value));
    }
    let value = match record_fields(schema) {
        Some(fields) => {
            let mut object = Map::new();
            for field in &fields {
                let name = field["name"].as_str().unwrap_or_default();
                let t = &field["type"];
                let value = if *t == json!([0]) {
                    Value::Null
                } else {
                    let (_, text) = given
                        .iter()
                        .find(|(n, _)| *n == name)
                        .ok_or_else(|| format!("missing --{name} {}", type_name(t, defs)))?;
                    flag_value(&format!("--{name}"), text, t, defs)?
                };
                object.insert(name.into(), value);
            }
            if let Some((name, _)) = given
                .iter()
                .find(|(n, _)| !fields.iter().any(|f| f["name"] == *n))
            {
                return Err(format!("unknown flag --{name}; use --help").into());
            }
            Value::Object(object)
        }
        None if schema.root == json!([0]) => {
            if let Some((name, _)) = given.first() {
                return Err(format!("unknown flag --{name}; this entry takes no argument").into());
            }
            Value::Null
        }
        None => {
            if let Some((name, _)) = given.iter().find(|(n, _)| *n != "arg") {
                return Err(format!("unknown flag --{name}; this entry takes --arg").into());
            }
            let (_, text) = given
                .first()
                .ok_or_else(|| format!("missing --arg {}", type_name(&schema.root, defs)))?;
            flag_value("--arg", text, &schema.root, defs)?
        }
    };
    Ok(encode(schema, &value)?)
}

// ---------------------------------------------------------------------------
// The std.codec value form and its JSON mapping (docs/spec/sdk.md)

pub fn encode(schema: &Schema, value: &Value) -> Result<Vec<u8>, String> {
    if schema.key {
        return Err("key schemas are not call arguments (unsupported)".into());
    }
    let mut out = Vec::new();
    write(&schema.root, &schema.definitions, value, &mut out, "")?;
    Ok(out)
}

fn count(n: usize, out: &mut Vec<u8>, at: &str) -> Result<(), String> {
    let n = u32::try_from(n).map_err(|_| format!("{at}: more than u32::MAX items"))?;
    out.extend_from_slice(&n.to_le_bytes());
    Ok(())
}

fn tagged<'a>(value: &'a Value, at: &str) -> Result<(&'a str, &'a Map<String, Value>), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{at}: expected {{\"tag\": ...}}"))?;
    let tag = object["tag"]
        .as_str()
        .ok_or_else(|| format!("{at}: expected a string \"tag\""))?;
    Ok((tag, object))
}

fn only_keys(object: &Map<String, Value>, keys: &[&str], at: &str) -> Result<(), String> {
    match object.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(k) => Err(format!("{at}: unexpected key \"{k}\"")),
        None => Ok(()),
    }
}

fn write(
    t: &Value,
    defs: &[Value],
    value: &Value,
    out: &mut Vec<u8>,
    at: &str,
) -> Result<(), String> {
    let op = t[0].as_u64().ok_or("Malformed: schema")?;
    let wrong = || {
        format!(
            "{} expected {}, got {value}",
            if at.is_empty() { "value" } else { at },
            type_name(t, defs)
        )
    };
    match op {
        0 => value.is_null().then_some(()).ok_or_else(wrong)?,
        1 => out.push(value.as_bool().ok_or_else(wrong)? as u8),
        2 => {
            let text = value.as_str().ok_or_else(wrong)?;
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => out.extend_from_slice(&(c as u32).to_le_bytes()),
                _ => return Err(wrong()),
            }
        }
        3..=10 => {
            let (min, max) = int_range(op);
            let n: i128 = match value {
                Value::Number(n) if n.is_u64() => n.as_u64().unwrap() as i128,
                Value::Number(n) if n.is_i64() => n.as_i64().unwrap() as i128,
                _ => return Err(wrong()),
            };
            if n < min || n > max {
                return Err(format!(
                    "{}: {n} is outside {min}..={max}",
                    if at.is_empty() { "value" } else { at }
                ));
            }
            let width = [1, 2, 4, 8][((op - 3) % 4) as usize];
            out.extend_from_slice(&(n as u64).to_le_bytes()[..width]);
        }
        11 | 12 => {
            let x = match value {
                Value::String(s) if s == "NaN" => f64::NAN,
                Value::String(s) if s == "inf" => f64::INFINITY,
                Value::String(s) if s == "-inf" => f64::NEG_INFINITY,
                Value::Number(n) => n.as_f64().ok_or_else(wrong)?,
                _ => return Err(wrong()),
            };
            if op == 11 {
                let y = if x.is_nan() {
                    f32::from_bits(0x7fc0_0000)
                } else {
                    x as f32
                };
                if x.is_finite() && !y.is_finite() {
                    return Err(format!("{at}: {x} is outside f32"));
                }
                out.extend_from_slice(&y.to_le_bytes());
            } else {
                let y = if x.is_nan() {
                    f64::from_bits(0x7ff8_0000_0000_0000)
                } else {
                    x
                };
                out.extend_from_slice(&y.to_le_bytes());
            }
        }
        13 => {
            let text = value.as_str().ok_or_else(wrong)?;
            count(text.len(), out, at)?;
            out.extend_from_slice(text.as_bytes());
        }
        14 if t[1] == json!([3]) => {
            let bytes = value.as_str().and_then(hex::decode).ok_or_else(wrong)?;
            count(bytes.len(), out, at)?;
            out.extend_from_slice(&bytes);
        }
        14 => {
            let items = value.as_array().ok_or_else(wrong)?;
            count(items.len(), out, at)?;
            for (i, item) in items.iter().enumerate() {
                write(&t[1], defs, item, out, &format!("{at}[{i}]"))?;
            }
        }
        15 => {
            let pairs = value.as_array().ok_or_else(wrong)?;
            count(pairs.len(), out, at)?;
            let mut seen = std::collections::BTreeSet::new();
            for (i, pair) in pairs.iter().enumerate() {
                let at = format!("{at}[{i}]");
                let [k, v] = pair.as_array().map(Vec::as_slice).unwrap_or_default() else {
                    return Err(format!("{at}: expected a [key, value] pair"));
                };
                let mut key = Vec::new();
                write(&t[1], defs, k, &mut key, &format!("{at}[0]"))?;
                if !seen.insert(key.clone()) {
                    return Err(format!("{at}: duplicate map key"));
                }
                out.extend_from_slice(&key);
                write(&t[2], defs, v, out, &format!("{at}[1]"))?;
            }
        }
        16 | 17 => {
            let (tag, object) = tagged(value, at)?;
            let names = if op == 16 {
                ["None", "Some"]
            } else {
                ["Ok", "Error"]
            };
            let index = names.iter().position(|n| *n == tag).ok_or_else(|| {
                format!("{at}: tag \"{tag}\" is not {} or {}", names[0], names[1])
            })?;
            count(index, out, at)?;
            if op == 16 && index == 0 {
                only_keys(object, &["tag"], at)?;
            } else {
                only_keys(object, &["tag", "value"], at)?;
                let inner = object
                    .get("value")
                    .ok_or_else(|| format!("{at}: {tag} needs \"value\""))?;
                write(
                    &t[if op == 16 { 1 } else { index + 1 }],
                    defs,
                    inner,
                    out,
                    &format!("{at}.{tag}"),
                )?;
            }
        }
        18 => {
            let parts = t[1].as_array().ok_or("Malformed: schema")?;
            let items = value
                .as_array()
                .filter(|i| i.len() == parts.len())
                .ok_or_else(wrong)?;
            for (i, (part, item)) in parts.iter().zip(items).enumerate() {
                write(part, defs, item, out, &format!("{at}[{i}]"))?;
            }
        }
        19 => {
            let d = resolve_ref(t, defs).ok_or("Malformed: schema reference")?;
            match d["kind"].as_u64() {
                Some(2) => write(&d["body"], defs, value, out, at)?,
                Some(0) => {
                    let object = value.as_object().ok_or_else(wrong)?;
                    let fields: Vec<&Value> = d["body"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|f| f["removed"] != true)
                        .collect();
                    let names: Vec<&str> =
                        fields.iter().filter_map(|f| f["name"].as_str()).collect();
                    only_keys(object, &names, at)?;
                    for field in fields {
                        let name = field["name"].as_str().unwrap_or_default();
                        let inner = object
                            .get(name)
                            .ok_or_else(|| format!("{at}: missing field \"{name}\""))?;
                        write(&field["type"], defs, inner, out, &format!("{at}.{name}"))?;
                    }
                }
                _ => {
                    let (tag, object) = tagged(value, at)?;
                    let variants = d["body"].as_array().ok_or("Malformed: schema")?;
                    let index = variants
                        .iter()
                        .position(|v| v["removed"] != true && v["name"] == tag)
                        .ok_or_else(|| {
                            format!(
                                "{at}: tag \"{tag}\" is not a variant of {}",
                                type_name(t, defs)
                            )
                        })?;
                    let fields = variants[index]["fields"]
                        .as_array()
                        .ok_or("Malformed: schema")?;
                    count(index, out, at)?;
                    let at = format!("{at}.{tag}");
                    match fields.len() {
                        0 => only_keys(object, &["tag"], &at)?,
                        1 => {
                            only_keys(object, &["tag", "value"], &at)?;
                            let inner = object
                                .get("value")
                                .ok_or_else(|| format!("{at}: needs \"value\""))?;
                            write(&fields[0], defs, inner, out, &at)?;
                        }
                        n => {
                            only_keys(object, &["tag", "values"], &at)?;
                            let values = object
                                .get("values")
                                .and_then(Value::as_array)
                                .filter(|v| v.len() == n)
                                .ok_or_else(|| format!("{at}: needs \"values\" with {n} items"))?;
                            for (i, (f, v)) in fields.iter().zip(values).enumerate() {
                                write(f, defs, v, out, &format!("{at}[{i}]"))?;
                            }
                        }
                    }
                }
            }
        }
        20 => return Err(format!("{at}: Never has no values")),
        _ => return Err(format!("Malformed: schema opcode {op}")),
    }
    Ok(())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).ok_or("Malformed: length")?;
        let out = self.bytes.get(self.at..end).ok_or("Malformed: truncated")?;
        self.at = end;
        Ok(out)
    }
    fn count(&mut self) -> Result<usize, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()) as usize)
    }
}

pub fn decode(schema: &Schema, bytes: &[u8]) -> Result<Value, String> {
    if schema.key {
        return Err("key schemas are not call results (unsupported)".into());
    }
    let mut cursor = Cursor { bytes, at: 0 };
    let value = read(&schema.root, &schema.definitions, &mut cursor)?;
    if cursor.at != bytes.len() {
        return Err("Malformed: trailing bytes".into());
    }
    Ok(value)
}

fn read(t: &Value, defs: &[Value], r: &mut Cursor) -> Result<Value, String> {
    let op = t[0].as_u64().ok_or("Malformed: schema")?;
    Ok(match op {
        0 => Value::Null,
        1 => match r.take(1)?[0] {
            0 => Value::Bool(false),
            1 => Value::Bool(true),
            _ => return Err("Malformed: bool".into()),
        },
        2 => {
            let c = char::from_u32(r.count()? as u32).ok_or("Malformed: char")?;
            Value::String(c.into())
        }
        3..=10 => {
            let width = [1, 2, 4, 8][((op - 3) % 4) as usize];
            let mut raw = [0u8; 8];
            raw[..width].copy_from_slice(r.take(width)?);
            let n = u64::from_le_bytes(raw);
            if op >= 7 {
                let shift = 64 - 8 * width as u32;
                Value::Number(Number::from(((n << shift) as i64) >> shift))
            } else {
                Value::Number(Number::from(n))
            }
        }
        11 => {
            let bits = u32::from_le_bytes(r.take(4)?.try_into().unwrap());
            float(f32::from_bits(bits) as f64, bits == 0x7fc0_0000)?
        }
        12 => {
            let bits = u64::from_le_bytes(r.take(8)?.try_into().unwrap());
            float(f64::from_bits(bits), bits == 0x7ff8_0000_0000_0000)?
        }
        13 => {
            let n = r.count()?;
            Value::String(
                String::from_utf8(r.take(n)?.to_vec()).map_err(|_| "Malformed: invalid UTF-8")?,
            )
        }
        14 if t[1] == json!([3]) => {
            let n = r.count()?;
            Value::String(hex::encode(r.take(n)?))
        }
        14 => {
            let n = r.count()?;
            let mut items = Vec::new();
            for _ in 0..n {
                items.push(read(&t[1], defs, r)?);
            }
            Value::Array(items)
        }
        15 => {
            let n = r.count()?;
            let mut seen = std::collections::BTreeSet::new();
            let mut pairs = Vec::new();
            for _ in 0..n {
                let start = r.at;
                let k = read(&t[1], defs, r)?;
                if !seen.insert(r.bytes[start..r.at].to_vec()) {
                    return Err("Malformed: duplicate map key".into());
                }
                pairs.push(Value::Array(vec![k, read(&t[2], defs, r)?]));
            }
            Value::Array(pairs)
        }
        16 | 17 => {
            let tag = r.count()?;
            if tag > 1 {
                return Err("Malformed: tag".into());
            }
            if op == 16 && tag == 0 {
                json!({"tag": "None"})
            } else {
                let name = [["None", "Some"], ["Ok", "Error"]][(op - 16) as usize][tag];
                let inner = &t[if op == 16 { 1 } else { tag + 1 }];
                json!({"tag": name, "value": read(inner, defs, r)?})
            }
        }
        18 => {
            let parts = t[1].as_array().ok_or("Malformed: schema")?;
            Value::Array(
                parts
                    .iter()
                    .map(|p| read(p, defs, r))
                    .collect::<Result<_, _>>()?,
            )
        }
        19 => {
            let d = resolve_ref(t, defs).ok_or("Malformed: schema reference")?;
            match d["kind"].as_u64() {
                Some(2) => read(&d["body"], defs, r)?,
                Some(0) => {
                    let mut object = Map::new();
                    for field in d["body"].as_array().into_iter().flatten() {
                        if field["removed"] != true {
                            let name = field["name"].as_str().unwrap_or_default().to_owned();
                            object.insert(name, read(&field["type"], defs, r)?);
                        }
                    }
                    Value::Object(object)
                }
                _ => {
                    let tag = r.count()?;
                    let v = d["body"]
                        .get(tag)
                        .filter(|v| v["removed"] != true)
                        .ok_or("Malformed: variant")?;
                    let fields = v["fields"].as_array().ok_or("Malformed: schema")?;
                    let mut values = fields
                        .iter()
                        .map(|f| read(f, defs, r))
                        .collect::<Result<Vec<_>, _>>()?;
                    let name = v["name"].clone();
                    match values.len() {
                        0 => json!({"tag": name}),
                        1 => json!({"tag": name, "value": values.pop()}),
                        _ => json!({"tag": name, "values": values}),
                    }
                }
            }
        }
        20 => return Err("Malformed: Never".into()),
        _ => return Err(format!("Malformed: schema opcode {op}")),
    })
}

fn float(x: f64, canonical_nan: bool) -> Result<Value, String> {
    if x.is_nan() {
        return if canonical_nan {
            Ok(Value::String("NaN".into()))
        } else {
            Err("Malformed: noncanonical NaN".into())
        };
    }
    Ok(match Number::from_f64(x) {
        Some(n) => Value::Number(n),
        None => Value::String(if x > 0.0 { "inf" } else { "-inf" }.into()),
    })
}

// ---------------------------------------------------------------------------
// One RFC 6455 client session speaking hirsel.4

struct Url {
    host: String,
    port: u16,
    path: String,
}
impl Url {
    fn parse(text: &str) -> Result<Self, Error> {
        if text.starts_with("wss://") {
            return Err(
                "wss:// is not supported by hirsel call yet; use ws:// (unsupported)".into(),
            );
        }
        let rest = text
            .strip_prefix("ws://")
            .ok_or_else(|| format!("--url expects ws://HOST:PORT/PATH, got `{text}`"))?;
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !host.ends_with(']') || authority.starts_with('[') => (
                host,
                port.parse()
                    .map_err(|_| format!("--url has a bad port `{port}`"))?,
            ),
            _ => (authority, 80),
        };
        if host.is_empty() {
            return Err(format!("--url has no host: `{text}`").into());
        }
        Ok(Self {
            host: host.to_owned(),
            port,
            path: if path.is_empty() {
                "/".into()
            } else {
                path.into()
            },
        })
    }
}

pub(crate) fn random<const N: usize>() -> Result<[u8; N], Error> {
    let mut bytes = [0u8; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

struct Session {
    stream: TcpStream,
    input: Vec<u8>,
    deadline: Instant,
}
impl Session {
    fn open(url: &Url, deadline: Instant) -> Result<Self, Error> {
        let address = (
            url.host.trim_start_matches('[').trim_end_matches(']'),
            url.port,
        )
            .to_socket_addrs()?
            .next()
            .ok_or("--url host has no address")?;
        let left = deadline.saturating_duration_since(Instant::now());
        let stream = TcpStream::connect_timeout(&address, left.max(Duration::from_millis(1)))
            .map_err(|e| format!("connect {}:{}: {e}", url.host, url.port))?;
        crate::host::socket_option(stream.set_nodelay(true))?;
        let mut session = Self {
            stream,
            input: Vec::new(),
            deadline,
        };
        let key = base64(&random::<16>()?);
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}:{}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Protocol: hirsel.4\r\n\r\n",
            url.path, url.host, url.port
        );
        session.stream.write_all(request.as_bytes())?;
        let end = loop {
            if let Some(end) = upgrade_head_end(&session.input) {
                break end;
            }
            if session.input.len() >= UPGRADE_HEAD_LIMIT {
                return Err("WebSocket upgrade response head is too large".into());
            }
            session.fill()?;
        };
        let head = String::from_utf8_lossy(&session.input[..end]).into_owned();
        session.input.drain(..end);
        let mut lines = head.split("\r\n");
        let status = lines.next().unwrap_or_default();
        if !status.starts_with("HTTP/1.1 101 ") {
            return Err(format!("server refused the WebSocket upgrade: {status}").into());
        }
        let header = |name: &str| {
            head.split("\r\n").skip(1).find_map(|line| {
                let (n, v) = line.split_once(':')?;
                n.trim()
                    .eq_ignore_ascii_case(name)
                    .then(|| v.trim().to_owned())
            })
        };
        let accept = base64(&sha1(
            format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
        ));
        if header("Sec-WebSocket-Accept").as_deref() != Some(accept.as_str()) {
            return Err("server's Sec-WebSocket-Accept does not answer our key".into());
        }
        if header("Sec-WebSocket-Protocol").as_deref() != Some("hirsel.4") {
            return Err("server did not select the hirsel.4 subprotocol".into());
        }
        Ok(session)
    }

    fn fill(&mut self) -> Result<(), Error> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("timed out waiting for the server".into());
        }
        crate::host::socket_option(self.stream.set_read_timeout(Some(left)))?;
        let mut buffer = [0u8; 65536];
        match self.stream.read(&mut buffer) {
            Ok(0) => Err("server closed the connection".into()),
            Ok(n) => {
                self.input.extend_from_slice(&buffer[..n]);
                Ok(())
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Err("timed out waiting for the server".into())
            }
            Err(e) => Err(e.into()),
        }
    }

    fn send(&mut self, opcode: u8, payload: &[u8]) -> Result<(), Error> {
        let mut frame = vec![0x80 | opcode];
        match payload.len() {
            n if n < 126 => frame.push(0x80 | n as u8),
            n if n < 65536 => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        let mask = random::<4>()?;
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.stream.write_all(&frame)?;
        Ok(())
    }

    /// The next complete binary message. Pings are answered; Close ends the session.
    fn message(&mut self) -> Result<Vec<u8>, Error> {
        let mut message: Vec<u8> = Vec::new();
        let mut fragmented = false;
        loop {
            while self.input.len() < 2 {
                self.fill()?;
            }
            let (first, second) = (self.input[0], self.input[1]);
            if first & 0x70 != 0 || second & 0x80 != 0 {
                return Err("server sent a malformed WebSocket frame".into());
            }
            let (header, length) = match second & 0x7f {
                126 => (4, None),
                127 => (10, None),
                n => (2, Some(n as usize)),
            };
            while self.input.len() < header {
                self.fill()?;
            }
            let length = match length {
                Some(n) => n,
                None if header == 4 => u16::from_be_bytes([self.input[2], self.input[3]]) as usize,
                None => usize::try_from(u64::from_be_bytes(self.input[2..10].try_into().unwrap()))
                    .unwrap_or(usize::MAX),
            };
            if length > MESSAGE_LIMIT - message.len() {
                return Err(format!("server message exceeds {MESSAGE_LIMIT_MIB} MiB").into());
            }
            while self.input.len() < header + length {
                self.fill()?;
            }
            let payload: Vec<u8> = self.input.drain(..header + length).skip(header).collect();
            let fin = first & 0x80 != 0;
            match first & 0x0f {
                0x9 => self.send(0xA, &payload)?,
                0xA => {}
                0x8 => {
                    let code = payload.get(..2).map(|c| u16::from_be_bytes([c[0], c[1]]));
                    let _ = self.send(0x8, payload.get(..2).unwrap_or_default());
                    return Err(match code {
                        Some(code) => format!("server closed the session with code {code}"),
                        None => "server closed the session".into(),
                    }
                    .into());
                }
                0x2 if !fragmented => {
                    message = payload;
                    if fin {
                        return Ok(message);
                    }
                    fragmented = true;
                }
                0x0 if fragmented => {
                    message.extend_from_slice(&payload);
                    if fin {
                        return Ok(message);
                    }
                }
                op => return Err(format!("server sent unexpected WebSocket opcode {op}").into()),
            }
        }
    }

    fn close(&mut self) {
        let _ = self.send(0x8, &1000u16.to_be_bytes());
        let _ = self.stream.shutdown(std::net::Shutdown::Write);
    }
}

/// Hello for one entry, Welcome, Placed, then one Call at position 0.
/// The Call is sent only once the server has
/// placed the entry under the Describe fingerprints; it is never resent.
fn exchange(
    url: &Url,
    frame: &Schema,
    description: &Describe,
    name: &str,
    entry: &Exported,
    argument: Vec<u8>,
    options: &Options,
) -> Result<Value, Error> {
    let timeout = options.timeout;
    let deadline = Instant::now() + timeout;
    let mut session = Session::open(url, deadline)?;
    let want = json!({"identity": hex::encode(&entry.identity), "args_fp": entry.args_fp, "result_fp": entry.result_fp});
    let hello = json!({"tag": "Hello", "values": [{"tag": "None"}, [want]]});
    session.send(0x2, &encode(frame, &hello)?)?;
    let mut stream = None;
    let mut sent = false;
    let deadline_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    loop {
        let bytes = match session.message() {
            Ok(bytes) => bytes,
            Err(error) if sent => return Err(format!("LocallyUnknown: {error}").into()),
            Err(error) => return Err(error),
        };
        let received = decode(frame, &bytes).map_err(|e| format!("server frame: {e}"))?;
        let (tag, _) = tagged(&received, "server frame")?;
        let values = &received["values"];
        match tag {
            "Welcome" if stream.is_none() => {
                let subject = values[0]["subject"]
                    .as_str()
                    .and_then(hex::decode)
                    .ok_or("Welcome carries an invalid Caller")?;
                if subject.len() > 64 {
                    return Err("Welcome Caller subject exceeds 64 bytes".into());
                }
                let now = values[6].as_u64().ok_or("Welcome has no keyed time")?;
                let (t, n) = match options.stream {
                    Some(id) => id,
                    None => (now, u64::from_le_bytes(random::<8>()?)),
                };
                stream = Some(json!({"t": t, "n": n}));
                let digest =
                    hex::decode(values[3].as_str().unwrap_or_default()).unwrap_or_default();
                let target = values[4].as_str().unwrap_or_default();
                let n_place = values[5].as_u64().unwrap_or_default();
                if !digest.is_empty()
                    && (digest != description.digest
                        || target != description.target
                        || n_place != description.n_place)
                {
                    session.close();
                    return Err(format!(
                        "the server runs release {} ({target}, n_place {n_place}), not the described {} ({}, n_place {}); the call was not sent",
                        hex::encode(&digest),
                        hex::encode(&description.digest),
                        description.target,
                        description.n_place
                    )
                    .into());
                }
            }
            "Placed" if stream.is_some() && !sent => {
                let (placed, _) = tagged(&received["value"], "Placed")?;
                match placed {
                    "Ready" => {
                        let slot = &received["value"]["value"];
                        let call = json!({"tag": "Call", "values": [stream, 0, slot, deadline_ms, hex::encode(&argument)]});
                        session
                            .send(0x2, &encode(frame, &call)?)
                            .map_err(|error| format!("LocallyUnknown: {error}"))?;
                        sent = true;
                    }
                    "Gone" => {
                        session.close();
                        return Err(format!(
                            "the server does not serve `{name}` with the described identity and fingerprints (args {:016x}, result {:016x}): Placed Gone; the call was not sent",
                            entry.args_fp, entry.result_fp
                        )
                        .into());
                    }
                    other => {
                        session.close();
                        return Err(format!(
                            "Placed {other} is not supported by hirsel call; the call was not sent"
                        )
                        .into());
                    }
                }
            }
            "Batch" | "Reset" if sent => {
                let outcomes = &values[if tag == "Batch" { 3 } else { 1 }];
                if let Some(done) = outcomes
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|d| Some(&d["stream"]) == stream.as_ref() && d["p"] == 0)
                {
                    session.close();
                    return Ok(done["outcome"].clone());
                }
            }
            "Batch" | "Ended" if stream.is_some() => {}
            other => {
                session.close();
                return Err(format!("server sent {other} out of order").into());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// RFC 3174 SHA-1 and RFC 4648 base64, for the RFC 6455 accept check only.

pub(crate) fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in padded.chunks(64) {
        let mut w = [0u32; 80];
        for (word, bytes) in w.iter_mut().zip(block.chunks(4)) {
            *word = u32::from_be_bytes(bytes.try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, t);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

pub(crate) fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | ((*b as u32) << (16 - 8 * i)));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

#[cfg(test)]
mod tests;
