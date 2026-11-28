use super::config::schemas;
use super::documents::{Encoding, Position, Text};
use hirsel_core_syntax::{
    Item, NodeId, NodeKind, Span, Symbols, SyntaxFile, TokenId, TokenKind, lex, parse_data_file,
};
use hirsel_data::{Body, Descriptor, Field, Fields, Limits, Scalar, Shape};
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn hover(
    path: &Path,
    text: &Text,
    position: Position,
    encoding: Encoding,
) -> Option<Value> {
    let offset = text.offset(position, encoding)? as u32;
    let syntax = parse(&text.bytes)?;
    let (root, schema) = document(path, &syntax)?;
    let Context::Fields {
        owner,
        fields,
        label: Some((name, span)),
        ..
    } = root.record(
        schema.root(),
        root.args(),
        root_fields(schema),
        schema,
        offset,
    )?
    else {
        return None;
    };
    let field = fields.iter().find(|field| field.name == name)?;
    let detail = type_name(&field.shape, schema);
    let description = description(owner, field, schema);
    Some(json!({
        "contents": {
            "kind": "markdown",
            "value": format!("`{name}: {detail}`\n\n{description}")
        },
        "range": range(text, span, encoding)?,
    }))
}

pub(super) fn complete(
    path: &Path,
    text: &Text,
    position: Position,
    encoding: Encoding,
) -> Option<Value> {
    if text.bytes.len() > Limits::default().input_bytes {
        return None;
    }
    let offset = text.offset(position, encoding)?;
    let source = std::str::from_utf8(&text.bytes).ok()?;
    let scanned = lex(source, &mut Symbols::default());
    let (mut start, mut end) = (offset, offset);
    for token in &scanned.tokens {
        let (lo, hi) = (token.span.start as usize, token.span.end as usize);
        if lo <= offset && offset <= hi && token.kind == TokenKind::Ident {
            (start, end) = (lo, hi);
            break;
        }
        if lo < offset && (offset < hi || offset == hi && token.kind.is_trivia()) {
            return None;
        }
    }
    let prefix = &source[start..offset];
    let replace = range(
        text,
        Span {
            start: start as u32,
            end: end as u32,
        },
        encoding,
    )?;
    let next = scanned
        .tokens
        .iter()
        .find(|token| !token.kind.is_trivia() && token.span.start as usize >= end);
    let has_colon = next.is_some_and(|token| token.kind == TokenKind::Colon);

    // Replace the cursor token so the data parser can identify its expected slot.
    let marker = "hirsel_editor_cursor";
    let mut repaired = format!("{}{marker}", &source[..start]);
    let prefix_end = repaired.len();
    repaired.push_str(&source[end..]);
    let syntax = parse(repaired.as_bytes()).or_else(|| {
        // An unfinished document can still identify the slot up to the cursor.
        repaired.truncate(prefix_end);
        let mut closing = Vec::new();
        for token in scanned
            .tokens
            .iter()
            .take_while(|token| (token.span.end as usize) <= start)
        {
            match token.kind {
                TokenKind::LParen => closing.push(')'),
                TokenKind::LBracket => closing.push(']'),
                TokenKind::RParen if closing.pop() != Some(')') => return None,
                TokenKind::RBracket if closing.pop() != Some(']') => return None,
                _ => {}
            }
        }
        repaired.extend(closing.into_iter().rev());
        parse(repaired.as_bytes())
    })?;
    let (root, schema) = document(path, &syntax)?;
    let context = root.record(
        schema.root(),
        root.args(),
        root_fields(schema),
        schema,
        start as u32,
    )?;
    let mut items = Vec::new();
    let mut item = |name: &str, kind: u8, suffix: &str, detail: &str| {
        if name.starts_with(prefix) {
            items.push(json!({
                "label": name,
                "kind": kind,
                "detail": detail,
                "textEdit": {"range": replace, "newText": format!("{name}{suffix}")},
            }));
        }
    };
    match context {
        Context::Fields { fields, args, .. } => {
            for field in fields {
                if !args
                    .iter()
                    .any(|arg| arg.label().is_some_and(|(name, _)| name == field.name))
                {
                    item(
                        &field.name,
                        5,
                        if has_colon { "" } else { ": " },
                        &type_name(&field.shape, schema),
                    );
                }
            }
        }
        Context::Value(Shape::Option(_)) => {
            item("None", 20, "", "Option");
            item("Some", 20, "", "Option");
        }
        Context::Value(Shape::Named(index)) => {
            let nominal = &schema.types()[*index];
            if let Body::Sum(cases) = &nominal.body {
                for case in cases {
                    item(&case.name, 20, "", &nominal.name);
                }
            }
        }
        Context::Value(Shape::Scalar(Scalar::Bool)) => {
            item("false", 12, "", "bool");
            item("true", 12, "", "bool");
        }
        Context::Value(_) => {}
    }
    Some(json!(items))
}

fn parse(bytes: &[u8]) -> Option<SyntaxFile> {
    (bytes.len() <= Limits::default().input_bytes)
        .then(|| parse_data_file(bytes).ok())
        .flatten()
}

fn document<'a>(path: &Path, syntax: &'a SyntaxFile) -> Option<(Site<'a>, &'static Descriptor)> {
    let item = syntax.item(0);
    let root = Site(item, item.root);
    let head = root.children().next()?;
    let tokens = head
        .node()
        .origin
        .tokens
        .clone()
        .map(TokenId)
        .filter(|token| !item.tree.tokens[token.0 as usize].kind.is_trivia())
        .map(|token| item.tree.token_text(item.text, token))
        .collect::<Vec<_>>();
    let [module, ".", name] = tokens.as_slice() else {
        return None;
    };
    let schema = schemas(path)
        .into_iter()
        .find(|schema| schema.module() == *module && schema.root() == *name)?;
    Some((root, schema))
}

fn root_fields(schema: &Descriptor) -> &[Field] {
    let Body::Record(fields) = &schema.types()[0].body else {
        unreachable!()
    };
    fields
}

fn range(text: &Text, span: Span, encoding: Encoding) -> Option<Value> {
    Some(json!({
        "start": text.position(span.start, encoding)?,
        "end": text.position(span.end, encoding)?,
    }))
}

fn type_name(shape: &Shape, schema: &Descriptor) -> String {
    match shape {
        Shape::Scalar(scalar) => scalar.name().to_owned(),
        Shape::Unit => "()".to_owned(),
        Shape::List(element) => format!("List[{}]", type_name(element, schema)),
        Shape::Map(value) => format!("Map[String, {}]", type_name(value, schema)),
        Shape::Option(value) => format!("Option[{}]", type_name(value, schema)),
        Shape::Tuple(elements) => format!(
            "({})",
            elements
                .iter()
                .map(|element| type_name(element, schema))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Shape::Named(index) => schema.types()[*index].name.clone(),
    }
}

fn description(owner: &str, field: &Field, schema: &Descriptor) -> String {
    let mut description = match (&field.default, &field.shape) {
        (Some(default), _) => format!("Optional. Defaults to `{default}`."),
        (_, Shape::Option(_)) => "Optional. Omission uses `None`.".to_owned(),
        _ => "Required.".to_owned(),
    };
    description.push_str(&format!(" Field of `{}.{owner}`.", schema.module()));
    match &field.shape {
        Shape::Map(_) => description
            .push_str(" Write a list of `(\"key\", value)` pairs with unique string keys."),
        Shape::Named(index) => {
            if let Body::Sum(cases) = &schema.types()[*index].body {
                let names = cases
                    .iter()
                    .map(|case| format!("`{}`", case.name))
                    .collect::<Vec<_>>();
                description.push_str(&format!(" Accepted cases are {}.", names.join(", ")));
            }
        }
        _ => {}
    }
    if let Some(purpose) = field_purpose(schema.module(), owner, &field.name) {
        description.push_str("\n\n");
        description.push_str(purpose);
    }
    description
}

fn field_purpose(module: &str, owner: &str, field: &str) -> Option<&'static str> {
    Some(match (module, owner, field) {
        ("pkg_v1", "Package", "name") => "The package identity, including its scope.",
        ("pkg_v1", "Package", "version") => "The package version in major.minor.patch form.",
        ("pkg_v1", "Package", "sources") => "Maps package-relative source paths to module names.",
        ("pkg_v1", "Package", "dependencies") => "Maps import aliases to dependency bindings.",
        ("pkg_v1", "Package", "charge") => "Selects the charge policy for the package.",
        ("pkg_v1", "Workspace", "members") => "Package directories relative to the workspace root.",
        ("pkg_v1", "Workspace", "root_package") => {
            "A package defined in the workspace root directory."
        }
        ("user_v1", "Config", "contexts") => {
            "Named operator contexts for selecting a host and principal."
        }
        ("user_v1", "Config", "routes") => "Registry routes for matching package name prefixes.",
        ("user_v1", "Config", "cache") => "Settings for the remote artifact cache.",
        ("host_v1", "Admission", "recipe_bytes") => "Maximum encoded size of an admitted recipe.",
        ("host_v1", "Admission", "members") => "Maximum number of members in an admitted closure.",
        ("host_v1", "Group", "grants") => "Capabilities granted to the running group.",
        ("exec_v1", "Policy", "profiles") => "Named execution profiles available to an Exec grant.",
        ("exec_v1", "Profile", "args") => "Accepted argument forms for calls to this profile.",
        ("toolchain_v1", "Packages", "packages") => {
            "Ordered package inventory for the installed toolchain."
        }
        ("product_v1", "Product", "hosts") => "Host targets supported by this distribution.",
        ("workloads_v1", "Config", "runs") => "Named runs selected by the workload tool.",
        _ => return None,
    })
}

enum Context<'a, 's> {
    Fields {
        owner: &'s str,
        fields: &'s [Field],
        args: Vec<Site<'a>>,
        label: Option<(&'a str, Span)>,
    },
    Value(&'s Shape),
}

#[derive(Clone, Copy)]
struct Site<'a>(Item<'a>, NodeId);

impl<'a> Site<'a> {
    fn node(self) -> &'a hirsel_core_syntax::Node {
        self.0.tree.node(self.1)
    }

    fn children(self) -> impl Iterator<Item = Self> + 'a {
        self.0
            .tree
            .children(self.1)
            .iter()
            .map(move |&id| Self(self.0, id))
    }

    fn args(self) -> impl Iterator<Item = Self> + 'a {
        self.children().skip(1)
    }

    fn span(self) -> Span {
        let span = self.node().origin.span;
        Span {
            start: self.0.file_offset + span.start,
            end: self.0.file_offset + span.end,
        }
    }

    fn contains(self, offset: u32) -> bool {
        let span = self.span();
        span.start <= offset && offset < span.end
    }

    fn name(self) -> Option<&'a str> {
        let token = self.node().token?;
        Some(self.0.tree.token_text(self.0.text, token))
    }

    fn label(self) -> Option<(&'a str, Span)> {
        if self.node().kind != NodeKind::LabelledArgument {
            return None;
        }
        let token = self.node().token?;
        Some((self.name()?, self.0.tree.tokens[token.0 as usize].span))
    }

    fn record<'s>(
        self,
        owner: &'s str,
        args: impl Iterator<Item = Self>,
        fields: &'s [Field],
        schema: &'s Descriptor,
        offset: u32,
    ) -> Option<Context<'a, 's>> {
        let args: Vec<_> = args.collect();
        let arg = args.iter().find(|arg| arg.contains(offset))?;
        let label = arg.label();
        if let Some((name, span)) = label
            && span.end <= offset
        {
            let field = fields.iter().find(|field| field.name == name)?;
            return arg
                .children()
                .next()?
                .expected(&field.shape, schema, offset);
        }
        Some(Context::Fields {
            owner,
            fields,
            args,
            label,
        })
    }

    fn expected<'s>(
        self,
        shape: &'s Shape,
        schema: &'s Descriptor,
        offset: u32,
    ) -> Option<Context<'a, 's>> {
        if !self.contains(offset) {
            return None;
        }
        let kind = self.node().kind;
        let child = || self.children().find(|child| child.contains(offset));
        if kind == NodeKind::Group {
            return child()?.expected(shape, schema, offset);
        }
        if kind == NodeKind::Path
            || kind == NodeKind::Call && self.children().next()?.contains(offset)
        {
            return Some(Context::Value(shape));
        }
        match shape {
            Shape::Named(index) if kind == NodeKind::Call => {
                let name = self.children().next()?.name()?;
                let nominal = &schema.types()[*index];
                match &nominal.body {
                    Body::Record(fields) if name == nominal.name => {
                        self.record(&nominal.name, self.args(), fields, schema, offset)
                    }
                    Body::Sum(cases) => {
                        let case = cases.iter().find(|case| case.name == name)?;
                        match &case.fields {
                            Fields::Labelled(fields) => {
                                self.record(&case.name, self.args(), fields, schema, offset)
                            }
                            Fields::Positional(types) => {
                                let (arg, ty) = self
                                    .args()
                                    .zip(types)
                                    .find(|(arg, _)| arg.contains(offset))?;
                                arg.expected(ty, schema, offset)
                            }
                        }
                    }
                    _ => None,
                }
            }
            Shape::Option(inner)
                if kind == NodeKind::Call && self.children().next()?.name()? == "Some" =>
            {
                self.args().next()?.expected(inner, schema, offset)
            }
            Shape::List(inner) if kind == NodeKind::Array => {
                child()?.expected(inner, schema, offset)
            }
            Shape::Map(inner) if kind == NodeKind::Array => {
                let pair = child()?;
                if pair.node().kind != NodeKind::Tuple {
                    return None;
                }
                pair.children().nth(1)?.expected(inner, schema, offset)
            }
            Shape::Tuple(elements) if kind == NodeKind::Tuple => {
                let (child, ty) = self
                    .children()
                    .zip(elements)
                    .find(|(child, _)| child.contains(offset))?;
                child.expected(ty, schema, offset)
            }
            _ => None,
        }
    }
}
