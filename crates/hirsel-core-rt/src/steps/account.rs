//! A lexical account stays on the task stack until a child needs its identity.
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone, Copy)]
pub(crate) struct Balance {
    pub(super) left: u64,
    pub(super) exhausted: bool,
}

impl Balance {
    pub(super) fn new(left: u64) -> Self {
        Self {
            left,
            exhausted: false,
        }
    }

    pub(super) fn available(&self) -> u64 {
        if self.exhausted { 0 } else { self.left }
    }
}

pub(crate) struct Account {
    id: u64,
    parent: Option<Arc<Account>>,
    state: Mutex<State>,
}

pub(super) struct State {
    pub(super) balance: Balance,
    pub(super) interest: u32,
}

impl Account {
    pub(super) fn new(id: u64, balance: Balance, parent: Option<Arc<Account>>) -> Arc<Self> {
        Arc::new(Self {
            id,
            parent,
            state: Mutex::new(State {
                balance,
                interest: 1,
            }),
        })
    }

    pub(super) fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn participate(&self) {
        self.state().interest += 1;
    }
}

pub(crate) enum Bound {
    Inline { id: u64, balance: Balance },
    Shared(Arc<Account>),
}

/// The first scope fits in the task itself. A task that nests scopes keeps
/// its spill vector for later calls.
#[derive(Default)]
pub(crate) enum Bounds {
    #[default]
    Empty,
    One(Bound),
    Nested(Vec<Bound>),
}

impl std::ops::Deref for Bounds {
    type Target = [Bound];

    fn deref(&self) -> &[Bound] {
        match self {
            Self::Empty => &[],
            Self::One(bound) => std::slice::from_ref(bound),
            Self::Nested(bounds) => bounds,
        }
    }
}

impl std::ops::DerefMut for Bounds {
    fn deref_mut(&mut self) -> &mut [Bound] {
        match self {
            Self::Empty => &mut [],
            Self::One(bound) => std::slice::from_mut(bound),
            Self::Nested(bounds) => bounds,
        }
    }
}

impl Bounds {
    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::One(_) => false,
            Self::Nested(bounds) => bounds.is_empty(),
        }
    }

    pub(super) fn push(&mut self, bound: Bound) {
        match self {
            Self::Empty => *self = Self::One(bound),
            Self::Nested(bounds) => bounds.push(bound),
            Self::One(_) => {
                let first = self.pop().unwrap();
                *self = Self::Nested(vec![first, bound]);
            }
        }
    }

    pub(super) fn pop(&mut self) -> Option<Bound> {
        if let Self::Nested(bounds) = self {
            return bounds.pop();
        }
        match std::mem::take(self) {
            Self::One(bound) => Some(bound),
            _ => None,
        }
    }
}

impl Bound {
    pub(super) fn id(&self) -> u64 {
        match self {
            Self::Inline { id, .. } => *id,
            Self::Shared(account) => account.id,
        }
    }

    pub(super) fn available(&self) -> u64 {
        match self {
            Self::Inline { balance, .. } => balance.available(),
            Self::Shared(account) => account.state().balance.available(),
        }
    }

    pub(super) fn with_balance<R>(&mut self, f: impl FnOnce(&mut Balance) -> R) -> R {
        match self {
            Self::Inline { balance, .. } => f(balance),
            Self::Shared(account) => f(&mut account.state().balance),
        }
    }

    pub(super) fn close(self, window: u64, parent: Option<&mut Self>) {
        match self {
            Self::Inline { balance, .. } => {
                if let Some(parent) = parent {
                    parent.with_balance(|b| b.left += balance.left + window);
                }
            }
            Self::Shared(account) => {
                account.state().balance.left += window;
                release(account);
            }
        }
    }
}

/// A child can outlive every lexical scope on this stack. Promote the inline
/// suffix from its funder outwards, giving each new edge one interest. A
/// running parent's prepaid window remains with that fiber until it suspends.
pub(super) fn promote(stack: &mut [Bound]) -> Option<Arc<Account>> {
    let start = stack.iter().rposition(|b| matches!(b, Bound::Shared(_)));
    let mut parent = start.map(|i| match &stack[i] {
        Bound::Shared(account) => account.clone(),
        Bound::Inline { .. } => unreachable!(),
    });
    for bound in &mut stack[start.map_or(0, |i| i + 1)..] {
        let Bound::Inline { id, balance } = *bound else {
            unreachable!()
        };
        if let Some(funder) = &parent {
            funder.participate();
        }
        let account = Account::new(id, balance, parent);
        *bound = Bound::Shared(account.clone());
        parent = Some(account);
    }
    parent
}

/// Drop one interest; the last participant returns the residual once per edge.
pub(super) fn release(mut account: Arc<Account>) {
    loop {
        let left = {
            let mut state = account.state();
            state.interest -= 1;
            if state.interest != 0 {
                return;
            }
            state.balance.left
        };
        let Some(parent) = account.parent.clone() else {
            return;
        };
        parent.state().balance.left += left;
        account = parent;
    }
}
