//! Declaration-path memoization for finite instantiated nominal graphs.
use crate::*;
use hirsel_intern::work::{self, Counter};
type HashMap<K, V> = std::collections::HashMap<K, V, hirsel_intern::IdHasher>;
type HashSet<K> = std::collections::HashSet<K, hirsel_intern::IdHasher>;

/// Operational budget for strictly growing polymorphic recursion. Ordinary
/// graph width, depth and regular recursive cycles do not consume this budget.
pub const MAX_POLYMORPHIC_EXPANSIONS: usize = 128;

pub fn type_proof_limit() -> QueryError {
    QueryError::Operational(OperationalFailure {
        operation: "type-proof-tooling-limit".into(),
        message: format!(
            "non-regular polymorphic recursion exceeded {MAX_POLYMORPHIC_EXPANSIONS} growing declaration expansions"
        ),
    })
}

// A constructor header with type edges erased. Sizes and declaration identity
// remain exact; a generic argument permutation is not constructor growth.
fn head(mut kind: TypeKind, marker: TypeId) -> TypeKind {
    match &mut kind {
        TypeKind::Tuple(ts) => ts.iter_mut().for_each(|t| *t = marker),
        TypeKind::Record(fs) => fs.iter_mut().for_each(|f| f.ty = marker),
        TypeKind::Variant(vs) => vs
            .iter_mut()
            .for_each(|v| v.fields.iter_mut().for_each(|f| f.1 = marker)),
        TypeKind::Box(t) => *t = marker,
        TypeKind::Named { arguments, .. } | TypeKind::Builtin { arguments, .. } => {
            arguments.iter_mut().for_each(|a| {
                if let GenericArgument::Type(t) = a {
                    *t = marker;
                }
            })
        }
        TypeKind::Function(f) => {
            f.parameters.iter_mut().for_each(|p| p.ty = marker);
            f.result = marker;
            if f.error.is_some() {
                f.error = Some(marker);
            }
        }
        _ => (),
    }
    kind
}
/// Heights of the rows a proof reaches. Structural classes are the
/// interner's semantic normal forms: two valid graphs share a class exactly
/// when `semantic_eq` holds.
#[derive(Default)]
pub(crate) struct Shapes {
    heights: HashMap<TypeId, Option<usize>>,
}
impl Shapes {
    /// The class of a valid graph; None for one `semantic_eq` rejects.
    fn class(&self, t: &TypeInterner, root: TypeId) -> Option<TypeId> {
        t.valid(root).then(|| t.sem(root))
    }
    /// Rows on the longest edge path; None when the graph is invalid.
    fn height(&mut self, t: &TypeInterner, root: TypeId) -> Option<usize> {
        if let Some(height) = self.heights.get(&root) {
            return *height;
        }
        if !t.valid(root) {
            self.heights.insert(root, None);
            return None;
        }
        let mut pending = vec![(root, false)];
        while let Some((id, exit)) = pending.pop() {
            if self.heights.contains_key(&id) {
                continue;
            }
            let children: Vec<_> = t
                .row(id)
                .kind
                .graph_edges()
                .into_iter()
                .map(|e| e.ty)
                .collect();
            if !exit {
                pending.push((id, true));
                pending.extend(children.into_iter().map(|c| (c, false)));
                continue;
            }
            work::count(Counter::TypeNormalRows);
            let height = children
                .iter()
                .map(|c| self.heights[c].expect("a valid graph's rows have heights"))
                .max()
                .unwrap_or(0)
                + 1;
            self.heights.insert(id, Some(height));
        }
        self.heights[&root]
    }
    fn equal(&mut self, t: &TypeInterner, a: TypeId, b: TypeId) -> bool {
        let a = self.class(t, a);
        a.is_some() && a == self.class(t, b)
    }
    // Homeomorphic embedding, using a finite pair worklist instead of the host
    // stack. Some(true) includes a strict constructor insertion; None cannot embed.
    fn embedding(&mut self, t: &TypeInterner, old: TypeId, new: TypeId) -> Option<bool> {
        // A shrinking actual cannot embed the older argument. This also keeps
        // finite nested generic aliases out of the pair-product worklist.
        match (self.height(t, old), self.height(t, new)) {
            (Some(a), Some(b)) if a <= b => (),
            _ => return None,
        }
        let mut memo = HashMap::default();
        let mut pending = vec![(old, new, false)];
        while let Some((a, b, exit)) = pending.pop() {
            if memo.contains_key(&(a, b)) {
                continue;
            }
            if self.equal(t, a, b) {
                memo.insert((a, b), Some(false));
                continue;
            }
            let (Some(ar), Some(br)) = (t.get(a), t.get(b)) else {
                memo.insert((a, b), None);
                continue;
            };
            let ac = ar.kind.graph_edges();
            let bc = br.kind.graph_edges();
            let coupled = head(ar.kind.clone(), old) == head(br.kind.clone(), old);
            if !exit {
                pending.push((a, b, true));
                pending.extend(bc.iter().map(|c| (a, c.ty, false)));
                if coupled {
                    pending.extend(ac.iter().zip(&bc).map(|(a, b)| (a.ty, b.ty, false)));
                }
                continue;
            }
            let children = coupled
                .then(|| {
                    ac.iter()
                        .zip(&bc)
                        .map(|(a, b)| memo[&(a.ty, b.ty)])
                        .collect::<Option<Vec<_>>>()
                })
                .flatten();
            let result = if let Some(children) = children {
                Some(children.into_iter().any(|grew| grew))
            } else if bc.iter().any(|child| memo[&(a, child.ty)].is_some()) {
                Some(true)
            } else {
                None
            };
            memo.insert((a, b), result);
        }
        memo[&(old, new)]
    }

    /// All old arguments embed in the new arguments of the same declaration, with
    /// at least one strict constructor insertion. This includes growth inside a
    /// fixed outer constructor. Sizes cannot grow (LANG-7). Shrinking finite actuals
    /// and regular cycles/permutations spend no polymorphic expansion budget.
    fn arguments_grow(&mut self, t: &TypeInterner, old: TypeId, new: TypeId) -> bool {
        let (
            Some(Type {
                kind:
                    TypeKind::Named {
                        definition: a,
                        arguments: aa,
                    },
                ..
            }),
            Some(Type {
                kind:
                    TypeKind::Named {
                        definition: b,
                        arguments: ba,
                    },
                ..
            }),
        ) = (t.get(old), t.get(new))
        else {
            return false;
        };
        if a != b || aa.len() != ba.len() {
            return false;
        }
        // An older root taller than the new one has an argument taller than
        // every new argument, which cannot embed.
        match (self.height(t, old), self.height(t, new)) {
            (Some(a), Some(b)) if a <= b => (),
            _ => return false,
        }
        let mut grew = false;
        for (a, b) in aa.iter().zip(ba.iter()) {
            match (a, b) {
                (GenericArgument::Size(a), GenericArgument::Size(b)) if a == b => (),
                (GenericArgument::Type(a), GenericArgument::Type(b)) => {
                    match self.embedding(t, *a, *b) {
                        Some(growth) => grew |= growth,
                        None => return false,
                    }
                }
                _ => return false,
            }
        }
        grew
    }
}

/// A proof's finite instantiation memo and active declaration path. Context
/// distinguishes inherited uniqueness or ownership state when the property
/// depends on it. Equality and growth read the session interner.
#[derive(Default)]
pub struct NominalExpansions {
    shapes: Shapes,
    memo: HashSet<(TypeId, usize)>,
    /// Entered roots, with the declaration whose `lowest` entry each pushed.
    active: Vec<(TypeId, usize, bool, Option<DefId>)>,
    /// Per declaration, the least height of its active instantiations at
    /// each nesting: only an instantiation no taller than a new root can
    /// embed in it, so a shrinking path is checked without a scan.
    lowest: HashMap<DefId, Vec<usize>>,
    growing: usize,
}
impl NominalExpansions {
    /// False assumes an already visited instantiation coinductively. The proof
    /// must still check every other owned component before publishing success.
    pub fn enter(
        &mut self,
        root: TypeId,
        context: usize,
        t: &TypeInterner,
    ) -> Result<bool, QueryError> {
        if let Some(class) = self.shapes.class(t, root)
            && !self.memo.insert((class, context))
        {
            return Ok(false);
        }
        let height = self.shapes.height(t, root);
        let definition = match t.get(root).map(|r| &r.kind) {
            Some(TypeKind::Named { definition, .. }) => Some(*definition),
            _ => None,
        };
        let candidates = match (&definition, height) {
            (Some(d), Some(h)) => self
                .lowest
                .get(d)
                .and_then(|l| l.last())
                .is_some_and(|lowest| *lowest <= h),
            _ => false,
        };
        let growth = candidates
            && (0..self.active.len())
                .rev()
                .any(|i| self.shapes.arguments_grow(t, self.active[i].0, root));
        if growth && self.growing == MAX_POLYMORPHIC_EXPANSIONS {
            return Err(type_proof_limit());
        }
        self.growing += usize::from(growth);
        let pushed = match (definition, height) {
            (Some(d), Some(h)) => {
                let lowest = self.lowest.entry(d).or_default();
                lowest.push(lowest.last().map_or(h, |l| h.min(*l)));
                Some(d)
            }
            _ => None,
        };
        self.active.push((root, context, growth, pushed));
        Ok(true)
    }
    /// The equality class `enter` memoizes `root` under.
    pub(crate) fn class_of(&self, t: &TypeInterner, root: TypeId) -> Option<TypeId> {
        self.shapes.class(t, root)
    }
    /// `root`'s height, which bounds what can embed in it.
    pub(crate) fn height_of(&mut self, t: &TypeInterner, root: TypeId) -> Option<usize> {
        self.shapes.height(t, root)
    }
    /// Whether the last entered root spent the polymorphic expansion budget.
    pub(crate) fn last_grew(&self) -> bool {
        self.active.last().is_some_and(|a| a.2)
    }
    /// Whether an active instantiation of `definition` is no taller than
    /// `height`: only then can an instantiation of that height grow from it.
    pub(crate) fn may_grow_into(&self, definition: DefId, height: usize) -> bool {
        self.lowest
            .get(&definition)
            .and_then(|l| l.last())
            .is_some_and(|lowest| *lowest <= height)
    }
    pub fn leave(&mut self, root: TypeId, context: usize) {
        let (t, c, growth, definition) =
            self.active.pop().expect("nominal proof exit without entry");
        assert_eq!((t, c), (root, context));
        self.growing -= usize::from(growth);
        if let Some(definition) = definition {
            self.lowest.get_mut(&definition).unwrap().pop();
        }
    }
}
