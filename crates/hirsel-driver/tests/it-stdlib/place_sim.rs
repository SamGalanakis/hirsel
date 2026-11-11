//! The whole-place simulation (STORE-2, STORE-11, STORE-12). One seed picks
//! the schedule, under which tasks pass the turn at every seam request (the
//! `seam` simulation point), the authority's fault and its request, and every
//! caller's ops. The fixture `place_sim/place-sim.hir` prints the history it
//! saw; `check` reads that history alone, in the style of Elle, and shares no
//! code with the store. Interpreter only: the schedule points and the
//! simulated authority's cuts exist only in the simulation.
use hirsel_engine::*;
use hirsel_front_phases::*;
use hirsel_intern::jobs::with_compiler_stack;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// One caller's op: caller and op number.
type Op = (u64, u64);

#[derive(Debug, Default)]
struct Calls {
    /// The shared key each op writes.
    key: BTreeMap<Op, u64>,
    /// The `seq` an acknowledged op committed as.
    acked: BTreeMap<Op, u64>,
    /// Ops some attempt of which has no known outcome.
    unknown: Vec<Op>,
    /// Ops every attempt of which failed with no effect.
    failed: Vec<Op>,
}

/// A fresh read: the rows an open read, and the ops acknowledged before it
/// began.
struct Read {
    at: String,
    rows: BTreeMap<String, String>,
    before: Vec<Op>,
}

fn rows(words: &[&str]) -> Result<BTreeMap<String, String>, String> {
    words
        .iter()
        .map(|w| {
            w.split_once('=')
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .ok_or_else(|| format!("row {w}"))
        })
        .collect()
}

fn num(w: &str) -> Result<u64, String> {
    w.parse().map_err(|_| format!("number {w}"))
}

/// The op and `seq` a value names: `v<C>.<N>@<S>`.
fn value(v: &str) -> Option<(Op, u64)> {
    let (op, seq) = v.strip_prefix('v')?.split_once('@')?;
    let (c, n) = op.split_once('.')?;
    Some(((c.parse().ok()?, n.parse().ok()?), seq.parse().ok()?))
}

/// The calls, the reads, and the final read among them.
fn parse(history: &str) -> Result<(Calls, Vec<Read>, usize), String> {
    let mut calls = Calls::default();
    let mut reads = Vec::new();
    let mut opening: BTreeMap<u64, Vec<Op>> = BTreeMap::new();
    let mut ended = None;
    let mut ok: Vec<Op> = Vec::new();
    for line in history.lines() {
        let w: Vec<&str> = line.split(' ').collect();
        match w.as_slice() {
            ["seed", _] | ["noopen", _, _] | ["nofinal", _] => {}
            ["opening", c, _] => {
                opening.insert(num(c)?, ok.clone());
            }
            ["open", c, n, row @ ..] => reads.push(Read {
                at: format!("caller {c} op {n}"),
                rows: rows(row)?,
                before: opening.remove(&num(c)?).ok_or("open before opening")?,
            }),
            ["inv", c, n, key, _] => {
                calls.key.insert((num(c)?, num(n)?), num(key)?);
            }
            ["ok", c, n, seq] => {
                calls.acked.insert((num(c)?, num(n)?), num(seq)?);
                ok.push((num(c)?, num(n)?));
            }
            ["unk", c, n] => calls.unknown.push((num(c)?, num(n)?)),
            ["fail", c, n] => calls.failed.push((num(c)?, num(n)?)),
            ["final", row @ ..] => {
                ended = Some(reads.len());
                reads.push(Read {
                    at: "the final open".into(),
                    rows: rows(row)?,
                    before: ok.clone(),
                });
            }
            _ => return Err(format!("unparsed event `{line}`")),
        }
    }
    let Some(last) = ended else {
        return Err("no final read".into());
    };
    let acked = &calls.acked;
    let unknown = calls.unknown.clone();
    calls
        .failed
        .retain(|o| !acked.contains_key(o) && !unknown.contains(o));
    calls.unknown.retain(|o| !acked.contains_key(o));
    Ok((calls, reads, last))
}

/// The laws, over the history alone. Every read of a store is checked as a
/// prefix of the applied calls in `seq` order, `final` naming them all.
fn check(history: &str) -> Result<(), String> {
    let (calls, reads, last) = parse(history)?;
    let last = &reads[last].rows;
    // The calls that applied, by `seq`, as the final read's own rows name them.
    let mut applied: BTreeMap<u64, Op> = BTreeMap::new();
    for (k, v) in last {
        if k.starts_with('p') {
            let (op, seq) = value(v).ok_or(format!("final: {k}={v}"))?;
            if format!("p{}.{}", op.0, op.1) != *k {
                return Err(format!("final: {k} holds another op's value {v}"));
            }
            if let Some(other) = applied.insert(seq, op) {
                return Err(format!("calls {other:?} and {op:?} both applied as {seq}"));
            }
        }
    }
    for (op, seq) in &calls.acked {
        // (a) An acknowledged call is never lost.
        if applied.get(seq) != Some(op) {
            return Err(format!(
                "lost write: {op:?} was acknowledged as {seq}, the final state lacks it"
            ));
        }
    }
    for op in &calls.failed {
        // (d) A call that failed with no effect has none.
        if applied.values().any(|o| o == op) {
            return Err(format!("{op:?} failed with no effect, yet it applied"));
        }
    }
    for read in &reads {
        let seq: u64 = read.rows.get("seq").map_or(Ok(0), |s| num(s))?;
        // (b) No stale read: an open sees every call acknowledged before it began.
        for op in &read.before {
            if calls.acked[op] > seq {
                return Err(format!(
                    "stale read at {}: {op:?} was acknowledged as {} before it began, it read {seq}",
                    read.at, calls.acked[op]
                ));
            }
        }
        // All or nothing, and no duplicate effect: the read is exactly the
        // applied calls up to its `seq`, each once.
        let mut want: BTreeMap<String, String> = BTreeMap::new();
        if seq > 0 {
            want.insert("seq".into(), seq.to_string());
        }
        let mut counts: BTreeMap<u64, u64> = BTreeMap::new();
        for s in 1..=seq {
            let Some(&(c, n)) = applied.get(&s) else {
                return Err(format!(
                    "{} read seq {seq}, but no applied call is {s}",
                    read.at
                ));
            };
            let v = format!("v{c}.{n}@{s}");
            want.insert(format!("k{}", calls.key[&(c, n)]), v.clone());
            want.insert(format!("p{c}.{n}"), v);
            *counts.entry(c).or_default() += 1;
        }
        for (c, k) in counts {
            want.insert(format!("n{c}"), k.to_string());
        }
        if read.rows != want {
            return Err(format!(
                "{} is not the applied calls up to {seq}: read {:?}, want {want:?}",
                read.at, read.rows
            ));
        }
    }
    Ok(())
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/place_sim/place-sim.hir")
}

/// The fault a seed puts on the authority: none, or one cut at one request.
fn grant(seed: u64) -> String {
    const CUTS: [&[&str]; 5] = [
        &[],
        &["fail_at"],
        &["lose_response_at"],
        &["tear_at"],
        &["crash_at"],
    ];
    let mut z = seed.wrapping_mul(0x9e3779b97f4a7c15);
    z ^= z >> 29;
    let cut = CUTS[(z % 5) as usize];
    let at = 1 + (z >> 8) % 60;
    let faults: Vec<String> = cut.iter().map(|f| format!(r#""{f}":{at}"#)).collect();
    format!(
        r#"{{"w":{{"provider":"authority.sim@1","floor":"disk","seed":{seed},"represent":"log","levels":["disk"],"faults":{{{}}}}}}}"#,
        faults.join(",")
    )
}

/// A seed's run with `ops` ops a caller. Fewer ops keep the seed's schedule,
/// fault and ops, cut short.
fn simulation(seed: u64, ops: u64) -> SimulationConfig {
    SimulationConfig {
        args: vec![seed.to_string(), ops.to_string()],
        authorities: hirsel_run_config::validate_authorities(grant(seed).as_bytes()).unwrap(),
        scheduler: Some(hirsel_run_schema::Scheduler::parse(&format!("random@1:{seed}")).unwrap()),
        debug: hirsel_driver::testing::SimulationDebug {
            seam_calls: true,
            ..Default::default()
        },
        ..SimulationConfig::default()
    }
}

/// The fixture's ops a caller.
const OPS: u64 = 5;

/// Runs one configuration: its history, or the law it breaks with the history.
fn observe(
    loaded: &mut LoadedProgram,
    config: SimulationConfig,
) -> Result<String, (String, String)> {
    let run = run_observed_snapshot(
        &mut loaded.engine,
        &RunConfiguration {
            entry: loaded.entry.clone(),
            hash_seed: 0,
            inputs: vec![],
        },
        config,
    )
    .unwrap();
    let history = run
        .observation
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.output).into_owned())
        .unwrap_or_default();
    if !matches!(
        run.outcome,
        Ok(RunOutcome::Returned(hirsel_run_schema::Returned::Ok))
    ) {
        return Err((format!("run ended {:?}", run.outcome), history));
    }
    match check(&history) {
        Ok(()) => Ok(history),
        Err(law) => Err((law, history)),
    }
}

/// Runs seeds `seeds` with `ops` ops a caller on one compiled fixture; the
/// first seed whose history breaks a law, with the law and the history. A
/// failing seed is shrunk by prefix: the history reported is that of the
/// fewest ops a caller that still breaks a law.
fn sweep(
    seeds: std::ops::Range<u64>,
    ops: u64,
    config: fn(u64, u64) -> SimulationConfig,
) -> Option<(u64, String, String)> {
    with_compiler_stack(|| {
        let path = fixture();
        let mut loaded = load_program_source(&path, None).unwrap();
        loaded.check_for_execution().unwrap();
        loaded.engine.adapters.interpreter = Some(Box::new(InterpreterAdapter {
            limits: hirsel_interp::Limits {
                steps: Some(40_000_000),
                call_depth: 256,
                source: None,
            },
            ..Default::default()
        }));
        // What the sweep reached: acknowledged calls, unknown outcomes, those
        // later acknowledged on a resend, and failures.
        let mut reached = [0usize; 4];
        for seed in seeds {
            let history = match observe(&mut loaded, config(seed, ops)) {
                Ok(history) => history,
                Err(broken) => {
                    let (law, history) = (1..ops)
                        .find_map(|k| {
                            let (law, history) = observe(&mut loaded, config(seed, k)).err()?;
                            Some((format!("{law} (shrunk to {k} ops a caller)"), history))
                        })
                        .unwrap_or(broken);
                    return Some((seed, law, history));
                }
            };
            let count = |p: &str| history.lines().filter(|l| l.starts_with(p)).count();
            let resolved = parse(&history).map_or(0, |(c, _, _)| {
                history
                    .lines()
                    .filter_map(|l| l.strip_prefix("unk "))
                    .filter(|o| {
                        let mut w = o.split(' ').map(|n| n.parse::<u64>().unwrap());
                        c.acked
                            .contains_key(&(w.next().unwrap(), w.next().unwrap()))
                    })
                    .count()
            });
            for (i, n) in [count("ok "), count("unk "), resolved, count("fail ")]
                .into_iter()
                .enumerate()
            {
                reached[i] += n;
            }
        }
        eprintln!(
            "place_sim reached: {} acked, {} unknown, {} of them acked on a resend, {} failed",
            reached[0], reached[1], reached[2], reached[3]
        );
        None
    })
}

#[test]
fn every_seeded_place_history_keeps_the_laws() {
    // PLACE_SIM_SEED replays one seed.
    let seeds = match std::env::var("PLACE_SIM_SEED") {
        Ok(s) => {
            let s: u64 = s.parse().unwrap();
            s..s + 1
        }
        Err(_) => 0..200,
    };
    if let Some((seed, law, history)) = sweep(seeds, OPS, simulation) {
        panic!("seed {seed}: {law}\n{history}");
    }
}

/// The served scenario: a head serves a store its successor fenced after an
/// acknowledged call, and caller 8 queries it.
fn served(_: u64, _: u64) -> SimulationConfig {
    SimulationConfig {
        args: vec!["served".into()],
        tasks_free: true,
        runners: hirsel_run_config::validate_runners(
            br#"[{"name":"head","provider":"sim@1","node":"place","models":"db@1","head":"application@1/head","grants":{"stdout":"stdout.bytes@1","dirs":[],"authorities":{"place":{"provider":"authority.memory@1","floor":"memory"}}},"definitions":null}]"#,
        )
        .unwrap(),
        ..SimulationConfig::default()
    }
}

/// FOR-SAM 63: a served query reads the head's own state without checking
/// that its claim still holds, so a fenced head serves a read staler than an
/// acknowledged call. Red until place-store-681 checks the tail before serving
/// (its seam land 3).
#[test]
fn a_fenced_head_serves_no_stale_read() {
    if let Some((_, law, history)) = sweep(0..1, 1, served) {
        panic!("{law}\n{history}");
    }
}
