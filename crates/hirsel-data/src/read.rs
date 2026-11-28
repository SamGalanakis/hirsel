use super::*;
use crate::decoded::{host_memory, storage};
use hirsel_core_syntax::{DataParseError, parse_data, parse_data_file};

#[derive(Debug, Default)]
pub(super) struct Index {
    pub span: Option<Span>,
    pub children: Vec<(PathPart, Index)>,
    pub map: bool,
    pub record: bool,
    pub tagged: bool,
}
type Checked<'s> = (Decoded<'s>, Index);
type Result<T> = std::result::Result<T, DataError>;

pub fn decode<'s>(bytes: &[u8], roots: &[&'s Descriptor], limits: Limits) -> Result<Document<'s>> {
    decode_with_root(bytes, roots, limits, &mut |_| ())
}

/// Report the selected role even when one of the document's fields is invalid.
/// Discovery uses this to retain a malformed workspace as the enclosing root.
pub fn decode_with_root<'s>(
    bytes: &[u8],
    roots: &[&'s Descriptor],
    limits: Limits,
    selected: &mut dyn FnMut(&Descriptor),
) -> Result<Document<'s>> {
    if bytes.len() > limits.input_bytes {
        return Err(error(ErrorKind::Limit("input bytes"), Span::default()));
    }
    let syntax = parse_data_file(bytes).map_err(|e| diagnostic(parse_error(e), limits))?;
    let (schema, value, index) =
        semantic::<true>(syntax.item(0), roots, limits, &mut |_| Ok(()), selected)
            .map_err(|e| diagnostic(e, limits))?;
    Ok(Document {
        schema,
        syntax,
        value: value.projection(),
        index,
    })
}

fn parse_error(e: DataParseError) -> DataError {
    match e {
        DataParseError::HostMemory => host_memory(),
        DataParseError::Syntax(at) => error(ErrorKind::Invalid("data syntax"), at),
        DataParseError::Resource(r) => error(ErrorKind::Limit("parser nesting"), r.site()),
    }
}

fn diagnostic(e: DataError, limits: Limits) -> DataError {
    if limits.diagnostics == 0 && matches!(e.kind, ErrorKind::Invalid(_) | ErrorKind::Limit(_)) {
        error(ErrorKind::Limit("diagnostics"), e.span)
    } else {
        e
    }
}

pub(crate) fn guest_with_budget<'s>(
    bytes: &[u8],
    schema: &'s Descriptor,
    limits: Limits,
    pay: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<Decoded<'s>> {
    if bytes.len() > limits.input_bytes {
        return Err(error(ErrorKind::Limit("input bytes"), Span::default()));
    }
    let mut read = || {
        pay(128 * (bytes.len() as u64).saturating_add(1))?;
        let parsed = parse_data(bytes).map_err(parse_error)?;
        Ok(semantic::<false>(parsed.item(), &[schema], limits, pay, &mut |_| ())?.1)
    };
    read().map_err(|e| diagnostic(e, limits))
}

fn semantic<'s, const EDIT: bool>(
    item: Item<'_>,
    roots: &[&'s Descriptor],
    limits: Limits,
    pay: &mut dyn FnMut(u64) -> Result<()>,
    selected: &mut dyn FnMut(&Descriptor),
) -> Result<(&'s Descriptor, Decoded<'s>, Index)> {
    let root = Site(item, item.root);
    if item.tree.nodes.len() > limits.nodes {
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
    selected(schema);
    let mut reader = Reader::<EDIT> {
        schema,
        limits,
        pay,
    };
    let Body::Record(fields) = &schema.types[0].body else {
        unreachable!()
    };
    let (value, index) = reader.record(root, kids, fields, 1)?;
    Ok((schema, value, index))
}
struct Reader<'s, 'a, const EDIT: bool> {
    pay: &'a mut dyn FnMut(u64) -> Result<()>,
    schema: &'s Descriptor,
    limits: Limits,
}
fn spend(budget: &mut usize, n: usize, label: &'static str, at: Site) -> Result<()> {
    *budget = budget
        .checked_sub(n)
        .ok_or_else(|| error(ErrorKind::Limit(label), at.span()))?;
    Ok(())
}
impl<'s, const EDIT: bool> Reader<'s, '_, EDIT> {
    fn length(&self, n: usize, at: Site) -> Result<()> {
        if n > self.limits.collection {
            return Err(error(ErrorKind::Limit("collection length"), at.span()));
        }
        Ok(())
    }
    fn check(&mut self, at: Site, shape: &Shape, depth: usize) -> Result<Checked<'s>> {
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
            Shape::Unit if at.kind() == NodeKind::Unit => Ok((Decoded::Scalar(Value::Null), leaf)),
            Shape::Scalar(s) => {
                let (value, size) = s
                    .literal(at)?
                    .ok_or_else(|| at.error("expected scalar literal"))?;
                spend(&mut self.limits.decoded_bytes, size, "decoded bytes", at)?;
                Ok((value, leaf))
            }
            Shape::Tuple(types) if at.kind() == NodeKind::Tuple => {
                self.sequence(at, kids, types, depth)
            }
            Shape::List(shape) if at.kind() == NodeKind::Array => {
                self.length(kids.len(), at)?;
                let mut list = storage(kids.len())?;
                let mut index = leaf;
                for (i, kid) in kids.enumerate() {
                    let (v, idx) = self.check(kid, shape, depth + 1)?;
                    list.push(v);
                    if EDIT {
                        index.children.push((PathPart::Index(i), idx));
                    }
                }
                Ok((Decoded::Members(Members::Positional(list)), index))
            }
            Shape::Map(shape) if at.kind() == NodeKind::Array => self.map(at, shape, depth),
            Shape::Option(shape) => {
                let (name, arguments) = constructor(at)?;
                let (values, mut index) = match (name, arguments.len(), at.kind()) {
                    ("None", 0, NodeKind::Path) => {
                        (Decoded::Members(Members::Positional(Vec::new())), leaf)
                    }
                    ("Some", 1, NodeKind::Call) => {
                        self.sequence(at, arguments, std::slice::from_ref(shape.as_ref()), depth)?
                    }
                    _ => return Err(at.error("expected an option constructor")),
                };
                spend(&mut self.limits.decoded_bytes, 4, "decoded bytes", at)?;
                index.tagged = true;
                Ok((
                    tag(if name == "None" { "None" } else { "Some" }, values),
                    index,
                ))
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
                        Ok((tag(&case.name, fields), index))
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
    ) -> Result<Checked<'s>> {
        self.length(kids.len(), at)?;
        if kids.len() != types.len() {
            return Err(at.error("wrong positional arity"));
        }
        let mut out = storage(kids.len())?;
        let mut index = Index {
            span: Some(at.span()),
            ..Default::default()
        };
        for (kid, ty) in kids.zip(types) {
            let (value, idx) = self.check(kid, ty, depth + 1)?;
            if EDIT {
                index.children.push((PathPart::Index(out.len()), idx));
            }
            out.push(value);
        }
        Ok((Decoded::Members(Members::Positional(out)), index))
    }
    fn record<'a>(
        &mut self,
        at: Site,
        kids: impl ExactSizeIterator<Item = Site<'a>>,
        fields: &'s [Field],
        depth: usize,
    ) -> Result<Checked<'s>> {
        if depth > self.limits.depth {
            return Err(error(ErrorKind::Limit("depth"), at.span()));
        }
        self.length(kids.len().max(fields.len()), at)?;
        (self.pay)(fields.len() as u64 * 128)?;
        let mut slots = storage(fields.len())?;
        slots.resize_with(fields.len(), || None);
        let mut index = Index {
            span: Some(at.span()),
            record: true,
            ..Default::default()
        };
        let mut unknown = Vec::new();
        for arg in kids {
            if arg.kind() != NodeKind::LabelledArgument {
                return Err(arg.error("expected a labelled field"));
            }
            let label = arg.token().unwrap();
            let mut found = None;
            for (i, field) in fields.iter().enumerate() {
                spend(
                    &mut self.limits.comparison_bytes,
                    1 + label.len().min(field.name.len()),
                    "key comparisons",
                    arg,
                )?;
                if field.name == label {
                    found = Some((i, field));
                    break;
                }
            }
            let Some((i, field)) = found else {
                if !EDIT || self.limits.diagnostics <= 1 {
                    return Err(arg.error("unknown field"));
                }
                if unknown.len() == self.limits.diagnostics {
                    return Err(error(ErrorKind::Limit("diagnostics"), arg.span()));
                }
                unknown.try_reserve(1).map_err(|_| host_memory())?;
                unknown.push((label, arg.span()));
                continue;
            };
            if slots[i].is_some() {
                return Err(arg.error("duplicate field"));
            }
            let (v, idx) = self
                .check(arg.kids().next().unwrap(), &field.shape, depth + 1)
                .map_err(|mut e| {
                    if matches!(e.kind, ErrorKind::HostMemory | ErrorKind::Capacity { .. }) {
                        return e;
                    }
                    if e.path.try_reserve(1).is_err() {
                        return host_memory();
                    }
                    let Ok(name) = copy_name(&field.name) else {
                        return host_memory();
                    };
                    e.path.insert(0, name);
                    e
                })?;
            slots[i] = Some(v);
            if EDIT {
                index
                    .children
                    .push((PathPart::Field(field.name.clone()), idx));
            }
        }
        if !unknown.is_empty() {
            let mut cost = 0usize;
            unknown.sort_unstable_by(|(a, _), (b, _)| {
                cost = cost.saturating_add(1 + a.len().min(b.len()));
                a.cmp(b)
            });
            spend(
                &mut self.limits.comparison_bytes,
                cost,
                "key comparisons",
                at,
            )?;
            let mut spans = unknown.into_iter().map(|(_, span)| span);
            let mut failure = error(ErrorKind::Invalid("unknown field"), spans.next().unwrap());
            failure.additional = storage(spans.len())?;
            failure.additional.extend(spans);
            return Err(failure);
        }
        let mut values = storage(fields.len())?;
        for (field, slot) in fields.iter().zip(slots) {
            if let Some(value) = slot {
                values.push((field.name.as_str(), value));
                continue;
            }
            let value = match (&field.shape, &field.default) {
                (Shape::Option(_), _) => {
                    spend(&mut self.limits.decoded_bytes, 4, "decoded bytes", at)?;
                    Decoded::Case("None", Members::Positional(Vec::new()))
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
                    Decoded::Default(default)
                }
                _ => {
                    let mut e = at.error("missing required field");
                    e.path = storage(1)?;
                    e.path.push(copy_name(&field.name)?);
                    return Err(e);
                }
            };
            values.push((field.name.as_str(), value));
        }
        Ok((Decoded::Members(Members::Labelled(values)), index))
    }
    fn map(&mut self, at: Site, shape: &Shape, depth: usize) -> Result<Checked<'s>> {
        self.length(at.kids().len(), at)?;
        let mut trie = storage(1)?;
        trie.push(Trie::default());
        let mut pairs = storage(at.kids().len())?;
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
                .literal(key)?
                .ok_or_else(|| key.error("expected string key"))?;
            spend(&mut self.limits.decoded_bytes, len, "decoded bytes", key)?;
            spend(
                &mut self.limits.comparison_bytes,
                len + 1,
                "key comparisons",
                key,
            )?;
            let Decoded::Scalar(Value::String(text)) = k else {
                unreachable!()
            };
            let mut pos = 0;
            for byte in text.bytes() {
                let edges = &mut trie[pos].edges;
                let next = match edges.binary_search_by_key(&byte, |&(b, _)| b) {
                    Ok(i) => edges[i].1,
                    Err(i) => {
                        edges.try_reserve(1).map_err(|_| host_memory())?;
                        let next = trie.len();
                        trie[pos].edges.insert(i, (byte, next));
                        trie.try_reserve(1).map_err(|_| host_memory())?;
                        trie.push(Trie::default());
                        next
                    }
                };
                pos = next;
            }
            if let Some(first) = trie[pos].first.replace(key.span()) {
                let mut e = key.error("duplicate map key");
                e.related = Some(first);
                return Err(e);
            }
            let (v, idx) = self.check(kids.next().unwrap(), shape, depth + 1)?;
            if EDIT {
                index.children.push((PathPart::Key(text.clone()), idx));
            }
            pairs.push((text, v));
        }
        Ok((Decoded::Map(pairs), index))
    }
}
#[derive(Default)]
struct Trie {
    edges: Vec<(u8, usize)>,
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

fn tag<'s>(name: &'s str, fields: Decoded<'s>) -> Decoded<'s> {
    let Decoded::Members(fields) = fields else {
        unreachable!()
    };
    Decoded::Case(name, fields)
}
fn copy_name(name: &str) -> Result<String> {
    let mut out = String::new();
    out.try_reserve_exact(name.len())
        .map_err(|_| host_memory())?;
    out.push_str(name);
    Ok(out)
}
