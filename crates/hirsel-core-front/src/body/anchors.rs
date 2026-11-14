//! Immutable facts of scoped values, independent of their emitted representation.
//! Field nodes retain provenance through projection/overwrite; joins share their inputs.
use super::*;

pub(super) type Fact = u32;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Component {
    Field(u32),
    Case(u32, u32),
    Element(Option<u64>),
}
#[derive(Clone, Copy)]
enum Root {
    Place(Place),
    Region(crate::types::Root),
    Temporary(NodeId),
    Owner(ValueId),
}
#[derive(Clone, Copy)]
enum Node {
    Empty,
    Root(Root),
    Union(Fact, Fact),
    Field(Component, Fact),
}
pub(super) struct Anchors {
    nodes: Vec<Node>,
    declared: HashMap<u32, Fact>,
    mut_params: HashMap<u32, Fact>,
    pub(super) expressions: HashMap<NodeId, Fact>,
    pub(super) place: Option<(NodeId, At)>,
    pub(super) values: HashMap<ValueId, Fact>,
    pub(super) last: Fact,
    seen: Vec<u32>,
    generation: u32,
    stack: Vec<Fact>,
    left: Vec<Root>,
    right: Vec<Root>,
}
impl Anchors {
    pub(super) fn new(_slots: usize) -> Self {
        Self {
            nodes: vec![],
            declared: HashMap::default(),
            mut_params: HashMap::default(),
            expressions: HashMap::default(),
            place: None,
            values: HashMap::default(),
            last: 0,
            seen: vec![],
            generation: 0,
            stack: vec![],
            left: vec![],
            right: vec![],
        }
    }
    fn node(&mut self, node: Node) -> Fact {
        if self.nodes.is_empty() {
            self.nodes.push(Node::Empty);
        }
        let id = self.nodes.len() as u32;
        self.nodes.push(node);
        id
    }
    pub(super) fn union(&mut self, a: Fact, b: Fact) -> Fact {
        if a == 0 {
            b
        } else if b == 0 || a == b {
            a
        } else {
            self.node(Node::Union(a, b))
        }
    }
    fn field(&mut self, component: Component, value: Fact) -> Fact {
        if value == 0 {
            0
        } else {
            self.node(Node::Field(component, value))
        }
    }
    pub(super) fn project(&mut self, value: Fact, component: Component) -> Fact {
        if value == 0 {
            return 0;
        }
        match self.nodes[value as usize] {
            Node::Empty => 0,
            Node::Root(_) => value,
            Node::Union(a, b) => {
                let a = self.project(a, component);
                let b = self.project(b, component);
                self.union(a, b)
            }
            Node::Field(c, f) => {
                if c == component
                    || matches!(
                        (c, component),
                        (Component::Element(_), Component::Element(None))
                            | (Component::Element(None), Component::Element(_))
                    )
                {
                    f
                } else {
                    0
                }
            }
        }
    }
    fn replace(&mut self, value: Fact, path: &[Component], new: Fact) -> Fact {
        let Some((&first, rest)) = path.split_first() else {
            return new;
        };
        let old = self.project(value, first);
        let next = self.replace(old, rest, new);
        let retained = self.without(value, first);
        let next = self.field(first, next);
        self.union(retained, next)
    }
    fn without(&mut self, value: Fact, component: Component) -> Fact {
        if value == 0 {
            return 0;
        }
        match self.nodes[value as usize] {
            Node::Union(a, b) => {
                let a = self.without(a, component);
                let b = self.without(b, component);
                self.union(a, b)
            }
            Node::Field(c, _) if c == component => 0,
            _ => value,
        }
    }
    fn collect(&mut self, value: Fact, roots: &mut Vec<Root>) {
        if value == 0 {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.seen.fill(0);
            self.generation = 1;
        }
        self.seen.resize(self.nodes.len(), 0);
        self.stack.push(value);
        while let Some(id) = self.stack.pop() {
            if self.seen[id as usize] == self.generation {
                continue;
            }
            self.seen[id as usize] = self.generation;
            match self.nodes[id as usize] {
                Node::Empty => {}
                Node::Root(root) => roots.push(root),
                Node::Union(a, b) => self.stack.extend([a, b]),
                Node::Field(_, f) => self.stack.push(f),
            }
        }
    }
    pub(super) fn same(&mut self, a: Fact, b: Fact, steps: &[PStep]) -> bool {
        if a == b {
            return true;
        }
        let mut left = std::mem::take(&mut self.left);
        let mut right = std::mem::take(&mut self.right);
        left.clear();
        right.clear();
        self.collect(a, &mut left);
        self.collect(b, &mut right);
        let same = |a: Root, b: Root| match (a, b) {
            (Root::Region(a), Root::Region(b)) => a == b,
            (Root::Temporary(a), Root::Temporary(b)) => a == b,
            (Root::Owner(a), Root::Owner(b)) => a == b,
            (Root::Place(a), Root::Place(b)) => {
                a.slot == b.slot
                    && a.len == b.len
                    && steps[a.start as usize..(a.start + a.len) as usize]
                        .iter()
                        .zip(&steps[b.start as usize..(b.start + b.len) as usize])
                        .all(|(a, b)| match (a, b) {
                            (PStep::Index { .. }, PStep::Index { .. }) => true,
                            _ => a == b,
                        })
            }
            _ => false,
        };
        let result = left.iter().all(|&a| right.iter().any(|&b| same(a, b)))
            && right.iter().all(|&b| left.iter().any(|&a| same(a, b)));
        self.left = left;
        self.right = right;
        result
    }
    fn permits(&mut self, allowed: Fact, actual: Fact, steps: &[PStep]) -> bool {
        if actual == 0 || allowed == actual {
            return true;
        }
        let mut left = std::mem::take(&mut self.left);
        let mut right = std::mem::take(&mut self.right);
        left.clear();
        right.clear();
        self.collect(allowed, &mut left);
        self.collect(actual, &mut right);
        let part = |actual: Root, allowed: Root| match (actual, allowed) {
            (Root::Region(a), Root::Region(b)) => a == b,
            (Root::Temporary(a), Root::Temporary(b)) => a == b,
            (Root::Owner(a), Root::Owner(b)) => a == b,
            (Root::Place(a), Root::Place(b)) => {
                a.slot == b.slot
                    && a.len >= b.len
                    && steps[a.start as usize..(a.start + b.len) as usize]
                        .iter()
                        .zip(&steps[b.start as usize..(b.start + b.len) as usize])
                        .all(|(a, b)| match (a, b) {
                            (PStep::Index { .. }, PStep::Index { .. }) => true,
                            _ => a == b,
                        })
            }
            _ => false,
        };
        let result = right.iter().all(|&a| left.iter().any(|&b| part(a, b)));
        self.left = left;
        self.right = right;
        result
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// Written scoped anchors have a fixed set, even for an empty initializer.
    pub(super) fn written_anchors(&mut self, n: NodeId) -> Option<Fact> {
        let mut result = None;
        if self.kind(n) == NodeKind::Anchor {
            let root = *self.kids(n).last().unwrap();
            if let Some(Target::Binder(slot)) = self.p.target(self.at, root).copied() {
                let region=self.vars[slot as usize].is_some_and(|v|matches!(self.t.kind(self.bare(v.ty)),TyKind::Adt(d,_) if self.p.decls[d.index()].role==Some(Role::Region)));
                let fact = if region {
                    let ty = self.body_root(root, false);
                    if let TyKind::Region(r) = self.t.kind(ty) {
                        self.anchors.node(Node::Root(Root::Region(*r)))
                    } else {
                        0
                    }
                } else if self.vars[slot as usize].is_some() {
                    let ty = self.var(slot).ty;
                    self.anchor_read(At {
                        place: Place::whole(slot),
                        ty,
                        shared: false,
                    })
                } else {
                    let ty = self.body_root(root, false);
                    if let TyKind::Region(r) = self.t.kind(ty) {
                        self.anchors.node(Node::Root(Root::Region(*r)))
                    } else {
                        0
                    }
                };
                result = Some(fact);
            }
        }
        for &child in self.kids(n) {
            if is_type_node(self.kind(child))
                && let Some(fact) = self.written_anchors(child)
            {
                result = Some(self.anchors.union(result.unwrap_or(0), fact));
            }
        }
        result
    }
    pub(super) fn declare_anchors(
        &mut self,
        slot: u32,
        declared: Option<Fact>,
        initial: Fact,
        n: NodeId,
        written: NodeId,
    ) {
        if let Some(declared) = declared {
            self.anchors.declared.insert(slot, declared);
        }
        if let Some(allowed) = declared {
            if !self.anchors.permits(allowed, initial, &self.steps) {
                self.refuse(
                    "OWN-19",
                    n,
                    "a scoped slot may read only its declared anchors",
                );
            }
            self.set_slot_anchors(slot, allowed);
            self.freeze_anchors(allowed, self.depth, written);
        } else {
            self.set_slot_anchors(slot, initial);
        }
    }
    #[inline(always)]
    pub(super) fn type_kind(&mut self, ty: TyId) -> Kind {
        if ty.0 <= HOLE.0 {
            return Kind::Plain;
        }
        if let Some((kind, _)) = self.kind_cache.get(&ty) {
            return *kind;
        }
        let kind = kind(&self.p.decls, &self.t, ty, &self.params);
        self.kind_cache
            .insert(ty, (kind, std::cell::Cell::new(None)));
        kind
    }
    #[inline(always)]
    pub(super) fn anchor_tracked(&mut self, ty: TyId) -> bool {
        matches!(self.type_kind(ty), Kind::Scoped | Kind::Stable)
    }
    pub(super) fn anchor_regions(&mut self, ty: TyId) -> Fact {
        fn walk(p: &Program<'_>, t: &impl TyRead, anchors: &mut Anchors, ty: TyId) -> Fact {
            match t.kind(ty) {
                TyKind::Region(root) => anchors.node(Node::Root(Root::Region(*root))),
                TyKind::View(x) => walk(p, t, anchors, *x),
                TyKind::Array(x, _) => {
                    let value = walk(p, t, anchors, *x);
                    anchors.field(Component::Element(None), value)
                }
                TyKind::Anchored(x, r) => {
                    let a = walk(p, t, anchors, *x);
                    let b = walk(p, t, anchors, *r);
                    anchors.union(a, b)
                }
                TyKind::Tuple(args) => {
                    let mut fact = 0;
                    for (i, &ty) in args.iter().enumerate() {
                        let root = walk(p, t, anchors, ty);
                        let root = anchors.field(Component::Field(i as u32), root);
                        fact = anchors.union(fact, root);
                    }
                    fact
                }
                TyKind::Adt(d, args) => {
                    let mut fact = 0;
                    if is_core_prelude(&p.decls[d.index()].path, &["Option", "Result"]) {
                        let DeclBody::Variant(cases) = &p.decls[d.index()].body else {
                            unreachable!()
                        };
                        for (c, case) in cases.iter().enumerate() {
                            for (i, field) in case.fields.iter().enumerate() {
                                let ty = match t.kind(field.ty) {
                                    TyKind::Param(i) => args[*i as usize],
                                    _ => field.ty,
                                };
                                let root = walk(p, t, anchors, ty);
                                let root = anchors.field(Component::Case(c as u32, i as u32), root);
                                fact = anchors.union(fact, root);
                            }
                        }
                    } else {
                        for &ty in args {
                            let root = walk(p, t, anchors, ty);
                            fact = anchors.union(fact, root);
                        }
                    }
                    fact
                }
                _ => 0,
            }
        }
        walk(self.p, &self.t, &mut self.anchors, ty)
    }
    /// The region being closed must outlive every root in its result, including
    /// a scoped callable whose root is carried only in static anchor facts.
    pub(super) fn region_result_anchors(&mut self, scope: usize, n: NodeId, ty: TyId) {
        if !self.anchor_tracked(ty) {
            return;
        }
        let typed = self.anchor_regions(ty);
        let fact = self.anchors.union(typed, self.anchor_expression(n));
        let mut roots = std::mem::take(&mut self.anchors.left);
        roots.clear();
        self.anchors.collect(fact, &mut roots);
        let escapes = roots.iter().any(|root| matches!(root,
            Root::Region(crate::types::Root::Local(slot)) if self.scope_slots[scope..].contains(slot)));
        self.anchors.left = roots;
        if escapes {
            self.refuse_at(
                "OWN-19",
                self.close_brace(n),
                "a scoped value cannot leave its region anchor",
            );
        }
    }

    pub(super) fn result_place_anchors(&mut self, scope: usize, n: NodeId, ty: TyId) {
        if self.anchors.last == 0 || !self.live() || !self.contains_callback(ty) {
            return;
        }
        let mut roots = std::mem::take(&mut self.anchors.left);
        roots.clear();
        self.anchors.collect(self.anchors.last, &mut roots);
        let escapes = roots.iter().any(|root| {
            matches!(root,
            Root::Place(place) if self.scope_slots[scope..].contains(&place.slot))
        });
        self.anchors.left = roots;
        if escapes {
            self.refuse_at(
                "OWN-19",
                self.close_brace(n),
                "a scoped value cannot leave its local place anchor",
            );
        }
    }

    pub(super) fn return_place_anchors(&mut self) {
        if self.anchors.last == 0 || !self.contains_callback(self.fd.result) {
            return;
        }
        let mut roots = std::mem::take(&mut self.anchors.left);
        roots.clear();
        self.anchors.collect(self.anchors.last, &mut roots);
        let binders = &self.p.items[self.at.module as usize][self.at.item as usize]
            .resolution
            .binders;
        let escapes = roots.iter().any(|root| match root {
            Root::Owner(_) | Root::Temporary(_) => true,
            Root::Place(place) => {
                let b = &binders[place.slot as usize];
                !self.fd.anchors.iter().any(|&i| {
                    let p = &self.fd.params[i as usize];
                    b.name == p.name
                        && p.site.span.start <= b.site.span.start
                        && b.site.span.end <= p.site.span.end
                })
            }
            Root::Region(_) => false,
        });
        self.anchors.left = roots;
        if escapes
            && !self
                .p
                .refusals
                .iter()
                .any(|r| r.rule == "OWN-20" && r.primary == self.fd.result_site)
        {
            self.out.refusals.push(refusal(
                "OWN-20",
                self.fd.result_site,
                "a scoped result may read only its declared anchors",
            ));
            self.halt();
        }
    }

    pub(super) fn parameter_anchors(&mut self, slot: u32, ty: TyId, mode: Mode, index: u32) {
        let tracked = self.anchor_tracked(ty);
        if !tracked && mode != Mode::Mut {
            return;
        }
        let regions = self.anchor_regions(ty);
        if !tracked && regions == 0 {
            return;
        }
        let stable = self.type_kind(ty) == Kind::Stable;
        let initial = if stable {
            if regions != 0 {
                regions
            } else {
                self.anchors
                    .node(Node::Root(Root::Region(crate::types::Root::Parameter(
                        index,
                    ))))
            }
        } else if tracked {
            self.anchor_root(Place::whole(slot))
        } else {
            0
        };
        self.replace_slot_anchors(slot, initial);
        let writer = matches!(self.t.kind(self.bare(ty)),TyKind::Adt(d,_) if matches!(self.p.decls[d.index()].role,Some(Role::Writer|Role::Dest)));
        if mode == Mode::Mut && !writer && (tracked || regions != 0) {
            let own = if tracked && !stable {
                initial
            } else {
                self.anchor_root(Place::whole(slot))
            };
            let allowed = self.anchors.union(own, regions);
            let allowed = self.anchors.union(allowed, initial);
            self.anchors.mut_params.insert(slot, allowed);
            self.vars[slot as usize].as_mut().unwrap().mut_anchor = true;
        }
    }
    pub(super) fn slot_anchors(&self, slot: u32) -> Fact {
        self.vars[slot as usize].map_or(0, |v| v.anchors)
    }
    pub(super) fn replace_slot_anchors(&mut self, slot: u32, fact: Fact) {
        self.vars[slot as usize]
            .as_mut()
            .expect("declared slot")
            .anchors = fact;
    }
    pub(super) fn retain_anchor_owners(&mut self, fact: Fact) {
        if fact == 0 {
            return;
        }
        let mut roots = std::mem::take(&mut self.anchors.left);
        roots.clear();
        self.anchors.collect(fact, &mut roots);
        for &root in &roots {
            if let Root::Owner(value) = root
                && let Some(i) = self.temp_index(Some(value))
            {
                self.retained_owners.push(i);
            }
        }
        self.anchors.left = roots;
    }
    pub(super) fn anchor_expression(&self, n: NodeId) -> Fact {
        self.anchors.expressions.get(&n).copied().unwrap_or(0)
    }
    pub(super) fn set_slot_anchors(&mut self, slot: u32, fact: Fact) {
        if self.slot_anchors(slot) != fact {
            let v = self.var(slot);
            self.log.push((
                slot,
                v.avail,
                v.at,
                self.descriptors.slots.get(&slot).copied(),
                self.slot_anchors(slot),
            ));
            self.replace_slot_anchors(slot, fact);
        }
    }
    pub(super) fn anchor_field(&mut self, n: NodeId, component: Component, child: NodeId) {
        let value = self.anchor_expression(child);
        if value == 0 {
            return;
        }
        let field = self.anchors.field(component, value);
        let old = self.anchor_expression(n);
        let joined = self.anchors.union(old, field);
        self.anchors.expressions.insert(n, joined);
    }
    fn anchor_temporary(&mut self, n: NodeId) -> Fact {
        let mut source = n;
        let root = loop {
            if let Some(value) = self.owners.iter().rev().find_map(|owner| match owner {
                cleanup::Owner::Temp(temp)
                    if temp.source == source && temp.avail != Avail::Moved =>
                {
                    Some(temp.value)
                }
                _ => None,
            }) {
                break Root::Owner(value);
            }
            // Wrappers forward their tail's owner without creating a new one.
            // Peel them only when a scoped result actually reaches that owner.
            source = match self.kind(source) {
                NodeKind::Group | NodeKind::ExpressionStatement => self.kids(source)[0],
                NodeKind::Block | NodeKind::Region | NodeKind::Bound => {
                    let Some(&tail) = self.kids(source).last() else {
                        break Root::Temporary(n);
                    };
                    tail
                }
                _ => break Root::Temporary(n),
            };
        };
        self.anchors.node(Node::Root(root))
    }
    pub(super) fn anchor_argument(&mut self, n: NodeId) -> Fact {
        let n = if matches!(
            self.kind(n),
            NodeKind::LabelledArgument | NodeKind::BytesArgument
        ) {
            self.kids(n)[0]
        } else {
            n
        };
        let fact = self.anchor_expression(n);
        if fact != 0 {
            return fact;
        }
        if let Some((_, at)) = self.anchors.place.filter(|(node, _)| *node == n) {
            self.anchor_read(at)
        } else {
            self.anchor_temporary(n)
        }
    }
    pub(super) fn anchor_payload(&mut self, n: NodeId, ty: TyId, value: Option<ValueId>) {
        if self.type_kind(ty) != Kind::Scoped {
            return;
        }
        let fact = self.anchor_argument(n);
        self.anchors.last = fact;
        if fact != 0 {
            self.anchors.expressions.insert(n, fact);
            if let Some(value) = value {
                self.anchors.values.insert(value, fact);
            }
        }
    }
    pub(super) fn anchor_root(&mut self, place: Place) -> Fact {
        self.anchors.node(Node::Root(Root::Place(place)))
    }
    pub(super) fn anchor_read(&mut self, at: At) -> Fact {
        if matches!(
            self.type_kind(self.var(at.place.slot).ty),
            Kind::Scoped | Kind::Stable
        ) {
            self.anchor_place(at)
        } else {
            self.anchor_root(at.place)
        }
    }
    pub(super) fn freeze_anchors(&mut self, fact: Fact, depth: u32, site: NodeId) {
        let mut roots = std::mem::take(&mut self.anchors.left);
        roots.clear();
        self.anchors.collect(fact, &mut roots);
        for &root in &roots {
            if let Root::Place(place) = root {
                self.freeze(place, Hold::View(Extent::Block(depth)), site);
            }
        }
        self.anchors.left = roots;
    }
    pub(super) fn anchor_place(&mut self, at: At) -> Fact {
        let mut fact = self.slot_anchors(at.place.slot);
        for i in at.place.start..at.place.start + at.place.len {
            let component = match self.steps[i as usize] {
                PStep::Field(i) => Component::Field(i),
                PStep::Index { .. } => Component::Element(None),
                PStep::Payload | PStep::Any => continue,
            };
            fact = self.anchors.project(fact, component);
        }
        fact
    }
    pub(super) fn anchor_store(
        &mut self,
        at: At,
        value: NodeId,
        emitted: Option<ValueId>,
        site: NodeId,
    ) {
        if !self.anchor_tracked(at.ty) {
            return;
        }
        let new = self.anchor_expression(value);
        let captures = if (new == 0 && self.anchors.mut_params.contains_key(&at.place.slot))
            || self.anchors.declared.contains_key(&at.place.slot)
        {
            self.capture_anchors(emitted)
        } else {
            0
        };
        let actual = self.anchors.union(new, captures);
        if let Some(&allowed) = self.anchors.mut_params.get(&at.place.slot) {
            if !self.anchors.permits(allowed, actual, &self.steps) {
                let cause = self.declared_at(at.place.slot);
                self.refuse_from(
                    "OWN-11",
                    self.span(site),
                    vec![cause],
                    "a value stored through a `mut` parameter may read only its original anchors",
                );
            }
            return;
        }
        if let Some(&allowed) = self.anchors.declared.get(&at.place.slot) {
            if !self.anchors.permits(allowed, actual, &self.steps) {
                self.refuse(
                    "OWN-19",
                    site,
                    "a scoped slot may read only its declared anchors",
                );
            }
            return;
        }
        if new != 0 {
            self.freeze_anchors(new, self.var(at.place.slot).depth, site);
        }
        if at.place.len == 0 {
            self.set_slot_anchors(at.place.slot, new);
            return;
        }
        let mut path = std::mem::take(&mut self.anchor_path);
        path.clear();
        for i in at.place.start..at.place.start + at.place.len {
            match self.steps[i as usize] {
                PStep::Field(i) => path.push(Component::Field(i)),
                PStep::Index { .. } => path.push(Component::Element(None)),
                PStep::Payload | PStep::Any => {}
            }
        }
        let fact = self
            .anchors
            .replace(self.slot_anchors(at.place.slot), &path, new);
        self.anchor_path = path;
        self.set_slot_anchors(at.place.slot, fact);
    }
    pub(super) fn finish_anchors(&mut self, n: NodeId, t: TyId, value: Option<ValueId>) {
        let mut fact = if let Some(&fact) = self.anchors.expressions.get(&n) {
            fact
        } else {
            let kids = self.kids(n);
            match self.kind(n) {
                NodeKind::Path | NodeKind::Projection | NodeKind::Index => {
                    if let Some((_, at)) = self.anchors.place.filter(|(node, _)| *node == n) {
                        self.anchor_read(at)
                    } else if let Some(&base) = kids.first() {
                        let fact = self.anchor_expression(base);
                        if fact != 0 {
                            fact
                        } else {
                            self.anchor_temporary(base)
                        }
                    } else {
                        0
                    }
                }
                NodeKind::Unary if matches!(self.token_text(n), "view" | "take") => {
                    if let Some((_, at)) = self.anchors.place.filter(|(node, _)| *node == kids[0]) {
                        self.anchor_read(at)
                    } else {
                        self.anchor_expression(kids[0])
                    }
                }
                NodeKind::Unary | NodeKind::Group => self.anchor_expression(kids[0]),
                NodeKind::Block => kids
                    .last()
                    .filter(|&&k| self.kind(k) == NodeKind::ExpressionStatement)
                    .map(|&k| self.anchor_expression(self.kids(k)[0]))
                    .unwrap_or(0),
                NodeKind::If => {
                    let a = self.anchor_expression(kids[1]);
                    let b = kids.get(2).map_or(0, |&n| self.anchor_expression(n));
                    self.anchors.union(a, b)
                }
                _ => value
                    .and_then(|v| self.anchors.values.get(&v).copied())
                    .unwrap_or(0),
            }
        };
        if fact == 0
            && let TyKind::Anchored(_, root) = self.t.kind(t)
            && let TyKind::Region(root) = self.t.kind(*root)
        {
            let region = self.anchors.node(Node::Root(Root::Region(*root)));
            fact = self.anchors.union(fact, region);
        }
        if fact != 0 {
            self.anchors.expressions.insert(n, fact);
            if let Some(value) = value {
                self.anchors.values.insert(value, fact);
            }
        }
        self.anchors.last = fact;
    }
}
