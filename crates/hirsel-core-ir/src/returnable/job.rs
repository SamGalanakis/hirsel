//! The job a transfer moves, and whether its root reads it after a park.

use super::{ALIGN, Set, has, params, place_layout};
use crate::extent::operands;
use crate::{
    AddressBase, Arg, Body, Callee, FuncId, Inst, Loan, Module, Param, PlaceTy, Terminator, TypeId,
    ValueId,
};
use std::collections::{BTreeMap, BTreeSet};

/// True when nothing that can point into the root's job, its entry parameters
/// other than the result place `dest`, is live after any site (`live`) or
/// taken by a site's own call. A resume then reads nothing through the job, so
/// the runtime may free the task input the job was moved into at the first
/// park (`hirsel_core_transfer`).
///
/// Names that can point into the job: those parameters, block parameters fed
/// one, addresses based on one, pointers made from such an address, and the
/// results of any other instruction or call that takes one. A value read out
/// of the job is a copy and points into nothing of it. A pointer the body
/// could keep where liveness does not see it, stored to memory or passed as a
/// value, makes the fact false.
pub(super) fn job_dead(body: &Body, dest: Option<ValueId>, live: &BTreeMap<usize, Set>) -> bool {
    let nv = body.values.len();
    let mut derived = vec![false; nv + body.addresses.len()];
    for p in params(body, 0) {
        if Some(*p) != dest {
            derived[p.index()] = true;
        }
    }
    loop {
        let mut changed = false;
        let mut mark = |derived: &mut Vec<bool>, n: usize| {
            if !derived[n] {
                derived[n] = true;
                changed = true;
            }
        };
        for inst in &body.insts {
            match *inst {
                Inst::Address { address, .. } => {
                    let base = match body.addresses[address.index()].base {
                        AddressBase::Value(v) => Some(v.index()),
                        AddressBase::Address(a) => Some(nv + a.index()),
                        AddressBase::Slot(_) => None,
                    };
                    if base.is_some_and(|b| derived[b]) {
                        mark(&mut derived, nv + address.index());
                    }
                }
                Inst::Read { .. } | Inst::Case { .. } => {}
                Inst::Store { value, .. } if derived[value.index()] => return false,
                Inst::Call { call } => {
                    let site = &body.calls[call.index()];
                    if let Callee::Indirect(v) = site.callee
                        && derived[v.index()]
                    {
                        return false;
                    }
                    let args = site.args.get(&body.pools.args).unwrap_or_default();
                    if args
                        .iter()
                        .any(|a| matches!(*a, Arg::Value(v) if derived[v.index()]))
                    {
                        return false;
                    }
                    if args
                        .iter()
                        .any(|a| matches!(*a, Arg::Address(x) if derived[nv + x.index()]))
                    {
                        for r in site.results.get(&body.pools.values).unwrap_or_default() {
                            mark(&mut derived, r.index());
                        }
                    }
                }
                _ => {
                    let (uses, defs) = operands(body, inst);
                    if uses.iter().any(|&u| derived[u]) {
                        for d in defs {
                            mark(&mut derived, d);
                        }
                    }
                }
            }
        }
        for terminator in &body.terminators {
            let edges = match terminator {
                Terminator::Jump(e) => vec![*e],
                Terminator::Branch { yes, no, .. } => vec![*yes, *no],
                Terminator::Switch {
                    arms, otherwise, ..
                } => arms
                    .get(&body.pools.arms)
                    .unwrap_or_default()
                    .iter()
                    .map(|a| a.edge)
                    .chain([*otherwise])
                    .collect(),
                _ => vec![],
            };
            for e in edges {
                let edge = &body.edges[e.index()];
                let args = edge.args.get(&body.pools.values).unwrap_or_default();
                for (a, p) in args.iter().zip(params(body, edge.target.index())) {
                    if derived[a.index()] {
                        mark(&mut derived, p.index());
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    live.iter().all(|(&k, set)| {
        let Inst::Call { call } = body.insts[k] else {
            unreachable!("a site is a call")
        };
        let site = &body.calls[call.index()];
        let taken = site
            .args
            .get(&body.pools.args)
            .unwrap_or_default()
            .iter()
            .any(|a| match *a {
                Arg::Value(v) => derived[v.index()],
                Arg::Address(x) => derived[nv + x.index()],
            });
        !taken && (0..derived.len()).all(|n| !derived[n] || !has(set, n))
    })
}

/// The job a transfer moves, when `g` can be a task's root on its own: it
/// takes one declared job and returns through its result place, and the job
/// holds nothing that borrows the frames the transfer gives back.
pub(super) fn transfer_job(
    module: &Module,
    recipes: &[hirsel_core_layout::RecipeRow],
    g: FuncId,
) -> Option<(TypeId, u64)> {
    let sig = &module.signatures[module.functions[g.index()].signature.index()];
    let [
        Param::Address {
            place: PlaceTy::Declared(job),
            loan: Loan::Take,
        },
    ] = sig.params[..]
    else {
        return None;
    };
    if sig.never || sig.dest.is_none() || !sig.results.is_empty() {
        return None;
    }
    let (size, align) = place_layout(module, recipes, PlaceTy::Declared(job))?;
    if align > ALIGN {
        return None;
    }
    let mut seen = BTreeSet::new();
    let mut rows = vec![job.index()];
    let mut fields: Vec<&hirsel_core_layout::FieldTy> = vec![];
    while let Some(t) = rows.pop() {
        if !seen.insert(t) {
            continue;
        }
        let shape = &module.types[t];
        if matches!(
            shape.repr,
            hirsel_core_layout::Repr::Scoped | hirsel_core_layout::Repr::Region
        ) {
            return None;
        }
        match &shape.body {
            hirsel_core_layout::Body::Record(fs) => fields.extend(fs.iter().map(|f| &f.ty)),
            hirsel_core_layout::Body::Variant(cases) => {
                fields.extend(cases.iter().flat_map(|c| c.fields.iter().map(|f| &f.ty)))
            }
        }
        fields.extend(shape.held.as_ref());
        while let Some(field) = fields.pop() {
            use hirsel_core_layout::FieldTy as F;
            match field {
                F::Borrow | F::View(_) | F::Locator(_) | F::RegionRef(_) => return None,
                F::Inline(x) | F::Box(x) | F::Rc(x) | F::Arc(x) | F::Shared(x) | F::Link(x) => {
                    rows.push(x.0 as usize)
                }
                F::Seq(e)
                | F::Array(e, _)
                | F::FixedSpan(e)
                | F::SeqSpan(e)
                | F::IndexedSpan(e) => fields.push(e),
                F::Map(k, v) => {
                    fields.push(k);
                    fields.push(v);
                }
                F::Scalar(_) | F::Fn | F::OwnFn | F::Handle(_) | F::Text | F::FrozenBytes => {}
            }
        }
    }
    Some((job, size))
}
