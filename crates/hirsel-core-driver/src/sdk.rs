//! TypeScript bindings from the runner's describe output and codec schemas.
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

type Error = Box<dyn std::error::Error>;
pub const RUNTIME: &str = include_str!("sdk_codec.js");
const DECLARATIONS: &str = include_str!("sdk_codec.d.ts");

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(n).ok_or("Malformed: length")?;
        let out = self.bytes.get(self.at..end).ok_or("Malformed: truncated")?;
        self.at = end;
        Ok(out)
    }
    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn count(&mut self) -> Result<usize, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?) as usize)
    }
    fn word(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn bytes(&mut self) -> Result<Vec<u8>, Error> {
        let n = self.count()?;
        Ok(self.take(n)?.to_vec())
    }
    fn name(&mut self) -> Result<String, Error> {
        Ok(String::from_utf8(self.bytes()?)?)
    }
    fn done(&self) -> Result<(), Error> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err("Malformed: trailing bytes".into())
        }
    }
    fn ty(&mut self) -> Result<Value, Error> {
        let op = self.byte()?;
        Ok(match op {
            0..=13 => json!([op]),
            14 | 16 => json!([op, self.ty()?]),
            15 | 17 => json!([op, self.ty()?, self.ty()?]),
            18 => {
                let n = self.count()?;
                let mut types = Vec::new();
                for _ in 0..n {
                    types.push(self.ty()?);
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
            let args = r.count()?;
            for _ in 0..args {
                let _ = r.ty()?;
            }
            let kind = r.byte()?;
            let body = match kind {
                0 => {
                    let n = r.count()?;
                    let mut fields = Vec::new();
                    for _ in 0..n {
                        fields
                            .push(json!({"removed":r.removed()?,"name":r.name()?,"type":r.ty()?}));
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
                            fields.push(r.ty()?);
                        }
                        variants.push(json!({"removed":removed,"name":name,"fields":fields}));
                    }
                    json!(variants)
                }
                2 => r.ty()?,
                _ => return Err("Malformed: schema definition kind".into()),
            };
            definitions.push(json!({"name":name,"kind":kind,"body":body}));
        }
        let root = r.ty()?;
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
                "Map<{}, {}>",
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
}
pub struct Describe {
    pub digest: Vec<u8>,
    pub target: String,
    pub n_place: u64,
    pub exported: Vec<Exported>,
}
impl Describe {
    pub fn read(bytes: &[u8]) -> Result<Self, Error> {
        let mut r = Reader { bytes, at: 0 };
        let digest = r.bytes()?;
        let target = r.name()?;
        let n_place = r.word()?;
        let n = r.count()?;
        let mut exported = Vec::new();
        for _ in 0..n {
            let kind = r.count()? as u32;
            if kind > 3 {
                return Err("Malformed: entry kind".into());
            }
            exported.push(Exported {
                kind,
                identity: r.bytes()?,
                args: r.bytes()?,
                result: r.bytes()?,
                args_fp: r.word()?,
                result_fp: r.word()?,
                revision: r.bytes()?,
            });
        }
        r.done()?;
        Ok(Self {
            digest,
            target,
            n_place,
            exported,
        })
    }
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
        _ => return Err("Malformed: describe error tag".into()),
    };
    r.done()?;
    Ok(message)
}

pub(crate) fn stub_name(identity: &[u8]) -> Result<String, Error> {
    // PROG-9: version 01 followed by UTF-8 D S, with S beginning at '('.
    if identity.first() != Some(&1) {
        return Err("Malformed: entry identity version".into());
    }
    let text = std::str::from_utf8(&identity[1..])?;
    let (path, _) = text.split_once('(').ok_or("Malformed: entry signature")?;
    let name = path
        .rsplit('/')
        .next()
        .ok_or("Malformed: definition identity")?;
    let mut out = Vec::new();
    let mut bytes = name.as_bytes().iter().copied();
    while let Some(b) = bytes.next() {
        out.push(if b == b'%' {
            let hi = bytes.next().ok_or("Malformed: percent escape")?;
            let lo = bytes.next().ok_or("Malformed: percent escape")?;
            u8::from_str_radix(std::str::from_utf8(&[hi, lo])?, 16)?
        } else {
            b
        });
    }
    Ok(String::from_utf8(out)?)
}

/// Complete all generation before touching the requested output directory.
pub fn generate(describe: &Describe) -> Result<BTreeMap<String, String>, Error> {
    let mut names = BTreeMap::new();
    for entry in &describe.exported {
        let name = stub_name(&entry.identity)?;
        if let Some(old) = names.insert(name, &entry.identity) {
            return Err(format!("DuplicateStubName({old:?}, {:?})", entry.identity).into());
        }
    }
    let mut js = RUNTIME.to_owned();
    let mut dts = DECLARATIONS.to_owned();
    let mut entries = "import * as codec from './codec.js';\n".to_owned();
    entries.push_str(&format!("export const identity = Object.freeze({{digest:Uint8Array.from({}),target:{},n_place:{}n}});\n",json!(describe.digest),json!(describe.target),describe.n_place));
    entries.push_str("export const want = [\n");
    for entry in &describe.exported {
        entries.push_str(&format!(
            "{{identity:Uint8Array.from({}),args_fp:{}n,result_fp:{}n}},\n",
            json!(entry.identity),
            entry.args_fp,
            entry.result_fp
        ));
    }
    entries.push_str("];\nexport function bind(client) {\nconst call = Object.create(null), subscribe = Object.create(null);\n");
    let mut calls = Vec::new();
    let mut subscriptions = Vec::new();
    for (i, entry) in describe.exported.iter().enumerate() {
        let args = Schema::read(&entry.args)?;
        let result = Schema::read(&entry.result)?;
        let arg_name = format!("Args{i}");
        let result_name = format!("Result{i}");
        js.push_str(&args.javascript(&arg_name));
        js.push_str(&result.javascript(&result_name));
        dts.push_str(&args.declarations(&arg_name));
        dts.push_str(&result.declarations(&result_name));
        let name = json!(stub_name(&entry.identity)?);
        if entry.kind == 2 {
            entries.push_str(&format!("subscribe[{name}] = (args,onRows,onEnd) => client.subscribe(want[{i}],codec.{arg_name}.encode(args),onRows,onEnd);\n"));
            subscriptions.push(format!(
                "{name}: (args: codec.{arg_name}, onRows: (rows: Row[]) => void, onEnd?: (reason: SubscriptionEnd) => void) => Subscription"
            ));
        } else {
            entries.push_str(&format!("call[{name}] = (args) => client.call(want[{i}],codec.{arg_name}.encode(args),codec.{result_name}.decode);\n"));
            calls.push(format!(
                "{name}: (args: codec.{arg_name}) => Promise<Outcome<codec.{result_name}>>"
            ));
        }
    }
    entries.push_str("return {call,subscribe};\n}\n");
    let entry_dts = format!(
        "import type * as codec from './codec.js';\nimport type {{Outcome, Row, Subscription, SubscriptionEnd, Transport}} from '@hirsel/client';\nexport const identity: {{readonly digest: Uint8Array; readonly target: string; readonly n_place: bigint}};\nexport const want: Array<{{identity: Uint8Array; args_fp: bigint; result_fp: bigint}}>;\nexport function bind(client: Transport): {{call: {{ {} }}; subscribe: {{ {} }} }};\n",
        calls.join("; "),
        subscriptions.join("; ")
    );
    Ok(BTreeMap::from([
        ("package.json".into(), "{\"type\":\"module\"}\n".into()),
        ("codec.js".into(), js),
        ("codec.d.ts".into(), dts),
        ("entries.js".into(), entries),
        ("entries.d.ts".into(), entry_dts),
    ]))
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
    for name in ["Outcome", "Fault", "Refusal"] {
        let index = schema
            .definitions
            .iter()
            .position(|d| {
                d["name"]
                    .as_str()
                    .is_some_and(|path| path.rsplit(['.', '/']).next() == Some(name))
            })
            .ok_or_else(|| format!("Frame schema has no {name}"))?;
        files.get_mut("codec.js").unwrap().push_str(&format!(
            "export const {name} = makeCodec([19,{index}],{});\n",
            json!(schema.definitions)
        ));
        files.get_mut("codec.d.ts").unwrap().push_str(&format!(
            "export type {name} = FrameDef{index};\nexport const {name}: Codec<{name}>;\n"
        ));
    }
    files.insert("frame.js".into(),"export {Frame,Outcome,Fault,Refusal} from './codec.js';\nexport const subprotocol = 'hirsel.1';\nexport const Raised = Object.freeze({Overflow:1,Bounds:2,Division:3,Unwrap:4,Fail:5,Cancelled:6,Capacity:7,OutOfTime:8,Tape:9});\nexport const BadKind = Object.freeze({Truncated:0,Invalid:1,Utf8:2,Duplicate:3,Trailing:4,Schema:5,Value:6});\n".into());
    files.insert("frame.d.ts".into(),"export {Frame,Outcome,Fault,Refusal} from './codec.js';\nexport const subprotocol: 'hirsel.1';\nexport const Raised: Readonly<{Overflow:1; Bounds:2; Division:3; Unwrap:4; Fail:5; Cancelled:6; Capacity:7; OutOfTime:8; Tape:9}>;\nexport const BadKind: Readonly<{Truncated:0; Invalid:1; Utf8:2; Duplicate:3; Trailing:4; Schema:5; Value:6}>;\n".into());
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
            (vec![15, 13, 6], "Map<string, bigint>"),
            (vec![16, 16, 5], "Option<Option<number>>"),
            (vec![17, 6, 13], "Result<bigint, string>"),
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
    fn sdk_duplicate_stub_name_writes_nothing() {
        let d = Describe {
            digest: vec![1],
            target: "t".into(),
            n_place: 16,
            exported: vec![entry("a", "toggle"), entry("b", "toggle")],
        };
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("absent");
        assert!(
            write(&d, &out)
                .unwrap_err()
                .to_string()
                .starts_with("DuplicateStubName(")
        );
        assert!(!out.exists());
        assert_eq!(
            stub_name(&entry("x", "%74oggle").identity).unwrap(),
            "toggle"
        );
        assert_eq!(
            stub_name(b"\x01store@1/runner_launch/echo(take u64)->u64").unwrap(),
            "echo"
        );
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
        query.result = scalar(13);
        let d = Describe {
            digest: vec![1, 2],
            target: "x86_64-unknown-linux-gnu".into(),
            n_place: u64::MAX,
            exported: vec![entry("m", "toggle"), query],
        };
        let files = generate(&d).unwrap();
        assert!(files["entries.js"].contains("n_place:18446744073709551615n"));
        assert!(files["entries.js"].contains("args_fp:18446744073709551615n"));
        assert!(files["entries.js"].contains("call[\"toggle\"]"));
        assert!(files["entries.js"].contains("subscribe[\"rows\"]"));
        assert!(files["codec.d.ts"].contains("export type Args0 = bigint"));
        assert!(files["codec.d.ts"].contains("export type Result1 = string"));
        assert!(!files["codec.js"].contains("require("));
        assert_eq!(
            serde_json::from_str::<Value>(&files["package.json"]).unwrap()["type"],
            "module"
        );
    }
    #[test]
    fn sdk_describe_and_nominal_schemas() {
        // Describe {digest: [], target: "t", n_place: 17, exported: []}.
        let bytes = [
            0, 0, 0, 0, 1, 0, 0, 0, b't', 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
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
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        blob(&mut bytes, &e.identity);
        blob(&mut bytes, &e.args);
        blob(&mut bytes, &[3, 0, 0, 0, 0, 17, 6, 13]);
        bytes.extend_from_slice(&e.args_fp.to_le_bytes());
        bytes.extend_from_slice(&e.result_fp.to_le_bytes());
        blob(&mut bytes, &e.revision);
        let description = Describe::read(&bytes).unwrap();
        assert_eq!(description.exported[0].args_fp, u64::MAX);
        assert_eq!(description.exported[0].result_fp, 7);
        assert_eq!(description.exported[0].revision, e.revision);
        let files = generate(&description).unwrap();
        assert!(files["codec.d.ts"].contains("export type Result0 = Result<bigint, string>"));
        assert!(files["entries.js"].contains("codec.Result0.decode"));
        assert!(!files["codec.js"].contains("export const Error0"));
    }
}
