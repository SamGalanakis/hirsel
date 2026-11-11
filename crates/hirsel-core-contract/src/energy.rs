//! A task's energy scopes (storage.md STORE-11 "Energy"): the one meter state
//! machine the native runtime and the interpreter share. The meter itself is a
//! `u64` the caller owns (`TaskCtx.energy` natively), and `u64::MAX` means
//! outside any budget. `energy.enter` opens a scope and `energy.leave` closes
//! the innermost one. An exhausted charge calls [`Scopes::exhaust`].

/// Energy granted once to a scope that ran out, so its cleanup can run
/// (failure.md FAIL-3). It is never renewed: a second exhaustion leaves the
/// meter at zero, so every later charge in the scope fails at once.
pub const RESERVE: u64 = 1_000_000;

/// `FailureKind.OutOfEnergy`'s ordinal (`failure_kind(ENERGY_RULE)`).
pub const OUT_OF_ENERGY: u32 = 5;

/// What one scope spent and whether it ran out (`EnergyUsed`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Used {
    pub units: u64,
    pub exhausted: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct Spend {
    /// The scope ran out and holds its cleanup reserve.
    exhausted: bool,
    /// The reserve ran out too.
    drained: bool,
}

#[derive(Clone, Copy, Debug)]
struct Scope {
    serial: u64,
    /// The enclosing allowance and its state, restored minus what this scope spent.
    outer: u64,
    outer_spend: Spend,
    allowance: u64,
}

/// The open scopes of one task, innermost last.
#[derive(Clone, Debug, Default)]
pub struct Scopes {
    open: Vec<Scope>,
    serial: u64,
    spend: Spend,
}

/// `energy.leave` was given a scope that is not the innermost open one.
#[derive(Debug, Eq, PartialEq)]
pub struct NotInnermost;

impl Scopes {
    /// Opens a scope of `budget` inside the current allowance `meter` and
    /// returns its token. A scope never holds more than its enclosing scope has
    /// left, so what it spends can always be charged to the enclosing scope.
    pub fn enter(&mut self, meter: &mut u64, budget: u64) -> u64 {
        self.serial += 1;
        let allowance = budget.min(*meter);
        self.open.push(Scope {
            serial: self.serial,
            outer: *meter,
            outer_spend: std::mem::take(&mut self.spend),
            allowance,
        });
        *meter = allowance;
        self.serial
    }

    /// Closes the scope `token`, which must be the innermost open one. The
    /// enclosing allowance is restored minus everything the scope spent, its
    /// cleanup reserve included; outside any budget it stays unbounded.
    pub fn leave(&mut self, meter: &mut u64, token: u64) -> Result<Used, NotInnermost> {
        if self.open.last().is_none_or(|s| s.serial != token) {
            return Err(NotInnermost);
        }
        let scope = self.open.pop().unwrap();
        let units = match (self.spend.exhausted, self.spend.drained) {
            (false, _) => scope.allowance - *meter,
            (true, false) => scope.allowance.saturating_add(RESERVE - *meter),
            (true, true) => scope.allowance.saturating_add(RESERVE),
        };
        let exhausted = self.spend.exhausted;
        self.spend = scope.outer_spend;
        *meter = if scope.outer == u64::MAX {
            u64::MAX
        } else {
            scope.outer.saturating_sub(units)
        };
        Ok(Used { units, exhausted })
    }

    /// A charge found the meter short: the scope has run out. The first time,
    /// it is granted the cleanup reserve; after that the meter stays at zero.
    pub fn exhaust(&mut self, meter: &mut u64) {
        if self.spend.exhausted {
            self.spend.drained = true;
            *meter = 0;
        } else {
            self.spend.exhausted = true;
            *meter = RESERVE;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A charge of `n` as compiled code makes it.
    fn charge(scopes: &mut Scopes, meter: &mut u64, n: u64) -> bool {
        if *meter < n {
            scopes.exhaust(meter);
            false
        } else {
            *meter -= n;
            true
        }
    }

    #[test]
    fn a_nested_scope_is_charged_to_its_enclosing_scope() {
        let (mut s, mut m) = (Scopes::default(), u64::MAX);
        let outer = s.enter(&mut m, 1000);
        assert!(charge(&mut s, &mut m, 100));
        // The inner budget is cut to what the outer has left.
        let inner = s.enter(&mut m, 5000);
        assert_eq!(m, 900);
        assert!(charge(&mut s, &mut m, 200));
        assert_eq!(
            s.leave(&mut m, inner),
            Ok(Used {
                units: 200,
                exhausted: false
            })
        );
        assert_eq!(m, 700);
        assert_eq!(
            s.leave(&mut m, outer),
            Ok(Used {
                units: 300,
                exhausted: false
            })
        );
        assert_eq!(m, u64::MAX);
    }

    #[test]
    fn an_exhausted_inner_scope_spends_its_reserve_from_the_outer_and_leaves_it_running() {
        let (mut s, mut m) = (Scopes::default(), u64::MAX);
        let outer = s.enter(&mut m, 2_000_000);
        let inner = s.enter(&mut m, 50);
        assert!(!charge(&mut s, &mut m, 51));
        assert_eq!(m, RESERVE);
        assert!(charge(&mut s, &mut m, 10));
        assert_eq!(
            s.leave(&mut m, inner),
            Ok(Used {
                units: 60,
                exhausted: true
            })
        );
        assert_eq!(m, 2_000_000 - 60);
        // The outer scope did not run out: its next shortfall grants its own reserve.
        assert!(charge(&mut s, &mut m, 1));
        assert_eq!(
            s.leave(&mut m, outer),
            Ok(Used {
                units: 61,
                exhausted: false
            })
        );
    }

    #[test]
    fn a_drained_reserve_stays_at_zero_and_counts_whole() {
        let (mut s, mut m) = (Scopes::default(), u64::MAX);
        let only = s.enter(&mut m, 7);
        assert!(!charge(&mut s, &mut m, 8));
        assert!(!charge(&mut s, &mut m, RESERVE + 1));
        assert_eq!(m, 0);
        assert!(!charge(&mut s, &mut m, 1));
        assert_eq!(m, 0);
        assert_eq!(
            s.leave(&mut m, only),
            Ok(Used {
                units: 7 + RESERVE,
                exhausted: true
            })
        );
        assert_eq!(m, u64::MAX);
    }

    #[test]
    fn only_the_innermost_scope_leaves_and_only_once() {
        let (mut s, mut m) = (Scopes::default(), u64::MAX);
        let outer = s.enter(&mut m, 100);
        let inner = s.enter(&mut m, 10);
        assert_eq!(s.leave(&mut m, outer), Err(NotInnermost));
        assert!(s.leave(&mut m, inner).is_ok());
        assert_eq!(s.leave(&mut m, inner), Err(NotInnermost));
        assert!(s.leave(&mut m, outer).is_ok());
        assert_eq!(s.leave(&mut m, outer), Err(NotInnermost));
    }
}
