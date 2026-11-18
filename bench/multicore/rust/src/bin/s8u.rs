use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::thread;
use std::time::Instant;

const ITEMS: u64 = 60;
const SPLIT: u64 = 5;
const KNAPSACK_LANES: usize = 1 << SPLIT;
const ROUND_NODES: u64 = 2048;
const ROUND_ITERATIONS: u64 = 16384;
const SEEDS: [u64; 4] = [11, 23, 47, 97];
const TARGET: u64 = 26;

#[derive(Clone, Copy)]
struct Hit {
    value: u64,
    witness: u64,
    at: Instant,
}

struct Receipt {
    hit: Hit,
    work: u64,
    stopped: Instant,
    rounds: u64,
}

fn prefix_weight(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let jitter = (n + 1199).wrapping_mul(11400714819323198485) >> 61;
    100 * n + 17 * n * (n - 1) / 2 + jitter
}

fn weight(i: u64) -> u64 {
    prefix_weight(i + 1) - prefix_weight(i)
}

fn upper(depth: u64, remaining: u64, value: u64) -> u64 {
    let base = prefix_weight(depth);
    let mut low = depth;
    let mut high = ITEMS + 1;
    while low + 1 < high {
        let mid = (low + high) / 2;
        if prefix_weight(mid) - base <= remaining {
            low = mid;
        } else {
            high = mid;
        }
    }
    let used = prefix_weight(low) - base;
    let mut bound = value + used + 300 * (low - depth);
    if low < ITEMS {
        let w = weight(low);
        bound += (remaining - used) * (w + 300) / w;
    }
    bound
}

#[derive(Clone, Copy)]
struct Cursor {
    prefix: u64,
    chosen: u64,
    depth: u64,
    remaining: u64,
    value: u64,
    done: bool,
}

impl Cursor {
    fn new(prefix: u64) -> Self {
        let mut remaining = prefix_weight(ITEMS) / 3 + 7;
        let mut value = 0;
        for i in 0..SPLIT {
            if prefix & (1 << i) != 0 {
                let w = weight(i);
                remaining -= w;
                value += w + 300;
            }
        }
        Self {
            prefix,
            chosen: 0,
            depth: SPLIT,
            remaining,
            value,
            done: false,
        }
    }

    fn visit(&mut self, incumbent: &AtomicU64, hit: &mut Option<Hit>) {
        let mut best = incumbent.load(Relaxed);
        if self.value > best {
            let previous = incumbent.fetch_max(self.value, Relaxed);
            if self.value > previous {
                *hit = Some(Hit {
                    value: self.value,
                    witness: self.prefix | self.chosen,
                    at: Instant::now(),
                });
            }
            best = previous.max(self.value);
        }
        let depth = self.depth;
        if depth < ITEMS && upper(depth, self.remaining, self.value) > best {
            self.depth += 1;
            let w = weight(depth);
            if w <= self.remaining {
                self.chosen |= 1 << depth;
                self.remaining -= w;
                self.value += w + 300;
            }
        } else if self.chosen == 0 {
            self.done = true;
        } else {
            let last = u64::from(63 - self.chosen.leading_zeros());
            self.chosen ^= 1 << last;
            let w = weight(last);
            self.depth = last + 1;
            self.remaining += w;
            self.value -= w + 300;
        }
    }
}

#[derive(Default)]
struct LaneReply {
    work: u64,
    hit: Option<Hit>,
    stopped: Option<Instant>,
}

fn reduce_knapsack(replies: &[LaneReply], rounds: u64) -> Receipt {
    Receipt {
        hit: replies
            .iter()
            .filter_map(|reply| reply.hit)
            .max_by_key(|hit| hit.value)
            .unwrap(),
        work: replies.iter().map(|reply| reply.work).sum(),
        stopped: replies
            .iter()
            .filter_map(|reply| reply.stopped)
            .max()
            .unwrap(),
        rounds,
    }
}

fn knapsack(pool: &rayon::ThreadPool, emulate: bool) -> Receipt {
    let best = AtomicU64::new(0);
    let mut lanes: [_; KNAPSACK_LANES] =
        std::array::from_fn(|i| Cursor::new((KNAPSACK_LANES - 1 - i) as u64));
    let mut replies: [_; KNAPSACK_LANES] = std::array::from_fn(|_| LaneReply::default());

    if !emulate {
        pool.install(|| {
            lanes
                .par_iter()
                .copied()
                .zip(&mut replies)
                .for_each(|(lane, reply)| {
                    let mut lane = lane;
                    let mut local = LaneReply::default();
                    while !lane.done {
                        lane.visit(&best, &mut local.hit);
                        local.work += 1;
                    }
                    local.stopped = Some(Instant::now());
                    *reply = local;
                });
        });
        return reduce_knapsack(&replies, 0);
    }

    // The region model retains private cursors, one bounded request per live lane,
    // a reply slot per lane, and the owner round barrier. Publications inside the
    // round go straight to the atomic instead of waiting for the owner.
    let mut rounds = 0;
    while lanes.iter().any(|lane| !lane.done) {
        let mut slots: [_; KNAPSACK_LANES] = std::array::from_fn(|_| LaneReply::default());
        pool.install(|| {
            lanes
                .par_iter_mut()
                .zip(&mut slots)
                .for_each(|(lane, reply)| {
                    let mut cursor = *lane;
                    if cursor.done {
                        return;
                    }
                    let mut local = LaneReply::default();
                    while local.work < ROUND_NODES && !cursor.done {
                        cursor.visit(&best, &mut local.hit);
                        local.work += 1;
                    }
                    if cursor.done {
                        local.stopped = Some(Instant::now());
                    }
                    *lane = cursor;
                    *reply = local;
                });
        });
        for (total, reply) in replies.iter_mut().zip(slots) {
            total.work += reply.work;
            total.hit = reply.hit.or(total.hit);
            total.stopped = reply.stopped.or(total.stopped);
        }
        rounds += 1;
    }
    reduce_knapsack(&replies, rounds)
}

const UNARY: u64 = random(0x94d049bb133111eb);
const EDGES: u64 = random(UNARY);

const fn random(state: u64) -> u64 {
    let x = state ^ (state << 13);
    let y = x ^ (x >> 7);
    y ^ (y << 17)
}

fn energy(bits: u64) -> u64 {
    let sites = (bits ^ UNARY).count_ones();
    let breaks = (((bits ^ (bits >> 1)) ^ EDGES) & (u64::MAX >> 1)).count_ones();
    u64::from(sites + 2 * breaks)
}

struct Search {
    random_state: u64,
    bits: u64,
    cost: u64,
    iterations: u64,
    hit: Option<Hit>,
}

impl Search {
    fn new(seed: u64) -> Self {
        Self {
            random_state: seed,
            bits: 0,
            cost: 0,
            iterations: 0,
            hit: None,
        }
    }

    fn run(&mut self, stop: &AtomicBool, budget: u64) {
        let Self {
            mut random_state,
            mut bits,
            mut cost,
            mut iterations,
            mut hit,
        } = *self;
        for _ in 0..budget {
            if stop.load(Relaxed) {
                break;
            }
            random_state = random(random_state);
            let restart = iterations & 127 == 0;
            let candidate = if restart {
                random_state
            } else {
                bits ^ (1 << ((random_state >> 32) & 63))
            };
            let next_cost = energy(candidate);
            if restart || next_cost <= cost || (next_cost == cost + 1 && random_state & 15 == 0) {
                bits = candidate;
                cost = next_cost;
            }
            iterations += 1;
            if cost == TARGET {
                if !stop.swap(true, Relaxed) {
                    hit = Some(Hit {
                        value: TARGET,
                        witness: bits,
                        at: Instant::now(),
                    });
                }
                break;
            }
        }
        *self = Self {
            random_state,
            bits,
            cost,
            iterations,
            hit,
        };
    }
}

fn portfolio_receipt(lanes: &[Search], stopped: Instant, rounds: u64) -> Receipt {
    Receipt {
        hit: lanes.iter().find_map(|lane| lane.hit).unwrap(),
        work: lanes.iter().map(|lane| lane.iterations).sum(),
        stopped,
        rounds,
    }
}

fn portfolio_threads(width: usize) -> Receipt {
    let stop = AtomicBool::new(false);
    let mut lanes = SEEDS.map(Search::new);
    let stopped = thread::scope(|scope| {
        if width == 1 {
            scope
                .spawn(|| {
                    while !stop.load(Relaxed) {
                        for lane in &mut lanes {
                            lane.run(&stop, ROUND_ITERATIONS);
                        }
                    }
                    Instant::now()
                })
                .join()
                .unwrap()
        } else {
            let joins = lanes.each_mut().map(|lane| {
                scope.spawn(|| {
                    lane.run(&stop, u64::MAX);
                    Instant::now()
                })
            });
            joins
                .map(|join| join.join().unwrap())
                .into_iter()
                .max()
                .unwrap()
        }
    });
    portfolio_receipt(&lanes, stopped, 0)
}

fn portfolio_region(pool: &rayon::ThreadPool) -> Receipt {
    let stop = AtomicBool::new(false);
    let mut lanes = SEEDS.map(Search::new);
    let mut stopped = [None; SEEDS.len()];
    let mut rounds = 0;
    while !stop.load(Relaxed) {
        pool.install(|| {
            lanes
                .par_iter_mut()
                .zip(&mut stopped)
                .for_each(|(lane, finished)| {
                    lane.run(&stop, ROUND_ITERATIONS);
                    *finished = Some(Instant::now());
                });
        });
        rounds += 1;
    }
    portfolio_receipt(&lanes, stopped.into_iter().flatten().max().unwrap(), rounds)
}

fn main() {
    let [_, case, side, width]: [String; 4] =
        std::env::args().collect::<Vec<_>>().try_into().unwrap();
    let width = width.parse().unwrap();
    assert!(matches!(width, 1 | 16));
    let start = Instant::now();
    let receipt = match (case.as_str(), side.as_str()) {
        ("portfolio", "rust") => portfolio_threads(width),
        ("knapsack", "rust" | "region") | ("portfolio", "region") => {
            rayon::ThreadPoolBuilder::new()
                .num_threads(width)
                .build_scoped(
                    |worker| worker.run(),
                    |pool| {
                        if case == "knapsack" {
                            knapsack(pool, side == "region")
                        } else {
                            portfolio_region(pool)
                        }
                    },
                )
                .unwrap()
        }
        _ => panic!("expected knapsack or portfolio and rust or region"),
    };
    let elapsed = start.elapsed().as_nanos();
    let lag = receipt
        .stopped
        .saturating_duration_since(receipt.hit.at)
        .as_nanos();
    println!(
        "{elapsed} {} {} {} {lag} {lag} {}",
        receipt.work, receipt.hit.value, receipt.hit.witness, receipt.rounds
    );
}
