use super::*;
use hirsel_core_syntax::{ParseMode, Symbols, parse_bytes};
use serde_json::{Map, json};
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub(super) struct Index {
    pub span: Option<Span>,
    pub children: Vec<(PathPart, Index)>,
    pub map: bool,
    pub record: bool,
    pub tagged: bool,
}
type Checked = (Value, Index);
type Result<T> = std::result::Result<T, DataError>;

pub fn decode<'s>(bytes: &[u8], roots: &[&'s Descriptor], limits: Limits) -> Result<Document<'s>> {
    if bytes.len() > limits.input_bytes {
        return Err(error(ErrorKind::Limit("input bytes"), Span::default()));
    }
    let checked = decode_inner(bytes, roots, limits);
    checked.map_err(|e| {
        if limits.diagnostics == 0 {
            error(ErrorKind::Limit("diagnostics"), e.span)
        } else {
            e
        }
    })
}
fn decode_inner<'s>(
    bytes: &[u8],
    roots: &[&'s Descriptor],
    limits: Limits,
) -> Result<Document<'s>> {
    let parsed = parse_bytes(bytes, ParseMode::Data, &mut Symbols::default());
    if let Some(r) = parsed.resource_refusal {
        return Err(error(ErrorKind::Limit("parser nesting"), r.site()));
    }
    if let Some(d) = parsed.diagnostics.first() {
        return Err(error(ErrorKind::Invalid("data syntax"), d.primary));
    }
    let syntax = parsed.syntax;
    let item = syntax.item(0);
    let root = Site(item, item.root);
    if syntax.tree.nodes.len() > limits.nodes {
        return Err(error(ErrorKind::Limit("nodes"), root.span()));
    }
    let mut kids = root.kids();
    let head = kids.next().unwrap();
    let mut words = head.words();
    let module = words.next().unwrap();
    let name = words.nth(1).unwrap();
    drop(words);
    let mut accepted = roots
        .iter()
        .copied()
        .filter(|s| s.module() == module && s.root() == name);
    let schema = accepted
        .next()
        .ok_or_else(|| head.error("wrong nominal root"))?;
    if accepted.any(|s| s.digest() != schema.digest()) {
        return Err(head.error("ambiguous schema digest"));
    }
    let mut reader = Reader { schema, limits };
    let Body::Record(fields) = &schema.types[0].body else {
        unreachable!()
    };
    let (value, index) = reader.record(root, kids, fields, 1)?;
    Ok(Document {
        schema,
        syntax,
        value,
        index,
    })
}
struct Reader<'s> {
    schema: &'s Descriptor,
    limits: Limits,
}
fn spend(budget: &mut usize, n: usize, label: &'static str, at: Site) -> Result<()> {
    *budget = budget
        .checked_sub(n)
        .ok_or_else(|| error(ErrorKind::Limit(label), at.span()))?;
    Ok(())
}
impl Reader<'_> {
    fn length(&self, n: usize, at: Site) -> Result<()> {
        if n > self.limits.collection {
            return Err(error(ErrorKind::Limit("collection length"), at.span()));
        }
        Ok(())
    }
    fn check(&mut self, at: Site, shape: &Shape, depth: usize) -> Result<Checked> {
        if depth > self.limits.depth {
            return Err(error(ErrorKind::Limit("depth"), at.span()));
        }
        let mut kids = at.kids();
        if at.kind() == NodeKind::Group {
            let (value, mut index) = self.check(kids.next().unwrap(), shape, depth + 1)?;
            index.span = Some(at.span());
            return Ok((value, index));
        }
        let leaf = Index {
            span: Some(at.span()),
            ..Default::default()
        };
        match shape {
            Shape::Unit if at.kind() == NodeKind::Unit => Ok((Value::Null, leaf)),
            Shape::Scalar(s) => {
                let (value, size) = s
                    .literal(at)
                    .ok_or_else(|| at.error("expected scalar literal"))?;
                spend(&mut self.limits.decoded_bytes, size, "decoded bytes", at)?;
                Ok((value, leaf))
            }
            Shape::Tuple(types) if at.kind() == NodeKind::Tuple => {
                self.sequence(at, kids, types, depth)
            }
            Shape::List(shape) if at.kind() == NodeKind::Array => {
                self.length(kids.len(), at)?;
                let mut list = vec![];
                let mut index = leaf;
                for (i, kid) in kids.enumerate() {
                    let (v, idx) = self.check(kid, shape, depth + 1)?;
                    list.push(v);
                    index.children.push((PathPart::Index(i), idx));
                }
                Ok((Value::Array(list), index))
            }
            Shape::Map(shape) if at.kind() == NodeKind::Array => self.map(at, shape, depth),
            Shape::Option(shape) => {
                let (name, arguments) = constructor(at)?;
                let (values, mut index) = match (name, arguments.len(), at.kind()) {
                    ("None", 0, NodeKind::Path) => (json!([]), leaf),
                    ("Some", 1, NodeKind::Call) => {
                        self.sequence(at, arguments, std::slice::from_ref(shape.as_ref()), depth)?
                    }
                    _ => return Err(at.error("expected an option constructor")),
                };
                spend(&mut self.limits.decoded_bytes, 4, "decoded bytes", at)?;
                index.tagged = true;
                Ok((json!({"case":name,"fields":values}), index))
            }
            Shape::Named(i) => {
                let (name, arguments) = constructor(at)?;
                let nominal = &self.schema.types[*i];
                match &nominal.body {
                    Body::Record(fields) if name == nominal.name && at.kind() == NodeKind::Call => {
                        self.record(at, arguments, fields, depth)
                    }
                    Body::Sum(cases) => {
                        let case = cases
                            .iter()
                            .find(|c| c.name == name)
                            .ok_or_else(|| at.error("unexpected data case"))?;
                        let (fields, mut index) = match &case.fields {
                            Fields::Positional(types)
                                if types.is_empty() && at.kind() != NodeKind::Path =>
                            {
                                return Err(at.error("expected bare case"));
                            }
                            Fields::Positional(types) => {
                                self.sequence(at, arguments, types, depth)?
                            }
                            Fields::Labelled(_) if at.kind() != NodeKind::Call => {
                                return Err(at.error("expected labelled case arguments"));
                            }
                            Fields::Labelled(fields) => {
                                self.record(at, arguments, fields, depth)?
                            }
                        };
                        spend(&mut self.limits.decoded_bytes, 4, "decoded bytes", at)?;
                        index.tagged = true;
                        Ok((json!({"case":name,"fields":fields}), index))
                    }
                    _ => Err(at.error("wrong record constructor")),
                }
            }
            _ => Err(at.error("expected literal data")),
        }
    }
    fn sequence<'a>(
        &mut self,
        at: Site,
        kids: impl ExactSizeIterator<Item = Site<'a>>,
        types: &[Shape],
        depth: usize,
    ) -> Result<Checked> {
        self.length(kids.len(), at)?;
        if kids.len() != types.len() {
            return Err(at.error("wrong positional arity"));
        }
        let mut out = vec![];
        let mut index = Index {
            span: Some(at.span()),
            ..Default::default()
        };
        for (kid, ty) in kids.zip(types) {
            let (value, idx) = self.check(kid, ty, depth + 1)?;
            index.children.push((PathPart::Index(out.len()), idx));
            out.push(value);
        }
        Ok((Value::Array(out), index))
    }
    fn record<'a>(
        &mut self,
        at: Site,
        kids: impl ExactSizeIterator<Item = Site<'a>>,
        fields: &[Field],
        depth: usize,
    ) -> Result<Checked> {
        if depth > self.limits.depth {
            return Err(error(ErrorKind::Limit("depth"), at.span()));
        }
        self.length(kids.len().max(fields.len()), at)?;
        let mut values = Map::new();
        let mut index = Index {
            span: Some(at.span()),
            record: true,
            ..Default::default()
        };
        for arg in kids {
            if arg.kind() != NodeKind::LabelledArgument {
                return Err(arg.error("expected a labelled field"));
            }
            let label = arg.token().unwrap();
            let mut found = None;
            for field in fields {
                spend(
                    &mut self.limits.comparison_bytes,
                    1 + label.len().min(field.name.len()),
                    "key comparisons",
                    arg,
                )?;
                if field.name == label {
                    found = Some(field);
                    break;
                }
            }
            let field = found.ok_or_else(|| arg.error("unknown field"))?;
            if values.contains_key(label) {
                return Err(arg.error("duplicate field"));
            }
            let (v, idx) = self
                .check(arg.kids().next().unwrap(), &field.shape, depth + 1)
                .map_err(|mut e| {
                    e.path.insert(0, field.name.clone());
                    e
                })?;
            values.insert(field.name.clone(), v);
            index
                .children
                .push((PathPart::Field(field.name.clone()), idx));
        }
        for field in fields {
            if values.contains_key(&field.name) {
                continue;
            }
            let value = match (&field.shape, &field.default) {
                (Shape::Option(_), _) => {
                    spend(&mut self.limits.decoded_bytes, 4, "decoded bytes", at)?;
                    json!({"case":"None","fields":[]})
                }
                (Shape::Scalar(s), Some(default)) => {
                    let size = match s {
                        Scalar::String => default.as_str().unwrap().len(),
                        Scalar::Bytes => default.as_array().unwrap().len(),
                        Scalar::Bool | Scalar::U8 | Scalar::I8 => 1,
                        Scalar::U16 | Scalar::I16 => 2,
                        Scalar::U32 | Scalar::I32 | Scalar::Char | Scalar::F32 => 4,
                        _ => 8,
                    };
                    spend(&mut self.limits.decoded_bytes, size, "decoded bytes", at)?;
                    default.clone()
                }
                _ => {
                    let mut e = at.error("missing required field");
                    e.path.push(field.name.clone());
                    return Err(e);
                }
            };
            values.insert(field.name.clone(), value);
        }
        Ok((Value::Object(values), index))
    }
    fn map(&mut self, at: Site, shape: &Shape, depth: usize) -> Result<Checked> {
        self.length(at.kids().len(), at)?;
        let mut trie = vec![Trie::default()];
        let mut pairs = vec![];
        let mut index = Index {
            span: Some(at.span()),
            map: true,
            ..Default::default()
        };
        for pair in at.kids() {
            if pair.kind() != NodeKind::Tuple || pair.kids().len() != 2 {
                return Err(pair.error("expected a string-key pair"));
            }
            let mut kids = pair.kids();
            let key = kids.next().unwrap();
            let (k, len) = Scalar::String
                .literal(key)
                .ok_or_else(|| key.error("expected string key"))?;
            spend(&mut self.limits.decoded_bytes, len, "decoded bytes", key)?;
            spend(
                &mut self.limits.comparison_bytes,
                len + 1,
                "key comparisons",
                key,
            )?;
            let text = k.as_str().unwrap();
            let mut pos = 0;
            for byte in text.bytes() {
                let next = trie.len();
                pos = *trie[pos].edges.entry(byte).or_insert(next);
                if pos == next {
                    trie.push(Trie::default())
                }
            }
            if let Some(first) = trie[pos].first.replace(key.span()) {
                let mut e = key.error("duplicate map key");
                e.related = Some(first);
                return Err(e);
            }
            let (v, idx) = self.check(kids.next().unwrap(), shape, depth + 1)?;
            index.children.push((PathPart::Key(text.into()), idx));
            pairs.push(json!([k, v]));
        }
        Ok((Value::Array(pairs), index))
    }
}
#[derive(Default)]
struct Trie {
    edges: BTreeMap<u8, usize>,
    first: Option<Span>,
}
fn constructor(at: Site<'_>) -> Result<(&str, impl ExactSizeIterator<Item = Site<'_>>)> {
    let mut args = at.kids();
    let head = match at.kind() {
        NodeKind::Call => args.next().unwrap(),
        NodeKind::Path => at,
        _ => return Err(at.error("expected constructor")),
    };
    if head.kind() != NodeKind::Path || head.words().count() != 1 {
        return Err(head.error("expected unqualified constructor"));
    }
    Ok((head.token().unwrap(), args))
}
