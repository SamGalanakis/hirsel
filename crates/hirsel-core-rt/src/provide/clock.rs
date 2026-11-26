use super::*;

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
    {
        let mut clock = lock(clock);
        let before = clock.mono;
        clock.mono = clock.mono.max(mono);
        clock.boot = match boot {
            Some(boot) => clock.boot.max(boot).max(clock.mono),
            None => clock.boot.saturating_add(clock.mono - before),
        };
        let now = clock.mono;
        clock.timers.retain(|&(at, token)| {
            let reached = at <= now;
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
