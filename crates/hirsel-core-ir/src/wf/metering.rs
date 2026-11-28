//! Check the control flow that can run between successful charges. A failed
//! Step has paid nothing, so its failure edge stays in this graph.
use super::*;

mod bulk;
mod cursor;
mod repeat;
mod scopes;

/// How far the cycle law looks between successful charges.
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum Cycles {
    Off,
    /// Cycles whose blocks hold only scalar arithmetic, comparisons,
    /// conversions and markers; any other instruction ends the scan.
    Scalar,
    /// Cycles inside the module through any instruction kind, following
    /// direct calls to local bodies.
    Local,
    /// `Local`, plus re-entry through opaque calls, drops and runtime calls.
    Reentrant,
}

/// The metering laws, each switchable alone so admission can enforce exactly
/// the ones the closed products satisfy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Checks {
    pub cycles: Cycles,
    /// Each counted element repeat follows a COPY precharge of its count.
    pub repeats: bool,
    /// `rt.zero_bytes` follows a matching COPY precharge.
    pub zero_bytes: bool,
    /// Explicit `copy` and `move` follow a matching COPY precharge.
    pub copies: bool,
    /// `alloc` and `grow` follow a matching COPY precharge; growth moves none.
    pub allocations: bool,
    /// `rt.compare_bytes` follows a precharge of both operand sizes.
    pub comparisons: bool,
    /// CRC, SHA block and XXH3 stripe kernels follow their table precharge.
    pub kernels: bool,
    /// `rt.xxh3_short` has a constant length within its short range.
    pub short_hashes: bool,
    /// Local step scopes balance on every success and failure transfer.
    pub scopes: bool,
}

impl Checks {
    /// The laws release admission enforces.
    pub const ADMISSION: Self = Self {
        cycles: Cycles::Scalar,
        repeats: true,
        zero_bytes: true,
        copies: false,
        allocations: false,
        comparisons: false,
        kernels: false,
        short_hashes: false,
        scopes: false,
    };
    /// Every law.
    pub const STRICT: Self = Self {
        cycles: Cycles::Reentrant,
        repeats: true,
        zero_bytes: true,
        copies: true,
        allocations: true,
        comparisons: true,
        kernels: true,
        short_hashes: true,
        scopes: true,
    };
    /// No law; enable fields one at a time.
    pub const NONE: Self = Self {
        cycles: Cycles::Off,
        repeats: false,
        zero_bytes: false,
        copies: false,
        allocations: false,
        comparisons: false,
        kernels: false,
        short_hashes: false,
        scopes: false,
    };
}

pub fn module(m: &Module) -> R {
    module_with_budget(m, &Debit::unlimited()).map_err(|e| e.to_string())
}

/// Verify canonical CoreIR with the metering laws active at admission.
pub fn metering_with_budget(m: &Module, budget: &Debit<'_>) -> Result<(), Error> {
    metering_checks_with_budget(m, budget, Checks::ADMISSION)
}

/// Verify canonical CoreIR, refusing cycles whose bound cannot be proved locally.
pub fn metering_strict_with_budget(m: &Module, budget: &Debit<'_>) -> Result<(), Error> {
    metering_checks_with_budget(m, budget, Checks::STRICT)
}

/// Verify canonical CoreIR with the chosen metering laws.
pub fn metering_checks_with_budget(
    m: &Module,
    budget: &Debit<'_>,
    checks: Checks,
) -> Result<(), Error> {
    structural(m, budget)?;
    check(m, budget, checks)
}

/// Recheck a compiler-transformed body after its canonical module was verified.
pub fn specialized_module(m: &Module) -> R {
    structural(m, &Debit::unlimited()).map_err(|e| e.to_string())
}

fn check(module: &Module, budget: &Debit<'_>, checks: Checks) -> Result<(), Error> {
    let scratch = budget.scratch();
    let mut entries = Vec::new();
    scratch.reserve(&mut entries, module.functions.len())?;
    let mut nodes = 0usize;
    for function in &module.functions {
        scratch.check()?;
        entries.push(nodes);
        if let Some(body) = &function.body {
            nodes = nodes
                .checked_add(body.blocks.len())
                .ok_or(Error::Limit(DecodeLimit::Bytes))?;
        }
    }
    let external = nodes;
    let size = nodes
        .checked_add(1)
        .ok_or(Error::Limit(DecodeLimit::Bytes))?;
    let mut edges = Vec::<Vec<usize>>::new();
    scratch.reserve(&mut edges, size)?;
    edges.resize_with(size, Vec::new);
    let mut incoming = Vec::new();
    scratch.reserve(&mut incoming, size)?;
    incoming.resize(size, 0usize);
    let mut connect = |from: usize, to: usize| -> Result<(), Error> {
        scratch.reserve(&mut edges[from], 1)?;
        edges[from].push(to);
        incoming[to] += 1;
        Ok(())
    };
    let reentrant = checks.cycles == Cycles::Reentrant;
    let mut layouts = repeat::Layouts::new(&scratch);
    for (fi, function) in module.functions.iter().enumerate() {
        let Some(body) = &function.body else { continue };
        let local = scratch.scratch();
        if checks.scopes {
            scopes::check(body, &local)?;
        }
        let facts = bulk::Facts::new(body, &local)?;
        let mut paid =
            repeat::paid_repeats(module, body, &facts, &mut layouts, checks.repeats, &local)?;
        facts.check_operations(body, checks, &paid, &local)?;
        if checks.cycles == Cycles::Off {
            continue;
        }
        if checks.cycles >= Cycles::Local {
            cursor::paid_latches(module, &mut layouts, body, &facts, &mut paid, &local)?;
        }
        let entry = entries[fi];
        connect(external, entry)?;
        'blocks: for (bi, block) in body.blocks.iter().enumerate() {
            scratch.check()?;
            let node = entry + bi;
            for inst in block.insts.get(&body.insts).unwrap_or_default() {
                scratch.check()?;
                if let Some(fail) = inst.fail(body) {
                    connect(node, entry + fail.index())?;
                }
                if let Inst::Call { call } = *inst
                    && facts.positive_work(body, &body.calls[call.index()])
                {
                    continue 'blocks;
                }
                match *inst {
                    Inst::Step { .. } => continue 'blocks,
                    Inst::Call { call } => match body.calls[call.index()].callee {
                        Callee::Direct(f) if module.functions[f.index()].body.is_some() => {
                            connect(node, entries[f.index()])?;
                        }
                        Callee::Direct(_) | Callee::Indirect(_) | Callee::PackExport(_) => {
                            if reentrant {
                                connect(node, external)?;
                            }
                        }
                        Callee::Runtime(
                            RtOp::WorkSteps
                            | RtOp::StepEnter
                            | RtOp::StepLeave
                            | RtOp::StepCatch
                            | RtOp::ZeroBytes
                            | RtOp::CompareBytes
                            | RtOp::Crc32c
                            | RtOp::Crc64Nvme
                            | RtOp::Sha1Blocks
                            | RtOp::Sha1Compress
                            | RtOp::Sha256Blocks
                            | RtOp::Sha512Blocks
                            | RtOp::Xxh3Stripes
                            | RtOp::Xxh3Short,
                        ) => {}
                        Callee::Runtime(_) if reentrant => connect(node, external)?,
                        Callee::Runtime(_) => {}
                    },
                    Inst::Drop { .. } | Inst::Release { .. } if reentrant => {
                        connect(node, external)?
                    }
                    _ => {}
                }
                // The scalar rule rejects a cycle only when every traversed
                // instruction is known to do no metered work. Open calls and
                // storage operations still need the wider proof.
                if checks.cycles == Cycles::Scalar
                    && !matches!(
                        inst,
                        Inst::Const { .. }
                            | Inst::Unary { .. }
                            | Inst::Binary { .. }
                            | Inst::IntArith { .. }
                            | Inst::Compare { .. }
                            | Inst::Convert { .. }
                            | Inst::LoopIteration
                            | Inst::StackCheck { .. }
                            | Inst::BuiltinUse { .. }
                    )
                {
                    continue 'blocks;
                }
            }
            if paid[bi] {
                continue;
            }
            let mut edge =
                |id: EdgeId| connect(node, entry + body.edges[id.index()].target.index());
            match body.terminators[bi] {
                Terminator::Jump(e) => edge(e)?,
                Terminator::Branch { yes, no, .. } => {
                    edge(yes)?;
                    edge(no)?;
                }
                Terminator::Switch {
                    arms, otherwise, ..
                } => {
                    for arm in arms.get(&body.pools.arms).unwrap_or_default() {
                        edge(arm.edge)?;
                    }
                    edge(otherwise)?;
                }
                Terminator::Raise {
                    cleanup: Some(fail),
                    ..
                } => connect(node, entry + fail.index())?,
                _ => {}
            }
        }
    }
    if checks.cycles == Cycles::Off {
        return Ok(());
    }
    let mut pending = Vec::new();
    scratch.reserve(&mut pending, size)?;
    for (i, &n) in incoming.iter().enumerate() {
        scratch.check()?;
        if n == 0 {
            pending.push(i);
        }
    }
    let mut removed = 0;
    while let Some(i) = pending.pop() {
        scratch.check()?;
        removed += 1;
        for &next in &edges[i] {
            scratch.check()?;
            incoming[next] -= 1;
            if incoming[next] == 0 {
                pending.push(next);
            }
        }
    }
    if removed == size {
        return Ok(());
    }
    // Name the first body block left in the graph: it lies on a cycle or
    // downstream of one.
    let left = incoming.iter().position(|&n| n != 0).unwrap_or(external);
    let at = entries
        .iter()
        .rposition(|&e| e <= left && left < external)
        .map_or_else(String::new, |f| format!("f{f} b{}: ", left - entries[f]));
    Err(Error::Invalid(format!(
        "{at}control-flow or call cycle without a metering charge"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{DecodeBudget, DecodeLimits};

    #[test]
    fn graph_storage_is_charged_to_the_callers_debit() {
        let module = crate::text::parse(include_str!("../../samples/control.core")).unwrap();
        let budget = DecodeBudget::new(DecodeLimits {
            bytes: 1,
            members: usize::MAX,
            slots: usize::MAX,
            depth: usize::MAX,
            deadline: None,
        });
        for checks in [Checks::ADMISSION, Checks::STRICT] {
            assert_eq!(
                check(&module, &budget.debit(), checks),
                Err(Error::Limit(DecodeLimit::Bytes))
            );
            check(&module, &Debit::unlimited(), checks).unwrap();
        }
    }
}
