//! A module row's closure: the rows a type reaches.
use super::*;

/// A hook of a closure row: its definition and dictionaries, each type argument as a
/// closure position, and its signature with every declared row and `dest` as a
/// position.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ClosureHook {
    pub definition: DefPath,
    pub dictionaries: Vec<String>,
    pub arguments: Vec<u32>,
    pub signature: Signature,
}

/// A row of a [`Closure`]: the shape with every row it names replaced by that row's
/// closure position and its hooks unset, and its finalizer, clone body and codec maker
/// as hooks.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ClosureRow {
    pub shape: Shape,
    pub hooks: [Option<ClosureHook>; 4],
}

/// The rows a root row reaches, independent of any module's numbering: positions are
/// first-visit order from the root (position 0), over fields, then `held`, then hook
/// type arguments, then hook signature rows. Two rows of two modules are one type
/// exactly when their closures are equal; `hash` names it.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Closure {
    pub rows: Vec<ClosureRow>,
    pub hash: ContentHash,
}
impl Closure {
    /// The closure of `rows`, hashed over their encoding (the encoding the store
    /// hashes IR with).
    pub fn new(rows: Vec<ClosureRow>) -> Self {
        let hash = crate::normal::closure_hash(&rows);
        Self { rows, hash }
    }
}

pub(crate) enum ClosureKind {
    Full,
    Abi,
}

impl Module {
    /// The closure of row `root` ([`Closure`]).
    pub fn closure(&self, root: TypeId) -> Closure {
        self.closure_impl(root, ClosureKind::Full, None)
            .expect("unmetered closure")
    }

    pub(crate) fn closure_impl(
        &self,
        root: TypeId,
        kind: ClosureKind,
        budget: Option<&Debit<'_>>,
    ) -> DecodeResult<Closure> {
        use std::collections::hash_map::Entry;
        let mut order = HashMap::<u32, u32>::new();
        let mut held = 0; // What `budget` holds for the table of `order`.
        let mut rows: Vec<Option<ClosureRow>> = Vec::new();
        let mut pending = Vec::new();
        let visit = |r: u32,
                     order: &mut HashMap<u32, u32>,
                     rows: &mut Vec<Option<ClosureRow>>,
                     pending: &mut Vec<u32>,
                     held: &mut usize| {
            if let Some(&number) = order.get(&r) {
                return Ok(number);
            }
            if let Some(budget) = budget {
                budget.reserve(rows, 1)?;
                budget.reserve(pending, 1)?;
                if order.len() == order.capacity() {
                    // This accounts for the growing hash table, including
                    // bucket control bytes and unused load-factor capacity.
                    let bytes = order.capacity().max(4).saturating_mul(32);
                    budget.regrow(*held, bytes, || order.try_reserve(1).is_ok())?;
                    *held = bytes;
                }
            }
            let next = order.len() as u32;
            let Entry::Vacant(entry) = order.entry(r) else {
                unreachable!()
            };
            entry.insert(next);
            rows.push(None);
            pending.push(r);
            Ok(next)
        };
        visit(root.0, &mut order, &mut rows, &mut pending, &mut held)?;
        while let Some(t) = pending.pop() {
            let original = &self.types[t as usize];
            if let Some(budget) = budget {
                crate::normal::charge_shape(original, budget)?;
            }
            let mut shape = Shape::clone(original);
            let mut error = None;
            shape.map_refs(
                &mut |r| match visit(r.0, &mut order, &mut rows, &mut pending, &mut held) {
                    Ok(number) => TypeRef(number),
                    Err(refused) => {
                        error = Some(refused);
                        r
                    }
                },
            );
            if let Some(error) = error {
                return Err(error);
            }
            let mut hook = |f: Option<FuncRef>| -> DecodeResult<Option<ClosureHook>> {
                let Some(f) = f else { return Ok(None) };
                let f = &self.functions[f.0 as usize];
                let key = &self.instances[f.instance.index()];
                let original = &self.signatures[f.signature.index()];
                if let Some(budget) = budget {
                    crate::normal::charge_identity(&key.definition, &key.dictionaries, budget)?;
                    budget.allocation::<u32>(key.arguments.len())?;
                    budget.allocation::<Param>(original.params.len())?;
                    budget.allocation::<Ty>(original.results.len())?;
                }
                let mut arguments = Vec::with_capacity(key.arguments.len());
                for argument in &key.arguments {
                    arguments.push(visit(
                        argument.0,
                        &mut order,
                        &mut rows,
                        &mut pending,
                        &mut held,
                    )?);
                }
                let mut signature = original.clone();
                for param in &mut signature.params {
                    if let Param::Address {
                        place: PlaceTy::Declared(id),
                        ..
                    } = param
                    {
                        *id = TypeId(visit(id.0, &mut order, &mut rows, &mut pending, &mut held)?);
                    }
                }
                if let Some(id) = &mut signature.dest {
                    *id = TypeId(visit(id.0, &mut order, &mut rows, &mut pending, &mut held)?);
                }
                Ok(Some(ClosureHook {
                    definition: key.definition.clone(),
                    dictionaries: key.dictionaries.clone(),
                    arguments,
                    signature,
                }))
            };
            // A codec maker is selected by the checking context. It is type
            // metadata and contributes no layout or calling-convention facts.
            let codec = shape.codec.take();
            let key_codec = shape.key_codec.take();
            let hooks = [
                hook(shape.finalizer.take())?,
                hook(shape.clone.take())?,
                match kind {
                    ClosureKind::Full => hook(codec)?,
                    ClosureKind::Abi => None,
                },
                match kind {
                    ClosureKind::Full => hook(key_codec)?,
                    ClosureKind::Abi => None,
                },
            ];
            rows[order[&t] as usize] = Some(ClosureRow { shape, hooks });
        }
        if let Some(budget) = budget {
            budget.allocation::<ClosureRow>(rows.len())?;
        }
        let rows: Vec<_> = rows
            .into_iter()
            .map(|r| r.expect("every visited row is filled"))
            .collect();
        let hash = match budget {
            Some(budget) => crate::normal::decoded_closure_hash(&rows, budget)?,
            None => crate::normal::closure_hash(&rows),
        };
        Ok(Closure { rows, hash })
    }
}
