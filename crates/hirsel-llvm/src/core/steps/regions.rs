//! Reserve an acyclic effect region, refunding the unexecuted path suffix.
use super::*;

pub(crate) struct Region {
    pub(crate) header: usize,
    nodes: BTreeSet<usize>,
    exits: BTreeSet<usize>,
    cost: u64,
}

impl Region {
    /// Whether this plan pays ahead for steps of `block`.
    pub(crate) fn covers(&self, block: usize) -> bool {
        self.nodes.contains(&block)
    }
}

fn visit(
    body: &Body,
    block: usize,
    allowed: &[bool],
    header: usize,
    active: &mut BTreeSet<usize>,
    costs: &mut BTreeMap<usize, u64>,
) -> Option<u64> {
    if !allowed[block] {
        return Some(0);
    }
    if let Some(&cost) = costs.get(&block) {
        return Some(cost);
    }
    // Bound planner recursion as well as emitted expansion.
    if active.len() + costs.len() >= 64 || !active.insert(block) {
        return None;
    }
    let mut tail = 0;
    for next in super::next(body, block).unwrap_or_default() {
        if next != header {
            tail = tail.max(visit(body, next, allowed, header, active, costs)?);
        }
    }
    active.remove(&block);
    let own = body.blocks[block]
        .insts
        .get(&body.insts)?
        .iter()
        .filter(|i| matches!(i, Inst::Step { .. }))
        .count() as u64;
    let cost = own.checked_add(tail)?;
    costs.insert(block, cost);
    Some(cost)
}

pub(crate) fn plans(
    module: &Module,
    body: &Body,
    neutral: &[bool],
    counted: &[super::Loop],
) -> Vec<Region> {
    let flow = flow::Flow::new(body);
    // Other loop headers are boundaries. A region starting at an uncounted
    // header covers one trip and refunds before its back edge.
    let mut headers = BTreeSet::new();
    for b in 0..body.blocks.len() {
        for next in super::next(body, b).unwrap_or_default() {
            if flow.block_dominates(next, b) {
                headers.insert(next);
            }
        }
    }
    let mut allowed = body
        .blocks
        .iter()
        .enumerate()
        .map(|(b, row)| {
            !matches!(body.terminators[b], Terminator::Switch { .. })
                && row
                    .insts
                    .get(&body.insts)
                    .unwrap()
                    .iter()
                    .all(|i| super::region_inst(module, body, i, neutral))
        })
        .collect::<Vec<_>>();
    let mut regions = Vec::new();
    for header in 0..body.blocks.len() {
        if !allowed[header] || counted.iter().any(|p| p.header == header) {
            continue;
        }
        let mut region_allowed = allowed.clone();
        for &h in &headers {
            if h != header {
                region_allowed[h] = false;
            }
        }
        let mut costs = BTreeMap::new();
        let Some(cost) = visit(
            body,
            header,
            &region_allowed,
            header,
            &mut BTreeSet::new(),
            &mut costs,
        ) else {
            continue;
        };
        let nodes = costs.keys().copied().collect::<BTreeSet<_>>();
        if cost < 2
            || nodes.len() < 2
            || nodes.len() > 64
            || nodes
                .iter()
                .flat_map(|&b| body.blocks[b].insts.get(&body.insts).unwrap())
                // A typed identity root emits no pointer arithmetic. Carrying
                // its memory domain must not price out an effect region.
                .filter(|inst| {
                    !matches!(inst, Inst::Address { address, .. }
                    if body.addresses[address.index()].steps.len == 0
                        && matches!(body.addresses[address.index()].base, AddressBase::Value(_))
                        && alias::writer_state_memory(module, body, *address).is_some())
                })
                .count()
                > 512
        {
            continue;
        }
        let exits = nodes
            .iter()
            .flat_map(|&b| super::next(body, b).unwrap_or_default())
            .filter(|b| *b == header || !nodes.contains(b))
            .collect();
        for &b in &nodes {
            allowed[b] = false;
        }
        regions.push(Region {
            header,
            nodes,
            exits,
            cost,
        });
    }
    regions
}
fn label(number: usize, blocks: usize, block: usize) -> Label {
    Label::new("step_region", number * blocks + block)
}
fn exit_label(number: usize, blocks: usize, block: usize) -> Label {
    Label::new("step_region_exit", number * blocks + block)
}
impl FunctionEmitter<'_> {
    pub(in crate::core) fn region_dispatch(
        &mut self,
        plan: &Region,
        window: Window,
        number: usize,
    ) {
        let counter = self.step_counter();
        let before = self.load_steps(&counter);
        let enough = self.cmp(Predicate::Sgt, &Type::I64, &before, &int(plan.cost));
        let n = self.counter();
        let fast = Label::new("step_region_reserve", n);
        let checked = Label::new("step_region_checked", n);
        self.likely(&enough, fast, checked);
        self.block(fast);
        let after = self.bin(Binary::Sub, &Type::I64, &before, &int(plan.cost));
        self.store_steps(&after, &counter);
        self.reserve_progress(window, &int(plan.cost));
        self.br(label(number, self.body.blocks.len(), plan.header));
        self.block(checked);
    }
    pub(in crate::core) fn region_version(
        &mut self,
        plan: &Region,
        window: Window,
        number: usize,
        location: impl Fn(Option<Anchor>) -> Option<usize>,
    ) -> Result<()> {
        self.step_labels = plan
            .nodes
            .iter()
            .map(|&b| (b, label(number, self.body.blocks.len(), b)))
            .chain(
                plan.exits
                    .iter()
                    .map(|&b| (b, exit_label(number, self.body.blocks.len(), b))),
            )
            .collect();
        self.unchecked_steps = true;
        self.precharge = Some(Precharge::Window {
            spent: Value::Local(window.spent),
            charged: Value::Local(window.charged),
        });
        for &b in &plan.nodes {
            self.block(label(number, self.body.blocks.len(), b));
            self.enter_block(b);
            let row = &self.body.blocks[b];
            for (offset, inst) in row.insts.get(&self.body.insts).unwrap().iter().enumerate() {
                if !matches!(inst, Inst::Const { .. }) {
                    self.debug_location =
                        location(self.body.inst_anchors[row.insts.start as usize + offset]);
                    self.inst(inst, row.insts.start as usize + offset)?;
                }
            }
            self.anchor = row.anchor;
            self.debug_location = location(row.anchor);
            self.term(b)?;
        }
        self.step_labels.clear();
        self.unchecked_steps = false;
        self.live = None;
        for &exit in &plan.exits {
            self.block(exit_label(number, self.body.blocks.len(), exit));
            self.step_refund();
            self.br(Label::block(exit));
        }
        self.precharge = None;
        Ok(())
    }
}
