use super::*;
use bulk::Bytes;
use hirsel_core_layout::RecipeRow;

/// The module's type layouts, computed on the first latch that copies a
/// declared element and charged to the module-wide scratch that holds them.
pub(super) struct Layouts<'s, 'b> {
    scratch: &'s Debit<'b>,
    rows: Option<Vec<RecipeRow>>,
}

impl<'s, 'b> Layouts<'s, 'b> {
    pub(super) fn new(scratch: &'s Debit<'b>) -> Self {
        Self {
            scratch,
            rows: None,
        }
    }

    /// The logical (MEM-24) size the frontend charges per element of `t`;
    /// none for a compact layout, which no fixed charge covers.
    fn logical(&mut self, m: &Module, t: TypeId) -> Result<Option<u64>, Error> {
        let rows = match &mut self.rows {
            Some(rows) => rows,
            None => self
                .rows
                .insert(crate::wf::own_names::layouts(m, self.scratch)?),
        };
        Ok(hirsel_core_layout::charge::held(
            m.types[t.index()].repr,
            &rows[t.index()],
        ))
    }
}

// A counted repeat has one entry from the paying block, a `<` test and a
// straight-line latch that stores or copies one element. Only the proved latch
// edge can be omitted. Every other incoming edge, including a failure edge,
// invalidates this proof. With `refuse`, a recognized repeat without its
// precharge is an error rather than a cycle left for the graph.
pub(super) fn paid_repeats(
    m: &Module,
    body: &Body,
    facts: &bulk::Facts<'_>,
    layouts: &mut Layouts<'_, '_>,
    refuse: bool,
    budget: &Debit<'_>,
) -> Result<Vec<bool>, Error> {
    let mut paid = Vec::new();
    budget.reserve(&mut paid, body.blocks.len())?;
    paid.resize(body.blocks.len(), false);
    for (bi, term) in body.terminators.iter().enumerate() {
        budget.check()?;
        if let Terminator::Jump(edge) = term
            && let Some(latch) = repeat(
                m,
                body,
                facts,
                layouts,
                bi,
                &body.edges[edge.index()],
                refuse,
                budget,
            )?
        {
            paid[latch] = true;
        }
    }
    Ok(paid)
}

/// The bytes of the one element a latch writes.
///
/// The longest latch the frontend emits for one element: its offset, two
/// addresses, the element's size, the write and the increment, with room for
/// the constants it materializes.
const MAX_LATCH: usize = 8;

fn unit(
    m: &Module,
    body: &Body,
    facts: &bulk::Facts<'_>,
    layouts: &mut Layouts<'_, '_>,
    rows: &[Inst],
    index: ValueId,
    next: ValueId,
    budget: &Debit<'_>,
) -> Result<Option<u64>, Error> {
    // Each paid unit covers one iteration of a short latch, so the work per
    // charged byte stays within a constant factor of a bulk copy's.
    if rows.len() > MAX_LATCH {
        return Ok(None);
    }
    let mut found = None;
    let mut increment = false;
    for row in rows {
        budget.check()?;
        let write = match *row {
            Inst::Store { address, .. } => match body.addresses[address.index()].ty {
                PlaceTy::Value(Ty::Scalar(s)) => Some(s.size()),
                _ => return Ok(None),
            },
            // One element's bytes, sized by the copied place's own type.
            Inst::Copy { to, bytes, .. } => {
                match (body.addresses[to.index()].ty, facts.definition(bytes)) {
                    (
                        PlaceTy::Declared(t),
                        Some(Inst::LayoutConst {
                            query: LayoutQuery::HeldSize(held),
                            ..
                        }),
                    ) if *held == t => match layouts.logical(m, t)? {
                        Some(bytes) => Some(bytes),
                        None => return Ok(None),
                    },
                    _ => return Ok(None),
                }
            }
            Inst::Binary {
                result,
                op: BinaryOp::Add,
                left,
                right,
                ..
            }
            | Inst::IntArith {
                result,
                op: ArithOp::Add,
                mode: ArithMode::Wrapping,
                left,
                right,
                ..
            } if result == next => {
                if left != index || facts.constant(right) != Some(1) {
                    return Ok(None);
                }
                increment = true;
                None
            }
            // The element's address: its byte offset, a constant or a size.
            Inst::IntArith {
                op: ArithOp::Mul,
                mode: ArithMode::Wrapping,
                left,
                ..
            } if left == index => None,
            Inst::Const { .. } | Inst::Address { .. } | Inst::LayoutConst { .. } => None,
            _ => return Ok(None),
        };
        if let Some(write) = write {
            if found.is_some() {
                return Ok(None);
            }
            found = Some(write);
        }
    }
    // A zero-sized element pays nothing, so it cannot bound the iterations.
    Ok(found.filter(|&bytes| increment && bytes > 0))
}

fn repeat(
    m: &Module,
    body: &Body,
    facts: &bulk::Facts<'_>,
    layouts: &mut Layouts<'_, '_>,
    pre: usize,
    entry: &Edge,
    refuse: bool,
    budget: &Debit<'_>,
) -> Result<Option<usize>, Error> {
    let head = entry.target.index();
    let params = |b: usize| {
        body.blocks[b]
            .params
            .get(&body.pools.values)
            .unwrap_or_default()
    };
    let instructions = |b: usize| body.blocks[b].insts.get(&body.insts).unwrap_or_default();
    let [initial] = entry.args.get(&body.pools.values).unwrap_or_default() else {
        return Ok(None);
    };
    let [index] = params(head) else {
        return Ok(None);
    };
    let [
        Inst::Compare {
            result,
            op: CompareOp::Lt,
            left,
            right: bound,
        },
    ] = instructions(head)
    else {
        return Ok(None);
    };
    let Terminator::Branch { condition, yes, no } = body.terminators[head] else {
        return Ok(None);
    };
    let latch = body.edges[yes.index()].target.index();
    if head == latch
        || pre == head
        || pre == latch
        || facts.incoming(head) != 2
        || facts.incoming(latch) != 1
        || condition != *result
        || left != index
        || body.edges[no.index()].target.index() == latch
        || body.values[index.index()] != Ty::Scalar(Scalar::U64)
        || !params(latch).is_empty()
    {
        return Ok(None);
    }
    let Terminator::Jump(back) = body.terminators[latch] else {
        return Ok(None);
    };
    let back = &body.edges[back.index()];
    let [next] = back.args.get(&body.pools.values).unwrap_or_default() else {
        return Ok(None);
    };
    if back.target.index() != head {
        return Ok(None);
    }
    let Some(unit) = unit(
        m,
        body,
        facts,
        layouts,
        instructions(latch),
        *index,
        *next,
        budget,
    )?
    else {
        return Ok(None);
    };
    // The trip count: `bound` from zero, or `count` from `initial` to
    // `initial + count` (a wrapped end runs no iteration). An unknown count is
    // an SSA value; the charge must name that same value with nothing
    // redefining it in between, so no memory read stands in for the charge.
    let elements = |count: ValueId| match facts.constant(count) {
        Some(n) => Bytes::Fixed(n.saturating_mul(unit)),
        None => Bytes::Exact(count, unit),
    };
    let (needed, after) = match (facts.definition(*bound), facts.constant(*initial)) {
        (
            Some(Inst::IntArith {
                op: ArithOp::Add,
                mode: ArithMode::Wrapping,
                left,
                right: count,
                ..
            }),
            _,
        ) if left == initial => (elements(*count), Some(*bound)),
        (_, Some(0)) => (elements(*bound), None),
        _ => return Ok(None),
    };
    if !facts.prepaid(
        body,
        pre,
        instructions(pre).len(),
        needed,
        after,
        hirsel_core_ids::work_steps::table::COPY,
        budget,
    )? {
        if !refuse {
            return Ok(None);
        }
        // An unpaid element repeat is refused even where the cycle rule in
        // force would not reach it.
        return Err(Error::Invalid(format!(
            "b{head}: element repeat lacks matching MEM-30 precharge"
        )));
    }
    Ok(Some(latch))
}
