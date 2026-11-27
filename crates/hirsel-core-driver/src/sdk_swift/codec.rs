use super::{Error, identifier, upper_camel};
use serde_json::Value;
use std::fmt::Write;

pub(super) const CORE: &str = include_str!("Codec.swift");

pub(super) fn emit(
    swift: &mut String,
    schema: &Value,
    name: &str,
    outcome: bool,
) -> Result<Option<(String, String)>, Error> {
    let definitions = array(&schema["definitions"])?;
    let names = definitions
        .iter()
        .enumerate()
        .map(|(index, def)| {
            let nominal = def["name"]
                .as_str()
                .unwrap_or("")
                .rsplit('.')
                .next()
                .unwrap_or("");
            format!("{name}Definition{index}_{}", upper_camel(nominal))
        })
        .collect();
    let mut emitter = Emitter {
        definitions,
        names,
        tuples: Vec::new(),
        tuple_types: Default::default(),
        scope: name,
    };
    let root = emitter.ty(&schema["root"])?;
    let key = schema["key"].as_bool().ok_or("missing codec key mode")?;
    let codec = format!("{name}Codec");
    writeln!(swift, "public typealias {name} = {root}")?;
    writeln!(
        swift,
        "public let {codec} = HirselCodec({root}.self, key: {key})\n"
    )?;
    let result = if outcome && emitter.repr(&schema["root"])[0] == 17 {
        let result = emitter.repr(&schema["root"]).clone();
        Some((emitter.ty(&result[1])?, emitter.ty(&result[2])?))
    } else {
        None
    };
    emitter.definitions(swift)?;
    Ok(result)
}

pub(super) fn callbacks(swift: &mut String) -> Result<(), Error> {
    let definitions: Vec<Value> = serde_json::from_str(include_str!("callbacks.json"))?;
    let names = definitions
        .iter()
        .map(|def| def["name"].as_str().expect("callback name").to_owned())
        .collect();
    let mut emitter = Emitter {
        definitions: &definitions,
        names,
        tuples: Vec::new(),
        tuple_types: Default::default(),
        scope: "Callback",
    };
    for name in &emitter.names {
        writeln!(swift, "public let {name}Codec = HirselCodec({name}.self)")?;
    }
    swift.push('\n');
    emitter.definitions(swift)
}

struct Emitter<'a> {
    definitions: &'a [Value],
    names: Vec<String>,
    tuples: Vec<(String, Vec<String>)>,
    tuple_types: std::collections::BTreeMap<String, String>,
    scope: &'a str,
}

impl Emitter<'_> {
    fn ty(&mut self, expr: &Value) -> Result<String, Error> {
        let atom = match expr[0].as_u64() {
            Some(0) => "HirselUnit",
            Some(1) => "Bool",
            Some(2) => "Unicode.Scalar",
            Some(3) => "UInt8",
            Some(4) => "UInt16",
            Some(5) => "UInt32",
            Some(6) => "UInt64",
            Some(7) => "Int8",
            Some(8) => "Int16",
            Some(9) => "Int32",
            Some(10) => "Int64",
            Some(11) => "Float",
            Some(12) => "Double",
            Some(13) => "String",
            Some(14) if self.repr(&expr[1])[0] == 3 => "Data",
            Some(14) => return Ok(format!("Array<{}>", self.ty(&expr[1])?)),
            Some(15) => {
                return Ok(format!(
                    "HirselMap<{}, {}>",
                    self.ty(&expr[1])?,
                    self.ty(&expr[2])?
                ));
            }
            Some(16) => {
                let option = if self.repr(&expr[1])[0] == 16 {
                    "HirselNestedOption"
                } else {
                    "Optional"
                };
                return Ok(format!("{option}<{}>", self.ty(&expr[1])?));
            }
            Some(17) => {
                return Ok(format!(
                    "HirselResult<{}, {}>",
                    self.ty(&expr[1])?,
                    self.ty(&expr[2])?
                ));
            }
            Some(18) => {
                let identity = expr.to_string();
                if let Some(name) = self.tuple_types.get(&identity) {
                    return Ok(name.clone());
                }
                let fields = self.fields(&expr[1])?;
                let name = format!("{}Tuple{}", self.scope, self.tuples.len());
                self.tuples.push((name.clone(), fields));
                self.tuple_types.insert(identity, name.clone());
                return Ok(name);
            }
            Some(19) => {
                return self
                    .names
                    .get(index(expr)?)
                    .cloned()
                    .ok_or_else(|| "invalid codec definition".into());
            }
            Some(20) => "Never",
            Some(21) => return Ok(format!("HirselDescending<{}>", self.ty(&expr[1])?)),
            _ => return Err("invalid codec opcode".into()),
        };
        Ok(atom.into())
    }

    fn repr<'a>(&'a self, mut expr: &'a Value) -> &'a Value {
        while expr[0] == 19 {
            let Some(def) = index(expr).ok().and_then(|i| self.definitions.get(i)) else {
                break;
            };
            if def["kind"] != 2 {
                break;
            }
            expr = &def["body"];
        }
        expr
    }

    fn fields(&mut self, fields: &Value) -> Result<Vec<String>, Error> {
        array(fields)?.iter().map(|expr| self.ty(expr)).collect()
    }

    fn definitions(&mut self, swift: &mut String) -> Result<(), Error> {
        for (index, def) in self.definitions.iter().enumerate() {
            let name = self.names[index].clone();
            match def["kind"].as_u64() {
                Some(0) => {
                    let fields = array(&def["body"])?
                        .iter()
                        .filter(|field| field["removed"] != true)
                        .map(|field| {
                            Ok((identifier(text(&field["name"])?), self.ty(&field["type"])?))
                        })
                        .collect::<Result<Vec<_>, Error>>()?;
                    record(swift, &name, &fields)?;
                }
                Some(1) => {
                    let variants = array(&def["body"])?
                        .iter()
                        .enumerate()
                        .filter(|(_, variant)| variant["removed"] != true)
                        .map(|(tag, variant)| {
                            let name = text(&variant["name"])?;
                            let case = name.chars().next().map_or_else(String::new, |first| {
                                format!(
                                    "{}{}",
                                    first.to_ascii_lowercase(),
                                    &name[first.len_utf8()..]
                                )
                            });
                            Ok((tag, identifier(&case), self.fields(&variant["fields"])?))
                        })
                        .collect::<Result<Vec<_>, Error>>()?;
                    sum(swift, &name, &variants)?;
                }
                Some(2) => writeln!(
                    swift,
                    "public typealias {name} = {}\n",
                    self.ty(&def["body"])?
                )?,
                _ => return Err("invalid codec definition kind".into()),
            }
        }
        for (name, fields) in &self.tuples {
            let fields: Vec<_> = fields
                .iter()
                .enumerate()
                .map(|(index, ty)| (format!("item{index}"), ty.clone()))
                .collect();
            record(swift, name, &fields)?;
        }
        Ok(())
    }
}

fn record(swift: &mut String, name: &str, fields: &[(String, String)]) -> std::fmt::Result {
    writeln!(
        swift,
        "public struct {name}: Sendable, Equatable, WireValue {{"
    )?;
    for (field, ty) in fields {
        writeln!(swift, "    public let {field}: {ty}")?;
    }
    writeln!(
        swift,
        "    public init({}) {{",
        fields
            .iter()
            .enumerate()
            .map(|(i, (f, t))| format!("{f} field{i}: {t}"))
            .collect::<Vec<_>>()
            .join(", ")
    )?;
    for (index, (field, _)) in fields.iter().enumerate() {
        writeln!(swift, "        self.{field} = field{index}")?;
    }
    writeln!(
        swift,
        "    }}\n    static func read(_ reader: inout WireReader) throws -> Self {{"
    )?;
    for (index, (_, ty)) in fields.iter().enumerate() {
        writeln!(
            swift,
            "        let field{index} = try readWire({ty}.self, &reader)"
        )?;
    }
    writeln!(
        swift,
        "        return Self({})\n    }}",
        fields
            .iter()
            .enumerate()
            .map(|(i, (f, _))| format!("{}: field{i}", f.trim_matches('`')))
            .collect::<Vec<_>>()
            .join(", ")
    )?;
    writeln!(
        swift,
        "    func write(_ writer: inout WireWriter) throws {{"
    )?;
    for (field, _) in fields {
        writeln!(swift, "        try writeWire(self.{field}, &writer)")?;
    }
    writeln!(swift, "    }}\n}}\n")
}

fn sum(
    swift: &mut String,
    name: &str,
    variants: &[(usize, String, Vec<String>)],
) -> std::fmt::Result {
    writeln!(
        swift,
        "public indirect enum {name}: Sendable, Equatable, WireValue {{"
    )?;
    for (_, case, fields) in variants {
        writeln!(swift, "    case {case}{}", associated(fields.join(", ")))?;
    }
    writeln!(
        swift,
        "    static func read(_ reader: inout WireReader) throws -> Self {{\n        switch try reader.count() {{"
    )?;
    for (tag, case, fields) in variants {
        writeln!(swift, "        case {tag}:")?;
        for (index, ty) in fields.iter().enumerate() {
            writeln!(
                swift,
                "            let field{index} = try readWire({ty}.self, &reader)"
            )?;
        }
        writeln!(
            swift,
            "            return .{case}{}",
            associated(bindings(fields.len(), ""))
        )?;
    }
    writeln!(
        swift,
        "        default: throw HirselCodecError.invalidTag\n        }}\n    }}"
    )?;
    if variants.is_empty() {
        writeln!(
            swift,
            "    func write(_ writer: inout WireWriter) throws {{}}\n}}\n"
        )?;
    } else {
        writeln!(
            swift,
            "    func write(_ writer: inout WireWriter) throws {{\n        switch self {{"
        )?;
        for (tag, case, fields) in variants {
            writeln!(
                swift,
                "        case .{case}{}:\n            writer.word(UInt32({tag}))",
                associated(bindings(fields.len(), "let "))
            )?;
            for index in 0..fields.len() {
                writeln!(swift, "            try writeWire(field{index}, &writer)")?;
            }
        }
        writeln!(swift, "        }}\n    }}\n}}\n")?;
    }
    Ok(())
}

fn associated(args: String) -> String {
    if args.is_empty() {
        args
    } else {
        format!("({args})")
    }
}

fn bindings(count: usize, prefix: &str) -> String {
    (0..count)
        .map(|i| format!("{prefix}field{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn array(value: &Value) -> Result<&[Value], Error> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| "expected codec array".into())
}

pub(super) fn text(value: &Value) -> Result<&str, Error> {
    value.as_str().ok_or_else(|| "expected codec name".into())
}

fn index(expr: &Value) -> Result<usize, Error> {
    Ok(usize::try_from(
        expr[1].as_u64().ok_or("invalid codec reference")?,
    )?)
}
