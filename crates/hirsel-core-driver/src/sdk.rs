//! TypeScript bindings from the runner's describe output and codec schemas.
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

type Error = Box<dyn std::error::Error>;
pub const RUNTIME: &str = include_str!("sdk_codec.js");
const DECLARATIONS: &str = include_str!("sdk_codec.d.ts");

mod describe;
mod http;
mod manifest;
mod stubs;

pub use http::HttpBinding;
pub use manifest::manifest_json;
use stubs::StubTypes;
pub(crate) use stubs::{entry_path, stub_paths};

pub(crate) struct Reader<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) at: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(n).ok_or("Malformed: length")?;
        let out = self.bytes.get(self.at..end).ok_or("Malformed: truncated")?;
        self.at = end;
        Ok(out)
    }
    pub(crate) fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn half(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    pub(crate) fn count(&mut self) -> Result<usize, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?) as usize)
    }
    pub(crate) fn word(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    pub(crate) fn bytes(&mut self) -> Result<Vec<u8>, Error> {
        let n = self.count()?;
        Ok(self.take(n)?.to_vec())
    }
    pub(crate) fn name(&mut self) -> Result<String, Error> {
        Ok(String::from_utf8(self.bytes()?)?)
    }
    pub(crate) fn done(&self) -> Result<(), Error> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err("Malformed: trailing bytes".into())
        }
    }
    fn ty(&mut self, key: bool) -> Result<Value, Error> {
        let op = self.byte()?;
        Ok(match op {
            0..=13 | 20 => json!([op]),
            14 | 16 => json!([op, self.ty(key)?]),
            21 if key => json!([op, self.ty(key)?]),
            15 | 17 => json!([op, self.ty(key)?, self.ty(key)?]),
            18 => {
                let n = self.count()?;
                let mut types = Vec::new();
                for _ in 0..n {
                    types.push(self.ty(key)?);
                }
                json!([op, types])
            }
            19 => json!([op, self.count()?]),
            _ => return Err(format!("Malformed: schema opcode {op}").into()),
        })
    }
    fn removed(&mut self) -> Result<bool, Error> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err("Malformed: schema tombstone".into()),
        }
    }
}

#[derive(serde::Serialize)]
pub struct Schema {
    pub(crate) root: Value,
    pub(crate) definitions: Vec<Value>,
    pub(crate) key: bool,
}
impl Schema {
    pub fn read(bytes: &[u8]) -> Result<Self, Error> {
        let mut r = Reader { bytes, at: 0 };
        let key = match r.byte()? {
            3 => false,
            4 => true,
            _ => return Err("Malformed: schema version".into()),
        };
        let n = r.count()?;
        let mut definitions = Vec::new();
        for _ in 0..n {
            let name = r.name()?;
            let count = r.count()?;
            let mut arguments = Vec::new();
            for _ in 0..count {
                arguments.push(r.ty(key)?);
            }
            let kind = r.byte()?;
            let body = match kind {
                0 => {
                    let n = r.count()?;
                    let mut fields = Vec::new();
                    for _ in 0..n {
                        fields.push(
                            json!({"removed":r.removed()?,"name":r.name()?,"type":r.ty(key)?}),
                        );
                    }
                    json!(fields)
                }
                1 => {
                    let n = r.count()?;
                    let mut variants = Vec::new();
                    for _ in 0..n {
                        let removed = r.removed()?;
                        let name = r.name()?;
                        let n = r.count()?;
                        let mut fields = Vec::new();
                        for _ in 0..n {
                            fields.push(r.ty(key)?);
                        }
                        variants.push(json!({"removed":removed,"name":name,"fields":fields}));
                    }
                    json!(variants)
                }
                2 => r.ty(key)?,
                _ => return Err("Malformed: schema definition kind".into()),
            };
            definitions.push(json!({"name":name,"arguments":arguments,"kind":kind,"body":body}));
        }
        let root = r.ty(key)?;
        r.done()?;
        Ok(Self {
            root,
            definitions,
            key,
        })
    }
    pub fn javascript(&self, name: &str) -> String {
        format!(
            "export const {name} = makeCodec({}, {}, {});\n",
            self.root,
            json!(self.definitions),
            self.key
        )
    }
    pub fn declarations(&self, name: &str) -> String {
        let mut out = String::new();
        for (i, d) in self.definitions.iter().enumerate() {
            let mut ty = match d["kind"].as_u64().unwrap() {
                0 => format!(
                    "{{ {} }}",
                    d["body"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|f| f["removed"] == false)
                        .map(|f| format!("{}: {}", f["name"], Self::typescript(&f["type"], name)))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
                1 => d["body"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|v| v["removed"] == false)
                    .map(|v| {
                        let fields = v["fields"].as_array().unwrap();
                        let payload = match fields.len() {
                            0 => String::new(),
                            1 => format!("; value: {}", Self::typescript(&fields[0], name)),
                            _ => format!(
                                "; values: [{}]",
                                fields
                                    .iter()
                                    .map(|f| Self::typescript(f, name))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        };
                        format!("{{ tag: {}{payload} }}", v["name"])
                    })
                    .collect::<Vec<_>>()
                    .join(" | "),
                _ => Self::typescript(&d["body"], name),
            };
            if ty.is_empty() {
                ty = "never".into();
            }
            out.push_str(&format!("export type {name}Def{i} = {ty};\n"));
        }
        out.push_str(&format!(
            "export type {name} = {};\nexport const {name}: Codec<{name}>;\n",
            Self::typescript(&self.root, name)
        ));
        out
    }
    fn typescript(t: &Value, prefix: &str) -> String {
        let op = t[0].as_u64().unwrap();
        match op {
            0 => "undefined".into(),
            1 => "boolean".into(),
            2 | 13 => "string".into(),
            6 | 10 => "bigint".into(),
            3..=12 => "number".into(),
            14 if t[1] == json!([3]) => "Uint8Array".into(),
            14 => format!("Array<{}>", Self::typescript(&t[1], prefix)),
            15 => format!(
                "ReadonlyMap<{}, {}>",
                Self::typescript(&t[1], prefix),
                Self::typescript(&t[2], prefix)
            ),
            16 => format!("Option<{}>", Self::typescript(&t[1], prefix)),
            17 => format!(
                "Result<{}, {}>",
                Self::typescript(&t[1], prefix),
                Self::typescript(&t[2], prefix)
            ),
            18 => format!(
                "[{}]",
                t[1].as_array()
                    .unwrap()
                    .iter()
                    .map(|t| Self::typescript(t, prefix))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            19 => format!("{prefix}Def{}", t[1]),
            20 => "never".into(),
            21 => format!("{{ value: {} }}", Self::typescript(&t[1], prefix)),
            _ => unreachable!(),
        }
    }
}

pub struct Exported {
    pub kind: u32,
    pub identity: Vec<u8>,
    pub args: Vec<u8>,
    pub result: Vec<u8>,
    pub args_fp: u64,
    pub result_fp: u64,
    pub revision: Vec<u8>,
    pub slots: Vec<ExportedSlot>,
    pub value: Vec<u8>,
}

#[derive(serde::Serialize)]
pub struct DeclaredCollection {
    pub name: String,
    pub id: u32,
    pub owner: u32,
    pub kind: u8,
}

pub struct ExportedSlot {
    pub name: String,
    pub kind: u32,
    pub key: Vec<u8>,
    pub key_json: Vec<u8>,
    pub value: Vec<u8>,
}

pub(crate) struct QuerySchemas {
    pub value: Schema,
    pub slots: Vec<QuerySlot>,
}

impl QuerySchemas {
    pub(crate) fn describe_types(self, name: impl Fn(&Schema) -> String) -> (String, Vec<Schema>) {
        let mut description = name(&self.value);
        let mut schemas = vec![self.value];
        for slot in self.slots {
            description.push_str(&format!(
                ", {}: rows[{}, {}]",
                slot.name,
                name(&slot.key),
                name(&slot.row)
            ));
            schemas.extend([slot.key, slot.row]);
        }
        (description, schemas)
    }
}

/// A Room entry's schemas: the admission view's Grant value and slots, then
/// the room's State and Message codecs and its bounds (runner.room).
pub(crate) struct RoomSchemas {
    pub query: QuerySchemas,
    pub state: Schema,
    pub message: Schema,
    pub peers: u32,
    pub state_max: u32,
    pub message_max: u32,
}

impl RoomSchemas {
    pub(crate) fn describe_types(self, name: impl Fn(&Schema) -> String) -> (String, Vec<Schema>) {
        let types = format!(
            ", state: {}, message: {}",
            name(&self.state),
            name(&self.message)
        );
        let (mut description, mut schemas) = self.query.describe_types(name);
        description.push_str(&types);
        schemas.extend([self.state, self.message]);
        (description, schemas)
    }
}

pub(crate) struct QuerySlot {
    pub name: String,
    pub key: Schema,
    pub row: Schema,
}

impl Exported {
    pub(crate) fn query_schemas(&self) -> Result<QuerySchemas, Error> {
        let value = Schema::read(&self.value)?;
        if value.key {
            return Err("Malformed: query result needs a value schema".into());
        }
        let mut slots = Vec::new();
        for slot in &self.slots {
            let key = match slot.kind {
                0 | 2 => Schema::read(&slot.key)?,
                1 if slot.key.is_empty() => Schema::read(&[4, 0, 0, 0, 0, 0])?,
                _ => return Err("Malformed: query slot kind or key schema".into()),
            };
            let row = Schema::read(&slot.value)?;
            if !key.key || row.key {
                return Err(
                    "Malformed: slot needs an ordered key schema and a value schema".into(),
                );
            }
            slots.push(QuerySlot {
                name: slot.name.clone(),
                key,
                row,
            });
        }
        Ok(QuerySchemas { value, slots })
    }

    /// The Room `Described.result`: the codec of (value, slots, state schema,
    /// message schema, peers, state bound, message bound).
    pub(crate) fn room_schemas(&self) -> Result<RoomSchemas, Error> {
        let mut r = Reader {
            bytes: &self.result,
            at: 0,
        };
        if r.bytes()? != self.value {
            return Err("Malformed: room result disagrees with its value schema".into());
        }
        r.bytes()?;
        let state = Schema::read(&r.bytes()?)?;
        let message = Schema::read(&r.bytes()?)?;
        let (peers, state_max, message_max) =
            (r.count()? as u32, r.count()? as u32, r.count()? as u32);
        r.done()?;
        if state.key || message.key {
            return Err("Malformed: room state and message need value schemas".into());
        }
        Ok(RoomSchemas {
            query: self.query_schemas()?,
            state,
            message,
            peers,
            state_max,
            message_max,
        })
    }
}
pub struct Describe {
    pub digest: Vec<u8>,
    pub target: String,
    pub n_place: u64,
    pub revision: Vec<u8>,
    pub exported: Vec<Exported>,
    pub tables: Vec<DeclaredCollection>,
    pub bindings: Vec<HttpBinding>,
}

pub fn place_error(bytes: &[u8]) -> Result<String, Error> {
    let mut r = Reader { bytes, at: 0 };
    let message = match r.count()? {
        0 => format!(
            "PlaceCapacity(Room {{ requested: {}, available: {} }})",
            r.word()?,
            r.word()?
        ),
        1 => format!("AlreadyRegistered({:?})", r.bytes()?),
        2 => "Bounds".to_owned(),
        3 => http::bind_error(&mut r)?,
        _ => return Err("Malformed: describe error tag".into()),
    };
    r.done()?;
    Ok(message)
}

/// Complete all generation before touching the requested output directory.
pub fn generate(describe: &Describe) -> Result<BTreeMap<String, String>, Error> {
    let paths = stub_paths(describe)?;
    let mut js = RUNTIME.to_owned();
    let mut dts = DECLARATIONS.to_owned();
    let mut entries = "import * as codec from './codec.js';\n".to_owned();
    entries.push_str(&format!("export const identity = Object.freeze({{digest:Uint8Array.from({}),target:{},n_place:{}n}});\n",json!(describe.digest),json!(describe.target),describe.n_place));
    entries.push_str(&format!(
        "export const tables = Object.freeze({});\n",
        json!(describe.tables)
    ));
    entries.push_str("export const want = [\n");
    for entry in &describe.exported {
        entries.push_str(&format!(
            "{{identity:Uint8Array.from({}),args_fp:{}n,result_fp:{}n}},\n",
            json!(entry.identity),
            entry.args_fp,
            entry.result_fp
        ));
    }
    entries.push_str("];\nexport function bind(client) {\nconst call = Object.create(null), subscribe = Object.create(null), subscribeDecoded = Object.create(null), room = Object.create(null);\nconst ns = (root, path) => path.reduce((o, k) => o[k] ??= Object.create(null), root);\n");
    let mut calls = StubTypes::default();
    let mut subscriptions = StubTypes::default();
    let mut decoded = StubTypes::default();
    let mut rooms = StubTypes::default();
    for (i, entry) in describe.exported.iter().enumerate() {
        let args = Schema::read(&entry.args)?;
        let arg_name = format!("Args{i}");
        js.push_str(&args.javascript(&arg_name));
        dts.push_str(&args.declarations(&arg_name));
        let path = &paths[i];
        let (name, module) = path.split_last().expect("stub path has a module");
        let at = |root: &str| format!("ns({root},{})[{}]", json!(module), json!(name));
        if entry.kind == 2 || entry.kind == 4 {
            let room_schemas = if entry.kind == 4 {
                Some(entry.room_schemas()?)
            } else {
                None
            };
            let query = entry.query_schemas()?;
            if room_schemas.is_none() {
                entries.push_str(&format!("{} = (args,onChange,onEnd) => client.subscribe(want[{i}],codec.{arg_name}.encode(args),onChange,onEnd);\n", at("subscribe")));
                subscriptions.insert(path, format!(
                    "(args: codec.{arg_name}, onChange: (sub: Subscription, change: Change<Uint8Array, Record<number, SlotRow>>) => void, onEnd?: (reason: SubscriptionEnd) => void) => Subscription"
                ));
            }
            let value = format!("Value{i}");
            js.push_str(&query.value.javascript(&value));
            dts.push_str(&query.value.declarations(&value));
            let mut names = Vec::new();
            let mut key_codecs = Vec::new();
            let mut row_codecs = Vec::new();
            let mut slot_types = Vec::new();
            let mut slot_keys = Vec::new();
            for (s, slot) in query.slots.iter().enumerate() {
                let key = format!("Key{i}_{s}");
                let row = format!("RowValue{i}_{s}");
                js.push_str(&slot.key.javascript(&key));
                js.push_str(&slot.row.javascript(&row));
                dts.push_str(&slot.key.declarations(&key));
                dts.push_str(&slot.row.declarations(&row));
                let slot_name = json!(slot.name);
                slot_types.push(format!(
                    "{slot_name}: DecodedRow<{slot_name}, codec.{key}, codec.{row}>"
                ));
                slot_keys.push(format!("{slot_name}: codec.{key}"));
                names.push(slot_name);
                key_codecs.push(format!("codec.{key}"));
                row_codecs.push(format!("codec.{row}"));
            }
            let decode = format!(
                "queryCodec(codec.{value}, {}, [{}], [{}])",
                json!(names),
                key_codecs.join(","),
                row_codecs.join(",")
            );
            let slots = format!("{{ {} }}", slot_types.join("; "));
            let keys = format!("{{ {} }}", slot_keys.join("; "));
            if let Some(live) = room_schemas {
                let (state, message) = (format!("State{i}"), format!("Message{i}"));
                js.push_str(&live.state.javascript(&state));
                js.push_str(&live.message.javascript(&message));
                dts.push_str(&live.state.declarations(&state));
                dts.push_str(&live.message.declarations(&message));
                entries.push_str(&format!(
                    "{} = (args,handlers) => client.room(want[{i}],codec.{arg_name}.encode(args),handlers ?? {{}},{decode},{{state:codec.{state},message:codec.{message},peers:{},stateMax:{},messageMax:{}}});\n",
                    at("room"), live.peers, live.state_max, live.message_max
                ));
                let types =
                    format!("codec.{state}, codec.{message}, codec.{value}, {slots}, {keys}");
                rooms.insert(
                    path,
                    format!("(args: codec.{arg_name}, handlers?: RoomHandlers<{types}>) => LiveRoom<{types}>"),
                );
                continue;
            }
            entries.push_str(&format!(
                "{} = (args,onChange,onEnd) => client.subscribe(want[{i}],codec.{arg_name}.encode(args),onChange,onEnd,{decode});\n",
                at("subscribeDecoded")
            ));
            let sub = format!("Subscription<codec.{value}, {slots}, {keys}>");
            decoded.insert(path, format!("(args: codec.{arg_name}, onChange: (sub: {sub}, change: Change<codec.{value}, {slots}>) => void, onEnd?: (reason: SubscriptionEnd) => void) => {sub}"));
        } else {
            let result = Schema::read(&entry.result)?;
            let result_name = format!("Result{i}");
            js.push_str(&result.javascript(&result_name));
            dts.push_str(&result.declarations(&result_name));
            entries.push_str(&format!("{} = (args,options) => client.call(want[{i}],codec.{arg_name}.encode(args),codec.{result_name}.decode,options);\n", at("call")));
            calls.insert(
                path,
                format!("(args: codec.{arg_name}, options?: CallOptions) => Promise<Outcome<codec.{result_name}>>"),
            );
        }
    }
    entries.push_str("return {call,subscribe,subscribeDecoded,room};\n}\n");
    if !decoded.is_empty() || !rooms.is_empty() {
        entries.push_str(r#"export function queryCodec(value,names,keys,rows,keyPrefixBytes = 4) {
    const positions = new Map(names.map((name, i) => [name, i]));
    const slot = position => {
        if (names[position] === undefined) throw new codec.Malformed('unknown row slot');
        return names[position];
    };
    const decodeRow = (i, key, value) => ({slot:slot(i),key:keys[i].decode(key),
        value:value.tag === 'Some' ? {tag:'Some',value:rows[i].decode(value.value)} : value});
    return {
        value: value.decode, slot, keyPrefixBytes,
        key: (name, key) => keys[positions.get(name)].encode(key),
        encodeValue: value.encode,
        encodeRow(name, key, row) {
            const i = positions.get(name);
            if (row.slot !== name || row.value.tag !== 'Some') throw new codec.Malformed('invalid Draft row');
            const actual = keys[i].encode(row.key);
            if (actual.length !== key.length || actual.some((byte, j) => byte !== key[j])) {
                throw new codec.Malformed('Draft row key mismatch');
            }
            const raw = rows[i].encode(row.value.value);
            return {raw, decoded:decodeRow(i,key,{tag:'Some',value:raw})};
        },
        row(row) {
            if (row.key.length < keyPrefixBytes) throw new codec.Malformed('truncated collection key');
            return decodeRow(row.slot,row.key.subarray(keyPrefixBytes),row.value);
        },
    };
}
"#);
    }
    let entry_dts = format!(
        "import type * as codec from './codec.js';\nimport type {{CallOptions, Change, Option, Outcome, LiveRoom, RoomHandlers, SlotRow, Subscription, SubscriptionEnd, Transport}} from './client.js';\nexport const identity: {{readonly digest: Uint8Array; readonly target: string; readonly n_place: bigint}};\nexport const want: Array<{{identity: Uint8Array; args_fp: bigint; result_fp: bigint}}>;\nexport const tables: ReadonlyArray<{{name: string; id: number; owner: number; kind: number}}>;\nexport type DecodedRow<S, K, V> = {{slot: S; key: K; value: Option<V>}};\nexport function bind(client: Transport): {{call: {}; subscribe: {}; subscribeDecoded: {}; room: {} }};\n",
        calls.render(),
        subscriptions.render(),
        decoded.render(),
        rooms.render()
    );
    let mut files = BTreeMap::from([
        ("package.json".into(), "{\"type\":\"module\"}\n".into()),
        ("codec.js".into(), js),
        ("codec.d.ts".into(), dts),
        ("entries.js".into(), entries),
        ("entries.d.ts".into(), entry_dts),
        (
            "client.ts".into(),
            include_str!("../../../sdk/client/client.ts").into(),
        ),
        (
            "image.ts".into(),
            include_str!("../../../sdk/client/image.ts").into(),
        ),
        (
            "browser.ts".into(),
            include_str!("../../../sdk/client/browser.ts").into(),
        ),
        (
            "node.ts".into(),
            include_str!("../../../sdk/client/node.ts").into(),
        ),
        (
            "persistence.ts".into(),
            include_str!("../../../sdk/client/persistence.ts").into(),
        ),
        (
            "room.ts".into(),
            include_str!("../../../sdk/client/room.ts").into(),
        ),
        (
            "wire.ts".into(),
            include_str!("../../../sdk/client/wire.ts").into(),
        ),
    ]);
    http::generate(describe, &mut files)?;
    files.insert("manifest.json".into(), manifest::render(describe, &paths)?);
    Ok(files)
}
pub fn write(describe: &Describe, out: &Path) -> Result<(), Error> {
    let files = generate(describe)?;
    write_files(files, out)
}

pub fn write_with_frame(describe: &Describe, frame_schema: &[u8], out: &Path) -> Result<(), Error> {
    let mut files = generate(describe)?;
    let schema = Schema::read(frame_schema)?;
    files
        .get_mut("codec.js")
        .unwrap()
        .push_str(&schema.javascript("Frame"));
    files
        .get_mut("codec.d.ts")
        .unwrap()
        .push_str(&schema.declarations("Frame"));
    for name in ["Outcome", "Fault", "Refusal", "StreamId", "Part"] {
        let index = schema
            .definitions
            .iter()
            .position(|d| {
                d["name"]
                    .as_str()
                    .is_some_and(|path| path.rsplit(['.', '/']).next() == Some(name))
            })
            .ok_or_else(|| format!("Frame schema has no {name}"))?;
        if name == "Outcome"
            && let Some(http) = files.get_mut("http.js")
        {
            http.push_str(&format!(
                "const outcomeSchema = [[19,{index}],{}];\n",
                json!(schema.definitions)
            ));
        }
        files.get_mut("codec.js").unwrap().push_str(&format!(
            "export const {name} = makeCodec([19,{index}],{});\n",
            json!(schema.definitions)
        ));
        files.get_mut("codec.d.ts").unwrap().push_str(&format!(
            "export type {name} = FrameDef{index};\nexport const {name}: Codec<{name}>;\n"
        ));
    }
    files.insert("frame.js".into(),"export {Frame,Outcome,Fault,Refusal,StreamId,Part} from './codec.js';\nexport const subprotocol = 'hirsel.4';\nexport const Raised = Object.freeze({Overflow:1,Bounds:2,Division:3,Unwrap:4,Fail:5,Cancelled:6,Capacity:7,OutOfTime:8});\nexport const BadKind = Object.freeze({Truncated:0,Invalid:1,Utf8:2,Duplicate:3,Trailing:4,Schema:5,Value:6,StreamFuture:7});\n".into());
    files.insert("frame.d.ts".into(),"export {Frame,Outcome,Fault,Refusal,StreamId,Part} from './codec.js';\nexport const subprotocol: 'hirsel.4';\nexport const Raised: Readonly<{Overflow:1; Bounds:2; Division:3; Unwrap:4; Fail:5; Cancelled:6; Capacity:7; OutOfTime:8}>;\nexport const BadKind: Readonly<{Truncated:0; Invalid:1; Utf8:2; Duplicate:3; Trailing:4; Schema:5; Value:6; StreamFuture:7}>;\n".into());
    write_files(files, out)
}

fn write_files(files: BTreeMap<String, String>, out: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(out)?;
    for (name, content) in files {
        std::fs::write(out.join(name), content)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::stubs::stub_path;
    use super::*;
    fn scalar(op: u8) -> Vec<u8> {
        vec![3, 0, 0, 0, 0, op]
    }
    fn entry(module: &str, name: &str) -> Exported {
        Exported {
            kind: 0,
            identity: format!("\x01app@1/{module}/{name}(take u64)->u64").into_bytes(),
            args: scalar(6),
            result: scalar(6),
            args_fp: u64::MAX,
            result_fp: 7,
            revision: vec![1; 32],
            slots: vec![],
            value: vec![],
        }
    }
    #[test]
    fn sdk_schema_opcodes_and_shapes() {
        for op in 0..=13 {
            let s = Schema::read(&scalar(op)).unwrap();
            assert_eq!(s.root, json!([op]));
            assert!(!s.declarations("T").is_empty());
        }
        for (tail, want) in [
            (vec![14, 3], "Uint8Array"),
            (vec![15, 13, 6], "ReadonlyMap<string, bigint>"),
            (vec![16, 16, 5], "Option<Option<number>>"),
            (vec![17, 6, 13], "Result<bigint, string>"),
            (vec![17, 3, 20], "Result<number, never>"),
            (vec![18, 2, 0, 0, 0, 6, 13], "[bigint, string]"),
        ] {
            let mut bytes = scalar(0);
            bytes.pop();
            bytes.extend(tail);
            let s = Schema::read(&bytes).unwrap();
            assert!(s.declarations("T").contains(want));
        }
        let s = Schema {
            root: json!([19, 0]),
            definitions: vec![
                json!({"name":"S","kind":1,"body":[{"name":"A","removed":false,"fields":[]},{"name":"B","removed":false,"fields":[[6]]},{"name":"C","removed":false,"fields":[[6],[13]]}]}),
            ],
            key: false,
        };
        let d = s.declarations("T");
        assert!(d.contains("{ tag: \"A\" }"));
        assert!(d.contains("{ tag: \"B\"; value: bigint }"));
        assert!(d.contains("{ tag: \"C\"; values: [bigint, string] }"));
        let empty = Schema {
            root: json!([19, 0]),
            definitions: vec![json!({"name":"Empty","kind":1,"body":[]})],
            key: false,
        };
        assert!(
            empty
                .declarations("Empty")
                .contains("export type EmptyDef0 = never;")
        );
    }
    #[test]
    fn sdk_stub_paths_namespace_by_module() {
        // Oracle: docs/spec/sdk.md section 4. Stubs are api.call.<module>.<name>,
        // nested for a nested module; a clash or a function/namespace overlap is
        // DuplicateStubName before any output is written.
        let mut d = Describe {
            digest: vec![1],
            target: "t".into(),
            n_place: 16,
            revision: vec![],
            exported: vec![entry("a", "get"), entry("b", "get"), entry("x/y", "get")],
            tables: vec![],
            bindings: vec![],
        };
        let files = generate(&d).unwrap();
        for line in [
            "ns(call,[\"a\"])[\"get\"] = (args,options) => client.call(want[0],codec.Args0.encode(args),codec.Result0.decode,options);\n",
            "ns(call,[\"b\"])[\"get\"] = (args,options) => client.call(want[1],codec.Args1.encode(args),codec.Result1.decode,options);\n",
            "ns(call,[\"x\",\"y\"])[\"get\"] = (args,options) => client.call(want[2],codec.Args2.encode(args),codec.Result2.decode,options);\n",
        ] {
            assert!(files["entries.js"].contains(line), "{line}");
        }
        assert!(files["entries.d.ts"].contains(
            "export function bind(client: Transport): {call: { \"a\": { \"get\": (args: codec.Args0, options?: CallOptions) => Promise<Outcome<codec.Result0>> }; \"b\": { \"get\": (args: codec.Args1, options?: CallOptions) => Promise<Outcome<codec.Result1>> }; \"x\": { \"y\": { \"get\": (args: codec.Args2, options?: CallOptions) => Promise<Outcome<codec.Result2>> } } }; subscribe: {  }; subscribeDecoded: {  } };\n"
        ));
        assert_eq!(
            stub_path(&entry("x", "%74oggle").identity).unwrap(),
            ["x", "toggle"]
        );
        assert_eq!(
            stub_path(b"\x01runner@1/runner_launch/echo(take u64)->u64").unwrap(),
            ["runner_launch", "echo"]
        );
        let generic_cases: &[(&[u8], [&str; 2])] = &[
            (
                b"\x01app@1/main/pick[u64]{plain[u64]}($0,$0)->$0",
                ["main", "pick"],
            ),
            (
                b"\x01app@1/main/swap[u64,core@1/core/Text]{send[u64]}(take($0,$1))->($1,$0)",
                ["main", "swap"],
            ),
            (b"\x01app@1/main/a%5Bb(u64)->u64", ["main", "a[b"]),
        ];
        for &(identity, expected) in generic_cases {
            assert_eq!(stub_path(identity).unwrap(), expected);
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("absent");
        d.exported.push(entry("x", "y"));
        assert_eq!(
            write(&d, &out).unwrap_err().to_string(),
            "DuplicateStubName(x.y::get, x::y)"
        );
        assert!(!out.exists());
        d.exported.truncate(2);
        d.exported[0].identity = generic_cases[0].0.to_vec();
        d.exported[1].identity = b"\x01app@1/main/pick[u32]{plain[u32]}($0,$0)->$0".to_vec();
        assert_eq!(
            write(&d, &out).unwrap_err().to_string(),
            "DuplicateStubName(main::pick, main::pick)"
        );
        assert!(!out.exists());
        let existing = dir.path().join("existing");
        std::fs::create_dir(&existing).unwrap();
        std::fs::write(existing.join("codec.js"), "original").unwrap();
        assert!(write(&d, &existing).is_err());
        assert_eq!(
            std::fs::read_to_string(existing.join("codec.js")).unwrap(),
            "original"
        );
    }
    #[test]
    fn sdk_generation_preserves_identity_fingerprints_and_entry_kinds() {
        let mut query = entry("m", "rows");
        query.kind = 2;
        query.value = scalar(0);
        query.slots = vec![
            ExportedSlot {
                name: "data".into(),
                kind: 0,
                key: vec![4, 0, 0, 0, 0, 6],
                key_json: scalar(6),
                value: scalar(13),
            },
            ExportedSlot {
                name: "other".into(),
                kind: 1,
                key: vec![],
                key_json: scalar(0),
                value: scalar(5),
            },
        ];
        let mut computed = entry("m", "total");
        computed.kind = 2;
        computed.value = scalar(6);
        let d = Describe {
            digest: vec![1, 2],
            target: "x86_64-unknown-linux-gnu".into(),
            n_place: u64::MAX,
            revision: vec![],
            exported: vec![entry("m", "toggle"), query, computed],
            tables: vec![],
            bindings: vec![],
        };
        let files = generate(&d).unwrap();
        assert!(files["entries.js"].contains("n_place:18446744073709551615n"));
        assert!(files["entries.js"].contains("args_fp:18446744073709551615n"));
        assert!(files["entries.js"].contains("ns(call,[\"m\"])[\"toggle\"]"));
        assert!(files["entries.js"].contains("ns(subscribe,[\"m\"])[\"rows\"]"));
        assert!(files["entries.js"].contains("ns(subscribeDecoded,[\"m\"])[\"rows\"]"));
        assert!(files["entries.js"].contains("ns(subscribe,[\"m\"])[\"total\"]"));
        assert!(files["entries.js"].contains("ns(subscribeDecoded,[\"m\"])[\"total\"]"));
        assert!(files["codec.d.ts"].contains("export type Args0 = bigint"));
        assert!(files["codec.d.ts"].contains("export type Key1_0 = bigint"));
        assert!(files["codec.d.ts"].contains("export type RowValue1_0 = string"));
        assert!(files["codec.d.ts"].contains("export type Value1 = undefined"));
        assert!(files["codec.d.ts"].contains("export type RowValue1_1 = number"));
        assert!(!files["codec.js"].contains("require("));
        assert_eq!(
            serde_json::from_str::<Value>(&files["package.json"]).unwrap()["type"],
            "module"
        );
    }
    #[test]
    fn sdk_describe_and_nominal_schemas() {
        // Describe {digest: [], target: "t", n_place: 17, revision: [], exported: [], tables: [], bindings: []}.
        let bytes = [
            0, 0, 0, 0, 1, 0, 0, 0, b't', 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0,
        ];
        let describe = Describe::read(&bytes).unwrap();
        assert_eq!(describe.target, "t");
        assert_eq!(describe.n_place, 17);
        assert!(describe.exported.is_empty());
        for at in 0..bytes.len() {
            assert!(Describe::read(&bytes[..at]).is_err());
        }
        let mut extra = bytes.to_vec();
        extra.push(0);
        assert!(Describe::read(&extra).is_err());
        // One nominal record R{x:u32}, root reference 0.
        let schema = [
            3, 1, 0, 0, 0, 1, 0, 0, 0, b'R', 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, b'x', 5, 19,
            0, 0, 0, 0,
        ];
        let record = Schema::read(&schema).unwrap();
        assert!(record.declarations("R").contains("\"x\": number"));
        assert!(record.javascript("R").contains("makeCodec([19,0]"));
        // Same definition identity, via u64 instead of a record body.
        let via = [
            3, 1, 0, 0, 0, 1, 0, 0, 0, b'V', 0, 0, 0, 0, 2, 6, 19, 0, 0, 0, 0,
        ];
        assert!(
            Schema::read(&via)
                .unwrap()
                .declarations("V")
                .contains("export type VDef0 = bigint;")
        );
    }
    #[test]
    fn sdk_place_errors_retain_tiny_allowance_capacity_and_duplicate_identity() {
        let capacity = [0, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            place_error(&capacity).unwrap(),
            "PlaceCapacity(Room { requested: 9, available: 1 })"
        );
        let duplicate = [1, 0, 0, 0, 2, 0, 0, 0, 1, 2];
        assert_eq!(
            place_error(&duplicate).unwrap(),
            "AlreadyRegistered([1, 2])"
        );
        assert!(place_error(&capacity[..19]).is_err());
        assert_eq!(place_error(&[2, 0, 0, 0]).unwrap(), "Bounds");
        assert!(place_error(&[3, 0, 0, 0]).is_err());
    }
    #[test]
    fn sdk_describe_result_is_the_complete_returned_codec() {
        fn blob(out: &mut Vec<u8>, value: &[u8]) {
            out.extend_from_slice(&(value.len() as u32).to_le_bytes());
            out.extend_from_slice(value);
        }
        let e = entry("m", "commit");
        let mut bytes = Vec::new();
        blob(&mut bytes, &[]);
        blob(&mut bytes, b"t");
        bytes.extend_from_slice(&17u64.to_le_bytes());
        blob(&mut bytes, &[42; 32]);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        blob(&mut bytes, &e.identity);
        blob(&mut bytes, &e.args);
        blob(&mut bytes, &[3, 0, 0, 0, 0, 17, 6, 13]);
        bytes.extend_from_slice(&e.args_fp.to_le_bytes());
        bytes.extend_from_slice(&e.result_fp.to_le_bytes());
        blob(&mut bytes, &e.revision);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        blob(&mut bytes, &[]);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        let description = Describe::read(&bytes).unwrap();
        assert_eq!(description.revision, [42; 32]);
        assert_eq!(description.exported[0].args_fp, u64::MAX);
        assert_eq!(description.exported[0].result_fp, 7);
        assert_eq!(description.exported[0].revision, e.revision);
        let files = generate(&description).unwrap();
        assert!(files["codec.d.ts"].contains("export type Result0 = Result<bigint, string>"));
        assert!(files["entries.js"].contains("codec.Result0.decode"));
        assert!(!files["codec.js"].contains("export const Error0"));
    }
}
