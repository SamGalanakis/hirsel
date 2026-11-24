use super::*;

// ---------------------------------------------------------------------------------------------
// The checks the IR alone answers.

pub(super) fn check_body(
    f: &FnInfo,
    ir: &Module,
    body: &IrBody,
    sig: &hirsel_core_ir::Signature,
    stats: &mut JoinStats,
) -> R<()> {
    // Parameter modes: the syntax mode must be the IR loan.
    if sig.params.len() != f.params.len() {
        bail!(
            "{}: {} syntax parameters, {} IR parameters",
            f.name,
            f.params.len(),
            sig.params.len()
        );
    }
    for (p, q) in f.params.iter().zip(&sig.params) {
        let loan = match q {
            IrParam::Value(_) => Mode::View,
            IrParam::Address {
                loan: Loan::Mut, ..
            } => Mode::Mut,
            IrParam::Address {
                loan: Loan::View, ..
            } => Mode::View,
            IrParam::Address {
                loan: Loan::Take, ..
            } => Mode::Take,
            #[allow(unreachable_patterns)]
            IrParam::Address { loan, .. } => {
                bail!("{}: parameter {} has loan {loan:?}", f.name, p.name)
            }
        };
        if loan != p.mode {
            bail!(
                "{}: parameter {} is {:?} in syntax, {:?} in the IR",
                f.name,
                p.name,
                p.mode,
                loan
            );
        }
        stats.params_checked += 1;
    }
    exclusions(&f.name, ir, body, stats);
    Ok(())
}

/// The instruction kinds an extracted body may contain. Each is either pure data movement or
/// control the Lean translation carries (`Call`, `Case`, `Step`); a kind not listed here (an
/// allocation the body manages itself, a region, a limit scope, a share or release, SIMD, ..)
/// is refused, so a new effectful kind cannot slip through.
fn allowed(inst: &Inst) -> bool {
    matches!(
        inst,
        Inst::Const { .. }
            | Inst::Unary { .. }
            | Inst::Binary { .. }
            | Inst::Compare { .. }
            | Inst::Convert { .. }
            | Inst::IntArith { .. }
            | Inst::Address { .. }
            | Inst::Read { .. }
            | Inst::Store { .. }
            | Inst::StorageLive { .. }
            | Inst::StorageDead { .. }
            | Inst::Call { .. }
            | Inst::Copy { .. }
            | Inst::Case { .. }
            | Inst::SetCase { .. }
            | Inst::Drop { .. }
            | Inst::Step { .. }
            | Inst::Share { .. }
            | Inst::BuiltinUse { .. }
            | Inst::Pointer { .. }
            | Inst::LayoutConst { .. }
    )
}

/// Failing instruction kinds the Lean model accounts for, and how (REPORT.md "can_fail census").
pub(super) fn fail_modelled(inst: &Inst, body: &IrBody) -> bool {
    match inst {
        // A call fails as its callee does: an extracted callee is translated with its causes, a
        // prelude store/codec call is total by S4, a builtin is translated by its checked form.
        Inst::Call { call } => !matches!(body.calls[call.index()].callee, Callee::Indirect(_)),
        // The reverse join demands a matching arithmetic or index expression.
        Inst::Binary { .. } | Inst::Address { .. } => true,
        // Freeing memory and charging steps fail only on resource limits, which the model omits.
        Inst::Drop { .. } | Inst::Step { .. } => true,
        _ => false,
    }
}

pub(super) fn kind_name(inst: &Inst) -> String {
    format!("{inst:?}")
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or("")
        .to_owned()
}

/// The exclusion checks: indirect calls, runtime operations with host effects (cancellation
/// checks among them), drops reaching a finalizer or a resource, instruction kinds outside
/// `allowed`, and failing instructions the model has no account of.
pub(super) fn exclusions(name: &str, ir: &Module, body: &IrBody, stats: &mut JoinStats) {
    for (i, inst) in body.insts.iter().enumerate() {
        let at = body.inst_anchors[i]
            .map(|a| format!("{}..{}", a.start, a.end))
            .unwrap_or("-".into());
        let kind = kind_name(inst);
        let entry = stats.kinds.entry(kind.clone()).or_default();
        entry.0 += 1;
        if inst.can_fail(body) {
            entry.1 += 1;
            if !fail_modelled(inst, body) {
                stats.refused.push(format!(
                    "{name}: {kind} can fail and the model has no account of it at {at}"
                ));
            }
        }
        if !allowed(inst) {
            stats
                .refused
                .push(format!("{name}: instruction kind {kind} at {at}"));
        }
        match inst {
            Inst::Call { call } => match &body.calls[call.index()].callee {
                Callee::Indirect(_) => stats.refused.push(format!("{name}: indirect call at {at}")),
                Callee::Runtime(op) if !pure_runtime(op) => stats
                    .refused
                    .push(format!("{name}: runtime operation {op:?} at {at}")),
                _ => {}
            },
            Inst::Drop { ty, .. } => {
                stats.drops_checked += 1;
                let mut stack = vec![ty.index()];
                let mut seen = BTreeSet::new();
                while let Some(t) = stack.pop() {
                    if !seen.insert(t) {
                        continue;
                    }
                    let shape = &ir.types[t];
                    // Finalizer-free: no reached row is a resource or names a finalizer.
                    if shape.repr == hirsel_core_layout::Repr::Resource || shape.finalizer.is_some()
                    {
                        stats.refused.push(format!(
                            "{name}: drops {} ({:?}, finalizer {:?}) at {at}",
                            shape.name, shape.repr, shape.finalizer
                        ));
                    }
                    let mut copy = (**shape).clone();
                    copy.map_refs(&mut |r| {
                        stack.push(r.index());
                        r
                    });
                }
            }
            _ => {}
        }
    }
}

/// Give every effect its exact syntax anchor and require a Lean rule at that anchor.
pub(super) fn verify_joins(fx: &mut Fx<'_, '_>) {
    let body = fx.join.body;
    let mut spans = BTreeSet::new();
    fx.s.walk(fx.f.root, &mut |n| {
        spans.insert(fx.s.span(n));
    });
    let name = &fx.f.name;
    for (inst, anchor) in body.insts.iter().zip(&body.inst_anchors) {
        let span = anchor.map(|a| (a.start, a.end));
        let effects = match inst {
            Inst::BuiltinUse { .. } | Inst::Call { .. } | Inst::Share { .. } => Some(&fx.joined_ir),
            Inst::Binary { .. } | Inst::Address { .. } if inst.can_fail(body) => {
                Some(&fx.joined_failures)
            }
            _ => None,
        };
        if span.is_some_and(|s| !spans.contains(&s))
            || effects.is_some_and(|joined| !span.is_some_and(|s| joined.contains(&s)))
        {
            let what = fx.join.direct_path(inst).unwrap_or_else(|| kind_name(inst));
            fx.stats
                .unresolved_ir
                .push(format!("{name}: {what} at {span:?}"));
        }
    }
    for (block, terminator) in body.blocks.iter().zip(&body.terminators) {
        if let hirsel_core_ir::Terminator::Raise { cause, .. } = terminator {
            let span = block.anchor.map(|a| (a.start, a.end));
            let joined = match cause {
                hirsel_core_ir::RaiseCause::Bounds => &fx.joined_failures,
                // Allocation refusals are outside the successful-run projection.
                hirsel_core_ir::RaiseCause::Capacity { .. } => &fx.joined_ir,
                _ => {
                    fx.stats
                        .refused
                        .push(format!("{name}: unsupported raise {cause:?} at {span:?}"));
                    continue;
                }
            };
            if !span.is_some_and(|s| joined.contains(&s)) {
                fx.stats
                    .unresolved_ir
                    .push(format!("{name}: raise {cause:?} at {span:?}"));
            }
        }
    }
}
