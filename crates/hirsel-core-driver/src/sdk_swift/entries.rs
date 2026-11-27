use super::{Error, Sha256, Slice, codec, identifier};
use hirsel_core_ids::embed::ABI_VERSION;
use serde_json::Value;
use sha2::Digest;
use std::collections::BTreeMap;
use std::fmt::Write;

pub(super) fn render(projection: &[u8], slices: &[Slice]) -> Result<(String, String), Error> {
    let projection_json: Value = serde_json::from_slice(projection)?;
    let mut codecs = String::from(codec::CORE);
    codec::callbacks(&mut codecs)?;
    let mut swift = String::from("import Foundation\n\n");
    writeln!(swift, "let embedABI: UInt32 = {ABI_VERSION}")?;
    writeln!(
        swift,
        "public let manifest_fp = Data({:?})\n",
        Sha256::digest(projection).as_slice()
    )?;
    writeln!(
        swift,
        "public struct Want: Equatable, Sendable {{\n    public let identity: Data\n    public let argsFingerprint: UInt64\n    public let resultFingerprint: UInt64\n}}"
    )?;
    writeln!(swift, "public let wants: [Want] = [")?;
    let mut trees: BTreeMap<&str, Module<'_>> = BTreeMap::new();
    for (index, entry) in codec::array(&projection_json["entries"])?
        .iter()
        .enumerate()
    {
        let id: Vec<u8> = serde_json::from_value(entry["identity"].clone())?;
        let args_fp: u64 = codec::text(&entry["args_fp"])?.parse()?;
        let result_fp: u64 = codec::text(&entry["result_fp"])?.parse()?;
        writeln!(
            swift,
            "    Want(identity: Data({id:?}), argsFingerprint: {args_fp}, resultFingerprint: {result_fp}),"
        )?;
        let group = match codec::text(&entry["kind"])? {
            "compute" | "transact" | "granted" => "call",
            "query" => "subscribe",
            "room" => "room",
            _ => return Err("unknown SDK entry kind".into()),
        };
        let prefix = format!("Entry{index}");
        codec::emit(&mut codecs, &entry["args"], &format!("{prefix}Args"), false)?;
        let outcome = codec::emit(
            &mut codecs,
            &entry["result"],
            &format!("{prefix}Result"),
            group == "call",
        )?;
        for (slot, schema) in codec::array(&entry["slots"])?.iter().enumerate() {
            for (field, suffix) in [("key", "Key"), ("key_json", "KeyValue"), ("value", "Row")] {
                codec::emit(
                    &mut codecs,
                    &schema[field],
                    &format!("{prefix}Slot{slot}{suffix}"),
                    false,
                )?;
            }
        }
        if group == "room" {
            for (field, suffix) in [("state", "State"), ("message", "Message")] {
                codec::emit(
                    &mut codecs,
                    &entry["room"][field],
                    &format!("{prefix}{suffix}"),
                    false,
                )?;
            }
        }
        let mut module = trees.entry(group).or_default();
        for part in codec::array(&entry["module"])? {
            module = module.children.entry(codec::text(part)?).or_default();
        }
        module.entries.push(Entry {
            index,
            name: codec::text(&entry["name"])?,
            group,
            outcome,
        });
    }
    writeln!(swift, "]\n")?;
    let mut api = String::from("public struct API: Sendable {\n    let session: Session\n");
    let mut module_index = 0;
    for (group, root) in trees {
        let name = root.emit(&mut swift, &mut module_index)?;
        writeln!(
            api,
            "    public var {group}: {name} {{ {name}(__hirselSession: session) }}"
        )?;
    }
    writeln!(api, "}}")?;
    swift.push_str(&api);

    for slice in slices {
        let segment = identifier(slice.apple.xcframework_slice);
        writeln!(swift, "let {segment} = Data({:?})", slice.digest)?;
    }
    writeln!(swift, "\nfunc requireEmbedAnchor() -> Data {{")?;
    for (i, slice) in slices.iter().enumerate() {
        let conditional = if i == 0 { "#if" } else { "#elseif" };
        let os = slice.apple.swiftpm_platform;
        let variant = match (slice.apple.family, slice.apple.llvm_environment) {
            ("ios", "simulator") => " && targetEnvironment(simulator)",
            ("ios", _) => " && !targetEnvironment(simulator)",
            _ => "",
        };
        writeln!(swift, "    {conditional} os({os}){variant}")?;
        // Entries.swift also imports the module that supplies this slice's header.
        writeln!(
            swift,
            "    hirsel_embed_v1_require_anchor_{}()",
            slice.anchor
        )?;
        writeln!(
            swift,
            "    return {}",
            identifier(slice.apple.xcframework_slice)
        )?;
    }
    writeln!(
        swift,
        "    #else\n    #error(\"This package requires an Apple XCFramework slice\")\n    #endif\n}}"
    )?;
    Ok((codecs, swift))
}

#[derive(Default)]
struct Module<'a> {
    children: BTreeMap<&'a str, Module<'a>>,
    entries: Vec<Entry<'a>>,
}

impl Module<'_> {
    fn emit(&self, swift: &mut String, index: &mut usize) -> Result<String, Error> {
        let name = format!("EntryModule{index}");
        *index += 1;
        let children = self
            .children
            .iter()
            .map(|(part, child)| Ok((identifier(part), child.emit(swift, index)?)))
            .collect::<Result<Vec<_>, Error>>()?;
        writeln!(
            swift,
            "public struct {name}: Sendable {{\n    let __hirselSession: Session"
        )?;
        for (part, child) in children {
            writeln!(
                swift,
                "    public var {part}: {child} {{ {child}(__hirselSession: __hirselSession) }}"
            )?;
        }
        for entry in &self.entries {
            entry.emit(swift)?;
        }
        writeln!(swift, "}}\n")?;
        Ok(name)
    }
}

struct Entry<'a> {
    index: usize,
    name: &'a str,
    group: &'a str,
    outcome: Option<(String, String)>,
}

impl Entry<'_> {
    fn emit(&self, swift: &mut String) -> std::fmt::Result {
        let name = identifier(self.name);
        let index = self.index;
        let prefix = format!("Entry{index}");
        match self.group {
            "call" => {
                let (value, error, decode) = match &self.outcome {
                    Some((value, error)) => (value.clone(), error.as_str(), "decodeResultOutcome"),
                    None => (format!("{prefix}Result"), "Never", "decodeOutcome"),
                };
                writeln!(
                    swift,
                    "    public func {name}(_ args: {prefix}Args, deadline: UInt32) async throws -> Outcome<{value}, {error}> {{"
                )?;
                writeln!(
                    swift,
                    "        let bytes = try await __hirselSession.call({index}, {prefix}ArgsCodec.encode(args), deadline: deadline)"
                )?;
                writeln!(
                    swift,
                    "        return try {decode}(bytes, {prefix}ResultCodec)\n    }}"
                )
            }
            "subscribe" => {
                writeln!(
                    swift,
                    "    public func {name}(_ args: {prefix}Args, buffer: Int = 1_024) -> AsyncThrowingStream<Change, Error> {{"
                )?;
                writeln!(
                    swift,
                    "        do {{ return try __hirselSession.subscribe({index}, {prefix}ArgsCodec.encode(args), buffer: buffer) }}"
                )?;
                writeln!(
                    swift,
                    "        catch {{ return AsyncThrowingStream {{ $0.finish(throwing: error) }} }}\n    }}"
                )
            }
            _ => {
                writeln!(
                    swift,
                    "    public func {name}(_ args: {prefix}Args, buffer: Int = 1_024) throws -> Room<{prefix}State, {prefix}Message> {{"
                )?;
                writeln!(
                    swift,
                    "        return try __hirselSession.join({index}, {prefix}ArgsCodec.encode(args), buffer: buffer, state: {prefix}StateCodec, message: {prefix}MessageCodec)\n    }}"
                )
            }
        }
    }
}
