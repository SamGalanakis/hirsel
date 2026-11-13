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
    pub(super) slots: Vec<Fact>,
    pub(super) expressions: HashMap<NodeId, Fact>,
    pub(super) places: HashMap<NodeId, At>,
    pub(super) values: HashMap<ValueId, Fact>,
    pub(super) constants: HashMap<ValueId, u64>,
    pub(super) last: Fact,
    seen: Vec<u32>,
    generation: u32,
    stack: Vec<Fact>,
    left: Vec<Root>,
    right: Vec<Root>,
}
impl Anchors {
    pub(super) fn new(slots: usize) -> Self {
        Self {
            nodes: vec![Node::Empty],
            slots: vec![0; slots],
            expressions: HashMap::default(),
            places: HashMap::default(),
            values: HashMap::default(),
            constants: HashMap::default(),
            last: 0,
            seen: vec![],
            generation: 0,
            stack: vec![],
            left: vec![],
            right: vec![],
        }
    }
    fn node(&mut self, node: Node) -> Fact {
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
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn anchor_expression(&self, n: NodeId) -> Fact {
        self.anchors.expressions.get(&n).copied().unwrap_or(0)
    }
    pub(super) fn set_slot_anchors(&mut self, slot: u32, fact: Fact) {
        if self.anchors.slots[slot as usize] != fact {
            let v = self.var(slot);
            self.log.push((
                slot,
                v.avail,
                v.at,
                self.descriptors.slots.get(&slot).copied(),
                self.anchors.slots[slot as usize],
            ));
            self.anchors.slots[slot as usize] = fact;
        }
    }
    pub(super) fn anchor_field(&mut self, n: NodeId, component: Component, child: NodeId) {
        let value = self.anchor_expression(child);
        let field = self.anchors.field(component, value);
        let old = self.anchor_expression(n);
        let joined = self.anchors.union(old, field);
        self.anchors.expressions.insert(n, joined);
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
        if let Some(&at) = self.anchors.places.get(&n) {
            self.anchor_read(at)
        } else {
            self.anchors.node(Node::Root(Root::Temporary(n)))
        }
    }
    pub(super) fn anchor_payload(&mut self, n: NodeId, ty: TyId, value: Option<ValueId>) {
        if kind(&self.p.decls, &self.t, ty, &self.params) != Kind::Scoped {
            return;
        }
        let fact = self.anchor_argument(n);
        self.anchors.expressions.insert(n, fact);
        self.anchors.last = fact;
        if let Some(value) = value {
            self.anchors.values.insert(value, fact);
        }
    }
    pub(super) fn anchor_root(&mut self, place: Place) -> Fact {
        self.anchors.node(Node::Root(Root::Place(place)))
    }
    pub(super) fn anchor_read(&mut self, at: At) -> Fact {
        if matches!(
            kind(
                &self.p.decls,
                &self.t,
                self.var(at.place.slot).ty,
                &self.params
            ),
            Kind::Scoped | Kind::Stable
        ) {
            self.anchor_place(at)
        } else {
            self.anchor_root(at.place)
        }
    }
    pub(super) fn freeze_anchors(&mut self, fact: Fact, depth: u32) {
        let mut roots = std::mem::take(&mut self.anchors.left);
        roots.clear();
        self.anchors.collect(fact, &mut roots);
        for &root in &roots {
            if let Root::Place(place) = root {
                self.freeze(place, Hold::View(Extent::Block(depth)));
            }
        }
        self.anchors.left = roots;
    }
    pub(super) fn anchor_place(&mut self, at: At) -> Fact {
        let mut fact = self.anchors.slots[at.place.slot as usize];
        for i in at.place.start..at.place.start + at.place.len {
            let component = match self.steps[i as usize] {
                PStep::Field(i) => Component::Field(i),
                PStep::Index { value, .. } => {
                    Component::Element(value.and_then(|v| self.anchors.constants.get(&v).copied()))
                }
                PStep::Payload | PStep::Any => continue,
            };
            fact = self.anchors.project(fact, component);
        }
        fact
    }
    pub(super) fn anchor_store(&mut self, at: At, value: NodeId) {
        let new = self.anchor_expression(value);
        if new != 0 {
            self.freeze_anchors(new, self.var(at.place.slot).depth);
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
                PStep::Index { value, .. } => path.push(Component::Element(
                    value.and_then(|v| self.anchors.constants.get(&v).copied()),
                )),
                PStep::Payload | PStep::Any => {}
            }
        }
        let fact = self
            .anchors
            .replace(self.anchors.slots[at.place.slot as usize], &path, new);
        self.anchor_path = path;
        self.set_slot_anchors(at.place.slot, fact);
    }
    pub(super) fn finish_anchors(&mut self, n: NodeId, t: TyId, value: Option<ValueId>) {
        let tracked = matches!(
            kind(&self.p.decls, &self.t, t, &self.params),
            Kind::Scoped | Kind::Stable
        );
        if !tracked {
            self.anchors.last = 0;
            return;
        }
        let mut fact = if let Some(&fact) = self.anchors.expressions.get(&n) {
            fact
        } else {
            let kids = self.kids(n);
            match self.kind(n) {
                NodeKind::Path | NodeKind::Projection | NodeKind::Index => {
                    if let Some(at) = self.anchors.places.get(&n).copied() {
                        self.anchor_read(at)
                    } else if let Some(&base) = kids.first() {
                        let fact = self.anchor_expression(base);
                        if fact != 0 {
                            fact
                        } else {
                            self.anchors.node(Node::Root(Root::Temporary(base)))
                        }
                    } else {
                        0
                    }
                }
                NodeKind::Unary if self.token_text(n) == "view" => {
                    if let Some(&at) = self.anchors.places.get(&kids[0]) {
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
        if let TyKind::Anchored(_, root) = self.t.kind(t)
            && let TyKind::Region(root) = self.t.kind(*root)
        {
            let region = self.anchors.node(Node::Root(Root::Region(*root)));
            fact = self.anchors.union(fact, region);
        }
        self.anchors.expressions.insert(n, fact);
        if let Some(value) = value {
            self.anchors.values.insert(value, fact);
        }
        self.anchors.last = fact;
    }
}
