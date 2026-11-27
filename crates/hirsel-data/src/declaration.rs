use super::*;
use hirsel_core_syntax::{ParseMode, Symbols, parse};
use std::collections::{BTreeMap, BTreeSet};

const SCHEMA_NODES: usize = 65536;
const SCHEMA_DEPTH: usize = 64;

impl Descriptor {
    pub fn lower(module: &str, source: &str, root: &str) -> Result<Self, DataError> {
        if source.len() > 1 << 20 {
            return Err(error(ErrorKind::Limit("schema bytes"), Span::default()));
        }
        let parsed = parse(source, ParseMode::Source, &mut Symbols::default());
        if let Some(refusal) = parsed.resource_refusal {
            return Err(error(ErrorKind::Limit("schema parser"), refusal.site()));
        }
        if let Some(d) = parsed.diagnostics.first() {
            return Err(error(ErrorKind::Invalid("schema syntax"), d.primary));
        }
        Self::lower_items(module, parsed.syntax.items(), root)
    }
    /// The caller supplies the declarations belonging to this resolved module.
    pub fn lower_items<'a>(
        module: &str,
        items: impl Iterator<Item = Item<'a>>,
        root: &str,
    ) -> Result<Self, DataError> {
        let mut declarations = BTreeMap::new();
        for item in items {
            let decl = Site(item, item.root);
            if matches!(
                decl.kind(),
                NodeKind::TypeDecl | NodeKind::ResourceType | NodeKind::ScopedType
            ) {
                if declarations.insert(decl.token().unwrap(), decl).is_some() {
                    return Err(decl.error("duplicate type"));
                }
            }
        }
        let start = *declarations
            .get(root)
            .ok_or_else(|| error(ErrorKind::Invalid("unknown schema root"), Span::default()))?;
        Self::lower_resolved(module, start.0, |item, id| {
            let head = Site(item, id);
            let words: Vec<_> = head.words().collect();
            let (name, local_only) = match words.as_slice() {
                [n] => (*n, false),
                [m, ".", n] if *m == module => (*n, true),
                _ => return None,
            };
            match declarations.get(name) {
                Some(decl) => Some(ResolvedType::Nominal {
                    module: module.into(),
                    item: decl.0,
                }),
                None if !local_only => Some(ResolvedType::Core(name)),
                None => None,
            }
        })
    }
    /// Lower a root using the caller's binding for each written type path.
    pub fn lower_resolved<'a>(
        module: &str,
        root: Item<'a>,
        resolve: impl Fn(Item<'a>, NodeId) -> Option<ResolvedType<'a>>,
    ) -> Result<Self, DataError> {
        let start = Site(root, root.root);
        let mut cx = Lower {
            resolve: &resolve,
            instances: BTreeMap::new(),
            types: Vec::new(),
            work: SCHEMA_NODES,
        };
        cx.nominal(start, module, vec![], 0)?;
        if !matches!(cx.types[0].body, Body::Record(_)) {
            return Err(start.error("root must be a concrete record"));
        }
        let mut descriptor = Self {
            module: module.into(),
            types: cx.types,
            digest: String::new(),
        };
        descriptor.digest = digest_hex(&descriptor.bytes());
        Ok(descriptor)
    }
}
pub enum ResolvedType<'a> {
    Core(&'a str),
    Nominal { module: String, item: Item<'a> },
}
struct Lower<'a, 'm> {
    resolve: &'m dyn Fn(Item<'a>, NodeId) -> Option<ResolvedType<'a>>,
    instances: BTreeMap<(String, String, Vec<Shape>), usize>,
    types: Vec<Nominal>,
    work: usize,
}
type Params<'a> = BTreeMap<&'a str, Shape>;
impl<'a> Lower<'a, '_> {
    fn charge(&mut self, at: Site, depth: usize) -> Result<(), DataError> {
        if depth >= SCHEMA_DEPTH || self.work == 0 {
            return Err(error(ErrorKind::Limit("schema complexity"), at.span()));
        }
        self.work -= 1;
        Ok(())
    }
    fn nominal(
        &mut self,
        decl: Site<'a>,
        module: &str,
        args: Vec<Shape>,
        depth: usize,
    ) -> Result<Shape, DataError> {
        self.charge(decl, depth)?;
        let name = decl
            .token()
            .ok_or_else(|| decl.error("expected data type declaration"))?;
        let key = (module.to_owned(), name.to_owned(), args);
        if let Some(&idx) = self.instances.get(&key) {
            return Ok(Shape::Named(idx));
        }
        let args = &key.2;
        if decl.kind() != NodeKind::TypeDecl
            || decl.kids().any(|x| {
                !matches!(
                    x.kind(),
                    NodeKind::Public
                        | NodeKind::Generics
                        | NodeKind::Bounds
                        | NodeKind::Fields
                        | NodeKind::Variants
                )
            })
        {
            return Err(decl.error("type has no structural data representation"));
        }
        let formal: Vec<_> = decl
            .kids()
            .filter(|n| n.kind() == NodeKind::Generics)
            .flat_map(|n| n.kids())
            .collect();
        if formal.len() != args.len() || formal.iter().any(|f| f.kind() != NodeKind::Generic) {
            return Err(decl.error("type arguments"));
        }
        let codec = decl
            .kids()
            .filter(|k| k.kind() == NodeKind::Bounds)
            .flat_map(|k| k.kids())
            .any(|k| k.token() == Some("codec"));
        let args_size = args.iter().map(Shape::nodes).sum::<usize>();
        self.work = self
            .work
            .checked_sub(args_size)
            .ok_or_else(|| error(ErrorKind::Limit("schema complexity"), decl.span()))?;
        let params = formal
            .iter()
            .map(|f| f.token().unwrap())
            .zip(args.iter().cloned())
            .collect();
        let index = self.types.len();
        self.instances.insert(key, index);
        self.types.push(Nominal {
            module: module.into(),
            name: name.to_owned(),
            body: Body::Record(vec![]),
        });
        let mut seen = BTreeSet::new();
        let body = decl
            .kids()
            .find(|x| matches!(x.kind(), NodeKind::Fields | NodeKind::Variants))
            .ok_or_else(|| decl.error("expected data record or sum"))?;
        self.types[index].body = if body.kind() == NodeKind::Fields {
            Body::Record(self.fields(body, &params, depth + 1, codec)?)
        } else {
            let mut cases = Vec::new();
            for case in body.kids() {
                if case.kind() != NodeKind::Variant || !seen.insert(case.token()) {
                    return Err(case.error("unsupported or duplicate data case"));
                }
                let fields = if case.kids().next().is_some_and(|f| f.token().is_some()) {
                    Fields::Labelled(self.fields(case, &params, depth + 1, false)?)
                } else {
                    Fields::Positional(
                        case.kids()
                            .map(|f| self.shape(f.kids().next().unwrap(), &params, depth + 1))
                            .collect::<Result<_, _>>()?,
                    )
                };
                cases.push(Case {
                    name: case.token().unwrap().into(),
                    fields,
                });
            }
            Body::Sum(cases)
        };
        Ok(Shape::Named(index))
    }
    fn fields(
        &mut self,
        node: Site<'a>,
        params: &Params<'a>,
        depth: usize,
        codec: bool,
    ) -> Result<Vec<Field>, DataError> {
        let mut fields = Vec::new();
        let mut seen = BTreeSet::new();
        for field in node.kids() {
            if !matches!(field.kind(), NodeKind::Field | NodeKind::Payload)
                || !seen.insert(field.token())
            {
                return Err(field.error("unsupported or duplicate data field"));
            }
            let mut kids = field.kids();
            let shape = self.shape(kids.next().unwrap(), params, depth)?;
            let default = kids
                .next()
                .map(|n| {
                    if !codec {
                        return Err(n.error("field default requires codec"));
                    }
                    if let Shape::Scalar(s) = shape {
                        s.literal(n)
                            .map(|(v, _)| v)
                            .ok_or_else(|| n.error("invalid scalar default"))
                    } else {
                        Err(n.error("non-scalar default"))
                    }
                })
                .transpose()?;
            fields.push(Field {
                name: field.token().unwrap().into(),
                shape,
                default,
            });
        }
        Ok(fields)
    }
    fn shape(
        &mut self,
        at: Site<'a>,
        params: &Params<'a>,
        depth: usize,
    ) -> Result<Shape, DataError> {
        self.charge(at, depth)?;
        let mut kids = at.kids();
        let head = match at.kind() {
            NodeKind::UnitType => return Ok(Shape::Unit),
            NodeKind::GroupType => return self.shape(kids.next().unwrap(), params, depth + 1),
            NodeKind::TupleType => {
                return Ok(Shape::Tuple(
                    kids.map(|n| self.shape(n, params, depth + 1))
                        .collect::<Result<_, _>>()?,
                ));
            }
            NodeKind::TypeArguments => kids.next().unwrap(),
            NodeKind::TypePath => at,
            _ => return Err(at.error("unsupported data type")),
        };
        let args = kids
            .map(|n| self.shape(n, params, depth + 1))
            .collect::<Result<Vec<_>, _>>()?;
        if args.is_empty()
            && head.words().count() == 1
            && let Some(param) = head.token().and_then(|n| params.get(n))
        {
            self.work = self
                .work
                .checked_sub(param.nodes())
                .ok_or_else(|| error(ErrorKind::Limit("schema complexity"), at.span()))?;
            return Ok(param.clone());
        }
        let name = match (self.resolve)(head.0, head.1)
            .ok_or_else(|| head.error("unresolved data type"))?
        {
            ResolvedType::Nominal { module, item } => {
                return self.nominal(Site(item, item.root), &module, args, depth + 1);
            }
            ResolvedType::Core(name) => name,
        };
        if args.is_empty()
            && let Some(s) = Scalar::parse(name)
        {
            return Ok(Shape::Scalar(s));
        }
        let mut args = args.into_iter();
        Ok(match (name, args.len()) {
            ("Option", 1) => Shape::Option(Box::new(args.next().unwrap())),
            ("List", 1) => Shape::List(Box::new(args.next().unwrap())),
            ("Map", 2) if args.next() == Some(Shape::Scalar(Scalar::String)) => {
                Shape::Map(Box::new(args.next().unwrap()))
            }
            ("Map", _) => return Err(at.error("map requires String keys and one value type")),
            _ => return Err(at.error("unsupported data type")),
        })
    }
}

impl Descriptor {
    /// Reconstitute a descriptor emitted by the build-time lowerer. The bytes
    /// and digest are pinned by the consumer together in its binary.
    pub fn embedded(bytes: &'static [u8], digest: &'static str) -> Result<Self, DataError> {
        let invalid = || {
            error(
                ErrorKind::Invalid("invalid embedded descriptor"),
                Span::default(),
            )
        };
        if digest_hex(bytes) != digest {
            return Err(invalid());
        }
        let (version, module, types): (u8, String, Vec<Nominal>) =
            serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if version != 1 || !matches!(types.first().map(|n| &n.body), Some(Body::Record(_))) {
            return Err(invalid());
        }
        Ok(Self {
            module,
            types,
            digest: digest.into(),
        })
    }
}

fn digest_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Shape {
    fn nodes(&self) -> usize {
        1 + match self {
            Self::List(s) | Self::Map(s) | Self::Option(s) => s.nodes(),
            Self::Tuple(ts) => ts.iter().map(Self::nodes).sum(),
            _ => 0,
        }
    }
}
