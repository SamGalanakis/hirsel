//! `flow_pool <stages> <items> <rounds>`: one owner pushes `items` keys
//! through a chain of `stages` stages, each mixing its item `rounds` times,
//! and pulls them from the sink. Prints the wall time and a checksum.
use super::*;

fn mix(mut x: u64, rounds: u64) -> u64 {
    for _ in 0..rounds {
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
        x ^= x >> 29;
    }
    x
}

fn pipeline(stages: usize, items: u64, rounds: u64) -> u64 {
    let context = crate::Context::new(1 << 30);
    let charge = || StageCharge {
        output: 8,
        working: 0,
        state: 0,
        item_steps: u64::MAX >> 2,
    };
    let mut flow = Reference::<u64>::default();
    let sink = flow.sink().unwrap();
    let mut to = sink;
    for _ in 0..stages {
        to = flow
            .stage_with_size(&context, to, 64, charge(), 0, || {
                move |stage: &mut crate::Context, _, x: &u64| {
                    // Native loops charge a step per round. Debit the same
                    // steps while keeping the Rust mixing loop unchanged.
                    if !unsafe { crate::steps::take_many(stage, rounds, 0) } {
                        return Err(Halt::OutOfTime);
                    }
                    Ok(Some(mix(*x, rounds)))
                }
            })
            .unwrap();
    }
    let port = flow.port_with_size(&context, to, 64, 8, 0).unwrap();
    let mut sum = 0u64;
    let mut pull = |flow: &mut Reference<u64>| loop {
        match flow.next(sink).unwrap() {
            Pull::Item(_, _, x) => sum = sum.wrapping_add(x),
            Pull::Stalled | Pull::End => return,
            Pull::Dropped(..) => unreachable!("no stage fails"),
        }
    };
    for key in 1..=items {
        let mut value = key;
        loop {
            match flow.push(port, key, value) {
                Ok(()) => break,
                Err(PushError::Stalled(back)) => {
                    value = back;
                    pull(&mut flow);
                }
                Err(_) => unreachable!("an open port in key order"),
            }
        }
    }
    flow.end(port).unwrap();
    pull(&mut flow);
    flow.finish().unwrap();
    sum
}

/// Runs once on the calling thread, or as the root unit of a
/// `HIRSEL_UNTIED`-worker pool.
#[doc(hidden)]
pub fn pool_pipeline(pool: bool, stages: usize, items: u64, rounds: u64) -> u8 {
    let once = || {
        let start = std::time::Instant::now();
        let sum = pipeline(stages, items, rounds);
        let ns = start.elapsed().as_nanos();
        let workers = crate::units::running().map_or(0, |_| crate::units::pool().w);
        eprintln!(
            "flow_pool workers={workers} stages={stages} items={items} rounds={rounds} ns={ns} sum={sum}"
        );
        0
    };
    if !pool {
        return once();
    }
    let w = crate::units::workers().expect("set HIRSEL_UNTIED to the worker count");
    // SAFETY: lengths, masks, descriptors and nullable pointers; the root
    // reads none of them.
    let config: crate::entry::OsConfig = unsafe { std::mem::zeroed() };
    crate::units::launch(1, w, &config, &|_| once())
}
