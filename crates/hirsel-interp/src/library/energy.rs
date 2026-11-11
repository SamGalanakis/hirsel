//! A store's energy scopes on each task's meter (storage.md STORE-11 "Energy"),
//! the reference's twin of the native `TaskCtx.energy`: one counted charge is
//! one unit here, and the count is the engine's own (only "ran out" is part of
//! an outcome).
use super::Library;
use super::dispatch::{arg, int, u64_value};
use crate::{Error, Value, invalid};
use hirsel_core_contract::energy::Scopes;

/// Per task: its meter (`u64::MAX` outside any budget) and open scopes.
#[derive(Default)]
pub(super) struct Meters(std::collections::BTreeMap<u64, (u64, Scopes)>);

impl Meters {
    pub(super) fn metered(&self, task: u64) -> bool {
        self.0
            .get(&task)
            .is_some_and(|(meter, _)| *meter != u64::MAX)
    }

    fn task(&mut self, task: u64) -> &mut (u64, Scopes) {
        self.0.entry(task).or_insert((u64::MAX, Scopes::default()))
    }
}

impl Library {
    pub(super) fn energy_enter(&mut self, a: &[Value]) -> Result<Value, Error> {
        let budget = int(arg(a, 0)?)?.bits() as u64;
        let (meter, scopes) = self.energy.task(self.tasks.current);
        Ok(Value::Handle {
            kind: "Energy".into(),
            id: scopes.enter(meter, budget),
        })
    }

    pub(super) fn energy_leave(&mut self, a: &[Value]) -> Result<Value, Error> {
        let Value::Handle { kind, id } = arg(a, 0)? else {
            return Err(invalid("invalid Energy representation"));
        };
        if kind != "Energy" {
            return Err(invalid("invalid Energy representation"));
        }
        let (meter, scopes) = self.energy.task(self.tasks.current);
        let used = scopes
            .leave(meter, *id)
            .map_err(|_| invalid("energy.leave of a scope that is not the innermost open one"))?;
        Ok(Value::Record {
            definition: (&hirsel_syntax::DefPath::core("EnergyUsed")).into(),
            fields: vec![
                Some(u64_value(used.units)),
                Some(Value::Bool(used.exhausted)),
            ],
        })
    }

    /// One counted charge; false when the running scope has run out.
    pub(super) fn energy_charge(&mut self) -> bool {
        let (meter, scopes) = self.energy.task(self.tasks.current);
        if *meter == 0 {
            scopes.exhaust(meter);
            return false;
        }
        if *meter != u64::MAX {
            *meter -= 1;
        }
        true
    }
}
