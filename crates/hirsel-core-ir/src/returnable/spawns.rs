//! The spawns of a task entry adapter that calls a stored function.

use super::*;

/// A spawn of the task entry adapter `entry` whose stored callee is `callee`,
/// found in the body of `function` at call `call`.
pub(crate) struct Spawn {
    pub(super) function: usize,
    pub(super) reference: usize,
    pub(crate) entry: FuncId,
    pub(crate) callee: FuncId,
    pub(super) indirect: CallId,
    /// The callee's code value has no use but the store into the input.
    pub(crate) sole: bool,
}

/// The spawns whose entry adapter calls a stored function: the spawning body
/// stores `fnref C` into the input's field 0 and passes `fnref E`, and `E`
/// makes one indirect call of the code it reads from its input's field 0.
pub(crate) fn spawns(module: &Module) -> Vec<Spawn> {
    let mut out = vec![];
    for (f, function) in module.functions.iter().enumerate() {
        let Some(body) = &function.body else { continue };
        let refs: HashMap<ValueId, (usize, FuncId)> = body
            .insts
            .iter()
            .enumerate()
            .filter_map(|(k, i)| match *i {
                Inst::FunctionRef { result, function } => Some((result, (k, function))),
                _ => None,
            })
            .collect();
        let mut uses = HashMap::<ValueId, usize>::new();
        for inst in &body.insts {
            for u in operands(body, inst).0 {
                if u < body.values.len() {
                    *uses.entry(ValueId(u as u32)).or_default() += 1;
                }
            }
        }
        for bi in 0..body.blocks.len() {
            for u in terminator_uses(body, bi) {
                if u < body.values.len() {
                    *uses.entry(ValueId(u as u32)).or_default() += 1;
                }
            }
        }
        for inst in &body.insts {
            let Inst::Call { call } = *inst else { continue };
            let c = &body.calls[call.index()];
            if !matches!(
                c.callee,
                Callee::Runtime(RtOp::Spawn { .. } | RtOp::SpawnUnit { .. })
            ) {
                continue;
            }
            let args = c.args.get(&body.pools.args).unwrap_or_default();
            let (Some(Arg::Address(input)), Some(Arg::Value(code))) = (args.get(1), args.get(2))
            else {
                continue;
            };
            let Some(&(reference, entry)) = refs.get(code) else {
                continue;
            };
            if uses.get(code) != Some(&1) {
                continue;
            }
            // The callee stored into field 0 of the input.
            let stored = body.insts.iter().find_map(|i| match *i {
                Inst::Store { address, value } => {
                    let info = &body.addresses[address.index()];
                    let steps = info.steps.get(&body.pools.steps).unwrap_or_default();
                    (info.base == AddressBase::Address(*input) && steps == [Step::Field(0)])
                        .then_some(value)
                }
                _ => None,
            });
            let Some((callee, sole)) =
                stored.and_then(|v| refs.get(&v).map(|r| (r.1, uses.get(&v) == Some(&1))))
            else {
                continue;
            };
            let Some(entry_body) = &module.functions[entry.index()].body else {
                continue;
            };
            let indirect: Vec<CallId> = entry_body
                .insts
                .iter()
                .filter_map(|i| match *i {
                    Inst::Call { call }
                        if matches!(entry_body.calls[call.index()].callee, Callee::Indirect(_)) =>
                    {
                        Some(call)
                    }
                    _ => None,
                })
                .collect();
            let [indirect] = indirect[..] else { continue };
            if entry_body.calls[indirect.index()].signature
                != module.functions[callee.index()].signature
                && module.signatures[entry_body.calls[indirect.index()].signature.index()]
                    != module.signatures[module.functions[callee.index()].signature.index()]
            {
                continue;
            }
            out.push(Spawn {
                function: f,
                reference,
                entry,
                callee,
                indirect,
                sole,
            });
        }
    }
    out
}
