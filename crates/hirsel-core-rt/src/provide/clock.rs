use super::*;
use crate::tasks::channel_clock;

/// Remove the timer a completed I/O operation registered under its token.
/// # Safety
/// The provider is live and belongs to a Clock capability.
pub(crate) unsafe fn retire_deadline(provider: &Provider, token: u64) {
    if owns(provider) {
        // SAFETY: the provided Clock pins its cell.
        let Cell::Clock(clock) = (unsafe { cell(provider.state) }) else {
            unreachable!("a Clock designates a clock cell");
        };
        lock(clock).timers.retain(|&(_, _, t)| t != token);
    }
}

/// The clock and deadline from the served call's adapter (NOTES 35).
/// # Safety
/// `request` is a pinned call on a port of this vocabulary.
pub(super) unsafe fn call_until(vocab: Vocab, request: &Request) -> Option<(Provider, u64, u64)> {
    // SAFETY: the adapters pin the request type selected by the operation.
    unsafe {
        match (vocab, request.operation) {
            (Vocab::Exec, EXEC_WAIT) => (*request.path.cast::<Call<ProcCall>>()).data.until,
            (Vocab::Net, op) if unix_operation(op) => (*request.path.cast::<UnixRequest>()).until,
            (Vocab::Net, op) if udp_operation(op) => crate::udp_net::served::until(request),
            (Vocab::Net, _) | (Vocab::Exec, RECV_BOUNDED | SEND_PAIR) => {
                (*request.path.cast::<crate::net::NetworkRequest>()).until
            }
            _ => None,
        }
    }
}

#[derive(Default)]
pub(super) struct ClockCell {
    pub(super) mono: u64,
    pub(super) boot: u64,
    pub(super) wall: u64,
    /// Waits on this clock's deadlines in registration order: a token stays
    /// until its deadline is reached; a stale one is pruned at quiescence.
    pub(super) timers: Vec<(u64, u64, u64)>,
    pub(super) channel_timers: channel_clock::Arms,
    pub(super) registration: u64,
    /// Waits on this clock's wall time, as `timers`; `set_wall` readies them.
    pub(super) wall_timers: Vec<(u64, u64)>,
    /// `clock.idle` waits: the token and whether quiescence has woken it.
    pub(super) idle: Vec<(u64, bool)>,
}

impl ClockCell {
    pub(super) fn try_next_registration(&mut self) -> Result<u64, crate::run_stop::Stop> {
        self.registration = self
            .registration
            .checked_add(1)
            .ok_or_else(|| crate::run_stop::Stop::from_static(b"TimerArms"))?;
        Ok(self.registration)
    }

    pub(super) fn next_registration(&mut self) -> u64 {
        self.try_next_registration()
            .unwrap_or_else(|stop| crate::run_stop::raise(stop))
    }

    pub(super) fn next_deadline(&self) -> Option<u64> {
        self.timers
            .iter()
            .map(|&(at, _, _)| at)
            .chain(self.channel_timers.first().map(|(at, _)| at))
            .min()
    }
}

/// A provided clock pinned by a live run's capability registry.
#[derive(Clone, Copy)]
pub(crate) struct ChannelClock(*const Mutex<ClockCell>);

impl ChannelClock {
    /// # Safety
    /// The provider is live and belongs to a Clock capability.
    pub(crate) unsafe fn of(provider: &Provider) -> Option<Self> {
        if !owns(provider) {
            return None;
        }
        // SAFETY: a provided Clock pins the cell through its run.
        match unsafe { cell(provider.state) } {
            Cell::Clock(clock) => Some(Self(clock)),
            _ => unreachable!("a Clock designates a clock cell"),
        }
    }

    /// # Safety
    /// The waiting task owns this arm and the run pins this clock.
    pub(crate) unsafe fn arm(self, arm: channel_clock::Link, at: u64, observed: bool) -> bool {
        // SAFETY: this task pins the clock and its own timer slot.
        unsafe {
            let mut clock = lock(&*self.0);
            if at <= clock.mono {
                if observed {
                    // The recorded observation was before this expiry.
                    // Arrange a wake, then make the next poll observe it.
                    let sequence = match clock.try_next_registration() {
                        Ok(seq) => seq,
                        Err(stop) => {
                            drop(clock);
                            crate::run_stop::raise(stop);
                        }
                    };
                    clock.channel_timers.insert(arm, sequence);
                    clock.channel_timers.fire_first();
                }
                return observed;
            }
            let sequence = match clock.try_next_registration() {
                Ok(seq) => seq,
                Err(stop) => {
                    drop(clock);
                    crate::run_stop::raise(stop);
                }
            };
            clock.channel_timers.insert(arm, sequence);
            true
        }
    }

    /// # Safety
    /// The running task still owns the arm and pins this clock.
    pub(crate) unsafe fn retire(self, arm: channel_clock::Link) {
        // SAFETY: the clock lock serializes retirement with expiry.
        unsafe { lock(&*self.0).channel_timers.retire(arm) }
    }

    #[cfg(test)]
    pub(crate) fn first(self) -> Option<(u64, u64)> {
        // SAFETY: tests inspect the clock before ending the run.
        unsafe { lock(&*self.0).channel_timers.first() }
    }
}

/// `clock.advance_to(mut h, mono_ns)`: time never moves back. Every wait whose
/// deadline is now reached is readied, earliest deadline first, ties in
/// registration order.
/// # Safety
/// Context is running and `hand` is a live ClockHand.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_clock_advance_to(
    context: *mut Context,
    hand: u64,
    mono: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe { advance(context, hand, mono, None) }
}

/// Advances both elapsed readings before readying any monotonic timer.
/// # Safety
/// Context is running and `hand` is a live ClockHand.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_clock_advance_to_with_boot(
    context: *mut Context,
    hand: u64,
    mono: u64,
    boot: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe { advance(context, hand, mono, Some(boot)) }
}

unsafe fn advance(context: *mut Context, hand: u64, mono: u64, boot: Option<u64>) -> Outcome {
    // SAFETY: forwarded.
    let Cell::Clock(clock) = (unsafe { held(context, hand) }) else {
        unreachable!("a ClockHand designates a clock cell")
    };
    let mut due = Vec::new();
    let mut clock = lock(clock);
    let before = clock.mono;
    clock.mono = clock.mono.max(mono);
    clock.boot = match boot {
        Some(boot) => clock.boot.max(boot).max(clock.mono),
        None => clock.boot.saturating_add(clock.mono - before),
    };
    let now = clock.mono;
    clock.timers.retain(|&(at, sequence, token)| {
        if at <= now {
            due.push((at, sequence, token));
            false
        } else {
            true
        }
    });
    due.sort_unstable();
    for (at, sequence, token) in due {
        while clock
            .channel_timers
            .first()
            .is_some_and(|arm| arm < (at, sequence))
        {
            // SAFETY: the clock mutex pins the nonempty arm list until publication.
            unsafe { clock.channel_timers.fire_first() };
        }
        crate::tasks::hirsel_core_ready(token);
    }
    while clock
        .channel_timers
        .first()
        .is_some_and(|(at, _)| at <= now)
    {
        // SAFETY: the clock mutex pins the nonempty arm list until publication.
        unsafe { clock.channel_timers.fire_first() };
    }
    Outcome::default()
}

/// `clock.set_wall(mut h, wall_ms)`: the wall clock may move either way. Every
/// wall wait now reached is readied, earliest reading first, ties in
/// registration order.
/// # Safety
/// Context is running and `hand` is a live ClockHand.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_clock_set_wall(
    context: *mut Context,
    hand: u64,
    wall: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    let Cell::Clock(clock) = (unsafe { held(context, hand) }) else {
        unreachable!("a ClockHand designates a clock cell")
    };
    let mut due = Vec::new();
    {
        let mut clock = lock(clock);
        clock.wall = wall;
        clock.wall_timers.retain(|&(at, token)| {
            let reached = at <= wall;
            if reached {
                due.push((at, token));
            }
            !reached
        });
    }
    due.sort_by_key(|&(at, _)| at);
    for (_, token) in due {
        crate::tasks::hirsel_core_ready(token);
    }
    Outcome::default()
}
