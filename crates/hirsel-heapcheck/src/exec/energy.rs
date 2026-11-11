//! STORE-11 "Energy": each task's meter and scopes, run on the one state
//! machine the runtime and the interpreter share
//! (`hirsel_core_contract::energy`). A charged unit's `Inst::Charge` takes its
//! units from the running task's allowance and takes its failure edge when
//! fewer remain; `energy.enter` and `energy.leave` open and close a scope.
//! Outside any scope the allowance is unbounded, so uncharged runs are as
//! before.
use super::*;
use hirsel_core_contract::energy::Scopes;

#[derive(Clone, Default)]
pub(super) struct Meters(BTreeMap<u32, (u64, Scopes)>);

impl Meters {
    fn task(&mut self, task: u32) -> &mut (u64, Scopes) {
        self.0
            .entry(task)
            .or_insert_with(|| (u64::MAX, Scopes::default()))
    }
}

/// The energy operation `id` names, if it is one.
pub(super) fn operation(id: &hirsel_core_contract::OperationId) -> Option<&'static str> {
    let path = &id.definition.0;
    (path.package.name_is("core") && path.modules == ["energy"])
        .then(|| ["enter", "leave"].into_iter().find(|n| *n == path.name))
        .flatten()
}

/// The integers from `from` to `to` of kind `scalar`, as `Energy::Span`
/// counts them: zero when empty, at most `u64::MAX`.
fn span(scalar: ScalarKind, from: u128, to: u128, inclusive: bool) -> Checked<u64> {
    let (bits, signed) = match scalar {
        ScalarKind::U8 => (8, false),
        ScalarKind::U16 => (16, false),
        ScalarKind::U32 => (32, false),
        ScalarKind::U64 => (64, false),
        ScalarKind::U128 => (128, false),
        ScalarKind::I8 => (8, true),
        ScalarKind::I16 => (16, true),
        ScalarKind::I32 => (32, true),
        ScalarKind::I64 => (64, true),
        ScalarKind::I128 => (128, true),
        _ => return fail(V::TypeConfusion, "a charge span over a non-integer"),
    };
    let mask = if bits == 128 {
        u128::MAX
    } else {
        (1u128 << bits) - 1
    };
    let (from, to) = (from & mask, to & mask);
    let wide = |v: u128| -> i128 {
        if signed && bits < 128 && v >> (bits - 1) & 1 == 1 {
            (v | !mask) as i128
        } else {
            v as i128
        }
    };
    let nonempty = match (signed, inclusive) {
        (true, true) => wide(from) <= wide(to),
        (true, false) => wide(from) < wide(to),
        (false, true) => from <= to,
        (false, false) => from < to,
    };
    if !nonempty {
        return Ok(0);
    }
    // Where the range is nonempty, `to - from` modulo 2^bits is its exact
    // unsigned distance.
    let distance = to.wrapping_sub(from) & mask;
    Ok(u64::try_from(distance.saturating_add(u128::from(inclusive))).unwrap_or(u64::MAX))
}

impl<S: Stage> Machine<'_, S> {
    fn running_task(&self) -> u32 {
        self.tasks.as_deref().map_or(0, |t| t.current)
    }

    /// Whether the running task is inside a budget (an allowance other than
    /// unbounded): drop glue then runs a finalizer's charged twin.
    pub(super) fn energy_budgeted(&mut self) -> bool {
        let task = self.running_task();
        self.energy.task(task).0 != u64::MAX
    }

    /// One `Inst::Charge`: false when the allowance ran short, so the charge
    /// takes its failure edge. The first shortfall in a scope grants it the
    /// cleanup reserve.
    pub(super) fn energy_charge(
        &mut self,
        f: &Frame,
        energy: hirsel_lir::ir::Energy,
    ) -> Checked<bool> {
        use hirsel_lir::ir::Energy;
        let units = match energy {
            Energy::Iteration | Energy::Entry => 1,
            Energy::Span {
                from,
                to,
                inclusive,
            } => {
                let (
                    Val::Bits { scalar, bits: l },
                    Val::Bits {
                        scalar: other,
                        bits: r,
                    },
                ) = (self.read_use(f, from)?, self.read_use(f, to)?)
                else {
                    return fail(V::TypeConfusion, "a charge span over non-scalars");
                };
                if scalar != other {
                    return fail(V::TypeConfusion, "a charge span over two integer kinds");
                }
                span(scalar, l, r, inclusive)?
            }
        };
        let task = self.running_task();
        let (meter, scopes) = self.energy.task(task);
        if *meter < units {
            scopes.exhaust(meter);
            Ok(false)
        } else {
            *meter -= units;
            Ok(true)
        }
    }

    /// `energy.enter(budget)` and `energy.leave(take saved)`.
    pub(super) fn energy_operation(&mut self, name: &str, args: &[OpValue]) -> Checked<OpOutcome> {
        let task = self.running_task();
        let (meter, scopes) = self.energy.task(task);
        Ok(match (name, args) {
            (
                "enter",
                [
                    OpValue::Bits {
                        scalar: ScalarKind::U64,
                        bits,
                    },
                ],
            ) => OpOutcome::Value(OpValue::Handle(scopes.enter(meter, *bits as u64))),
            ("leave", [OpValue::Handle(token)]) => {
                let used = scopes.leave(meter, *token).map_err(|_| {
                    err(
                        V::Malformed,
                        "energy.leave of a scope that is not the innermost open one",
                    )
                })?;
                OpOutcome::Value(OpValue::Aggregate(vec![
                    OpValue::Bits {
                        scalar: ScalarKind::U64,
                        bits: used.units.into(),
                    },
                    OpValue::Bits {
                        scalar: ScalarKind::Bool,
                        bits: used.exhausted.into(),
                    },
                ]))
            }
            _ => return fail(V::TypeConfusion, format!("energy.{name} operands {args:?}")),
        })
    }
}
