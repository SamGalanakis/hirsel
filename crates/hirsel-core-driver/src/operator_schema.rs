//! Emit Rust bindings for the operator types that Hirsel's value codec derives.
use hirsel_core_syntax::{NodeId, NodeKind, ParseMode, Symbols, SyntaxFile, parse};
use std::fmt::Write;

pub fn generate(source: &str) -> String {
    let parsed = parse(source, ParseMode::Source, &mut Symbols::default());
    assert!(
        parsed.diagnostics.is_empty() && parsed.resource_refusal.is_none(),
        "cannot parse operator.hirsel: {:?}",
        parsed.diagnostics
    );
    let mut out = String::new();
    let file = &parsed.syntax;
    for item in file.items() {
        if file.tree.node(item.root).kind != NodeKind::TypeDecl {
            continue;
        }
        let schema = Schema {
            file,
            offset: item.file_offset as usize,
        };
        let children = file.tree.children(item.root);
        if !children.iter().any(|&n| {
            schema.kind(n) == NodeKind::Bounds
                && file
                    .tree
                    .children(n)
                    .iter()
                    .any(|&b| schema.name(b) == "codec")
        }) {
            continue;
        }
        let name = schema.name(item.root);
        for &node in children {
            match schema.kind(node) {
                NodeKind::Fields => schema.record(&mut out, name, node),
                NodeKind::Variants => schema.sum(&mut out, name, node),
                NodeKind::Generics | NodeKind::CodecVia => {
                    panic!("unsupported operator codec declaration {name}");
                }
                _ => {}
            }
        }
    }
    out
}

struct Schema<'a> {
    file: &'a SyntaxFile,
    offset: usize,
}

impl<'a> Schema<'a> {
    fn kind(&self, node: NodeId) -> NodeKind {
        self.file.tree.node(node).kind
    }

    fn name(&self, node: NodeId) -> &'a str {
        let token = self.file.tree.node(node).token.expect("schema name");
        self.file.tree.token_text(&self.file.source, token)
    }

    fn ty(&self, node: NodeId) -> String {
        let children = self.file.tree.children(node);
        match self.kind(node) {
            NodeKind::GroupType => self.ty(children[0]),
            NodeKind::TypeArguments => {
                let [head, argument] = children else {
                    panic!("expected one type argument in operator schema");
                };
                let container = match self.ty(*head).as_str() {
                    "List" => "Vec",
                    "Option" => "Option",
                    other => panic!("unsupported operator container {other}"),
                };
                format!("{container}<{}>", self.ty(*argument))
            }
            NodeKind::TypePath => {
                let span = self.file.tree.node(node).origin.span;
                let text = &self.file.source
                    [self.offset + span.start as usize..self.offset + span.end as usize];
                match text {
                    "Bytes" => "Vec<u8>".into(),
                    "launcher.Attempt" => "Attempt".into(),
                    "launcher.Group" => "Group".into(),
                    "launcher.Grants" => "Grants".into(),
                    "launcher.Wanted" => "Needed".into(),
                    "runner.fate.Fates" => "Fates".into(),
                    "runner.TriggerReport" => "TriggerReport".into(),
                    other => other.into(),
                }
            }
            other => panic!("unsupported operator field type {other:?}"),
        }
    }

    fn record(&self, out: &mut String, name: &str, node: NodeId) {
        let fields: Vec<_> = self
            .file
            .tree
            .children(node)
            .iter()
            .map(|&field| {
                let [ty] = self.file.tree.children(field) else {
                    panic!("expected a field without a default in {name}");
                };
                assert_eq!(self.kind(field), NodeKind::Field, "{name}");
                let field = self.name(field);
                let ty = self.ty(*ty);
                // Registration validates that each package root is exactly 32 bytes.
                let ty =
                    if (name, field, ty.as_str()) == ("Registration", "packages", "Vec<Vec<u8>>") {
                        "Vec<[u8; 32]>".into()
                    } else {
                        ty
                    };
                (field, ty)
            })
            .collect();
        let default = match name {
            "Preview" => ", Default",
            "EventPosition" => ", Copy",
            _ => "",
        };
        writeln!(out, "#[derive(Clone, Debug{default}, PartialEq, Eq)]").unwrap();
        writeln!(out, "pub(crate) struct {name} {{").unwrap();
        for (field, ty) in &fields {
            writeln!(out, "    pub(crate) {field}: {ty},").unwrap();
        }
        writeln!(out, "}}\nimpl Wire for {name} {{").unwrap();
        writeln!(out, "    fn put(&self, w: &mut Writer) {{").unwrap();
        for (field, _) in &fields {
            writeln!(out, "        self.{field}.put(w);").unwrap();
        }
        writeln!(
            out,
            "    }}\n    fn get(r: &mut Reader<'_>) -> io::Result<Self> {{"
        )
        .unwrap();
        writeln!(out, "        Ok(Self {{").unwrap();
        for (field, _) in &fields {
            writeln!(out, "            {field}: Wire::get(r)?,").unwrap();
        }
        writeln!(out, "        }})\n    }}\n}}").unwrap();
    }

    fn sum(&self, out: &mut String, name: &str, node: NodeId) {
        let variants: Vec<_> = self
            .file
            .tree
            .children(node)
            .iter()
            .map(|&variant| {
                assert_eq!(self.kind(variant), NodeKind::Variant, "{name}");
                let written = self.name(variant);
                let variant_name = rust_variant(name, written);
                let fields = self
                    .file
                    .tree
                    .children(variant)
                    .iter()
                    .map(|&payload| {
                        let [ty] = self.file.tree.children(payload) else {
                            panic!("expected a payload type in {name}.{written}");
                        };
                        let ty = self.ty(*ty);
                        match (name, written, ty.as_str()) {
                            ("Ask", "RegisterAsk", "Registration")
                            | ("Ask", "DecideAsk", "Decision")
                            | ("Phase", "AwaitDecision", "Decision") => format!("Box<{ty}>"),
                            _ => ty,
                        }
                    })
                    .collect::<Vec<_>>();
                (variant_name, fields)
            })
            .collect();
        let copy = if matches!(
            name,
            "Forward" | "EntryChange" | "Class" | "DecisionStage" | "Health" | "OperationPhase"
        ) {
            ", Copy"
        } else {
            ""
        };
        writeln!(out, "#[derive(Clone{copy}, Debug, PartialEq, Eq)]").unwrap();
        writeln!(out, "pub(crate) enum {name} {{").unwrap();
        for (variant, fields) in &variants {
            writeln!(out, "    {},", constructor(name, variant, fields)).unwrap();
        }
        writeln!(out, "}}\nimpl Wire for {name} {{").unwrap();
        writeln!(
            out,
            "    fn put(&self, w: &mut Writer) {{\n        match self {{"
        )
        .unwrap();
        for (tag, (variant, fields)) in variants.iter().enumerate() {
            let bindings: Vec<_> = (0..fields.len()).map(|i| format!("x{i}")).collect();
            let pattern = constructor(name, variant, &bindings);
            writeln!(
                out,
                "            Self::{pattern} => {{\n                w.u32({tag});"
            )
            .unwrap();
            for binding in &bindings {
                writeln!(out, "                {binding}.put(w);").unwrap();
            }
            writeln!(out, "            }}").unwrap();
        }
        writeln!(
            out,
            "        }}\n    }}\n    fn get(r: &mut Reader<'_>) -> io::Result<Self> {{"
        )
        .unwrap();
        writeln!(out, "        Ok(match r.u32()? {{").unwrap();
        for (tag, (variant, fields)) in variants.iter().enumerate() {
            let reads = vec!["Wire::get(r)?".to_owned(); fields.len()];
            let value = constructor(name, variant, &reads);
            writeln!(out, "            {tag} => Self::{value},").unwrap();
        }
        let error = match name {
            "Forward" => "front forwarding mode".into(),
            "Resolved" => "operator pin tag".into(),
            "EntryChange" => "entry change tag".into(),
            "DecisionStage" => "decision stage tag".into(),
            "BeginInput" => "operator begin input tag".into(),
            _ => format!("operator {} tag", name.to_ascii_lowercase()),
        };
        writeln!(out, "            _ => return Err(malformed({error:?})),").unwrap();
        writeln!(out, "        }})\n    }}\n}}").unwrap();
    }
}

fn rust_variant<'a>(ty: &str, written: &'a str) -> &'a str {
    if (ty, written) == ("Class", "NotInstalledClass") {
        return "NotInstalledAfterRecovery";
    }
    if ty == "EntryChange" {
        return written.strip_prefix("Entry").unwrap_or(written);
    }
    let suffix = match ty {
        "Input" => "Input",
        "BeginInput" => "Begin",
        "DecisionStage" => "Stage",
        other => other,
    };
    written.strip_suffix(suffix).unwrap_or(written)
}

fn constructor(ty: &str, variant: &str, fields: &[String]) -> String {
    if (ty, variant) == ("BeginInput", "Source") {
        let [manifest, gate] = fields else {
            panic!("expected a manifest and gate in SourceBegin");
        };
        // Preserve the named fields in the CLI's existing representation.
        format!("{variant} {{ manifest: {manifest}, gate: {gate} }}")
    } else if fields.is_empty() {
        variant.into()
    } else {
        format!("{variant}({})", fields.join(", "))
    }
}
