//! A task's energy scopes (storage.md STORE-11 "Energy"): the one meter state
//! machine the native runtime and the interpreter share. The meter itself is a
//! `u64` the caller owns (`TaskCtx.energy` natively), and `u64::MAX` means
//! outside any budget. `energy.enter` opens a scope and `energy.leave` closes
//! the innermost one. An exhausted charge calls [`Scopes::exhaust`].
//!
//! The open scopes form one chain. What any of them spends is spent by every
//! scope around it, and when one runs out they all have: the chain is granted
//! one cleanup reserve, which nothing inside it renews.

/// Energy granted once to a chain of scopes that ran out, so its cleanup can
/// run (failure.md FAIL-3). It is never renewed: a second exhaustion, in the
/// scope that ran out or in any scope opened inside it, leaves the meter at
/// zero, so every later charge in the chain fails at once.
pub const RESERVE: u64 = 1_000_000;

/// `FailureKind.OutOfEnergy`'s ordinal (`failure_kind(ENERGY_RULE)`).
pub const OUT_OF_ENERGY: u32 = 5;

/// What one scope spent and whether it ran out (`EnergyUsed`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Used {
    pub units: u64,
    pub exhausted: bool,
}

#[derive(Clone, Copy, Debug)]
struct Scope {
    serial: u64,
    /// The chain's spending when the scope opened.
    start: u64,
    /// The chain's spending at which the scope's allowance is gone.
    limit: u64,
    /// The scope opened after the chain ran out, inside its reserve.
    in_reserve: bool,
}

/// The open scopes of one task, innermost last.
#[derive(Clone, Debug, Default)]
pub struct Scopes {
    open: Vec<Scope>,
    serial: u64,
    /// Units the chain has spent, as of the last call. Between calls only
    /// compiled charges move the meter, and only down.
    spent: u64,
    /// The meter as of the last call.
    last: u64,
    /// Once the chain has run out: the spending at which its reserve is gone.
    reserve: Option<u64>,
}

/// `energy.leave` was given a scope that is not the innermost open one.
#[derive(Debug, Eq, PartialEq)]
pub struct NotInnermost;

impl Scopes {
    /// Opens a scope of `budget` inside the current allowance `meter` and
    /// returns its token. A scope never holds more than its enclosing scope has
    /// left, and a scope opened after the chain ran out holds only what is
    /// left of the reserve and is out of energy itself.
    pub fn enter(&mut self, meter: &mut u64, budget: u64) -> u64 {
        self.sync(*meter);
        let left = if self.open.is_empty() {
            self.spent = 0;
            self.reserve = None;
            u64::MAX
        } else {
            self.bound() - self.spent
        };
        // `u64::MAX` is outside any budget, which no open scope's meter is.
        let allowance = budget.min(left).min(u64::MAX - 1);
        self.serial += 1;
        self.open.push(Scope {
            serial: self.serial,
            start: self.spent,
            limit: self.spent + allowance,
            in_reserve: self.reserve.is_some(),
        });
        self.set(meter);
        self.serial
    }

    /// Closes the scope `token`, which must be the innermost open one, and
    /// reports what it and the scopes inside it spent, reserve included, and
    /// whether the chain ran out while it was open or before.
    pub fn leave(&mut self, meter: &mut u64, token: u64) -> Result<Used, NotInnermost> {
        if self.open.last().is_none_or(|s| s.serial != token) {
            return Err(NotInnermost);
        }
        self.sync(*meter);
        let scope = self.open.pop().unwrap();
        let used = Used {
            units: self.spent - scope.start,
            exhausted: self.reserve.is_some(),
        };
        if self.open.is_empty() {
            *meter = u64::MAX;
        } else {
            self.set(meter);
        }
        Ok(used)
    }

    /// A charge found the meter short: the chain has run out and spent all it
    /// had. The first time it is granted the reserve; after that the meter
    /// stays at zero.
    pub fn exhaust(&mut self, meter: &mut u64) {
        if self.open.is_empty() {
            return;
        }
        self.spent = self.bound();
        self.reserve = Some(match self.reserve {
            None => self.spent.saturating_add(RESERVE),
            Some(_) => self.spent,
        });
        self.set(meter);
    }

    /// The spending at which the innermost scope's allowance is gone: its
    /// own limit, every enclosing limit being as large, until the chain runs
    /// out; then the reserve's, or a smaller scope opened inside the reserve.
    fn bound(&self) -> u64 {
        let innermost = self.open.last().expect("an open scope");
        match self.reserve {
            None => innermost.limit,
            Some(reserve) if innermost.in_reserve => innermost.limit.min(reserve),
            Some(reserve) => reserve,
        }
    }

    fn sync(&mut self, meter: u64) {
        if !self.open.is_empty() {
            self.spent += self.last.saturating_sub(meter);
        }
    }

    fn set(&mut self, meter: &mut u64) {
        *meter = self.bound() - self.spent;
        self.last = *meter;
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
    fn an_inner_scope_that_runs_out_runs_out_its_enclosing_scope() {
        // Energy attack 753 finding 5: outer 10, inner 10, a charge of 11
        // fails, cleanup spends 20 of the reserve, both scopes leave.
        let (mut s, mut m) = (Scopes::default(), u64::MAX);
        let outer = s.enter(&mut m, 10);
        let inner = s.enter(&mut m, 10);
        assert!(!charge(&mut s, &mut m, 11));
        assert!(charge(&mut s, &mut m, 20));
        let ran_out = Used {
            units: 30,
            exhausted: true,
        };
        assert_eq!(s.leave(&mut m, inner), Ok(ran_out));
        // The outer scope holds what is left of the one reserve, not its own.
        assert_eq!(m, RESERVE - 20);
        assert_eq!(s.leave(&mut m, outer), Ok(ran_out));
        assert_eq!(m, u64::MAX);
    }

    #[test]
    fn no_scope_inside_a_chain_that_ran_out_renews_its_reserve() {
        let (mut s, mut m) = (Scopes::default(), u64::MAX);
        let outer = s.enter(&mut m, 2_000_000);
        let inner = s.enter(&mut m, 50);
        assert!(!charge(&mut s, &mut m, 51));
        assert!(s.leave(&mut m, inner).unwrap().exhausted);
        // Back in the outer scope, a second shortfall drains the reserve
        // the inner one was granted: the meter stays at zero.
        assert!(!charge(&mut s, &mut m, RESERVE + 1));
        assert_eq!(m, 0);
        // A scope opened now holds nothing, and its shortfall grants none.
        let late = s.enter(&mut m, 100);
        assert_eq!(m, 0);
        assert!(!charge(&mut s, &mut m, 1));
        assert_eq!(m, 0);
        assert!(s.leave(&mut m, late).unwrap().exhausted);
        assert_eq!(
            s.leave(&mut m, outer),
            Ok(Used {
                units: 50 + RESERVE,
                exhausted: true
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
