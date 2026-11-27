//! The containment generator's independent work instruments, O3b and O3c
//! (`docs/internal/containment-generator.md`, "O3: work ≤ K·steps"). They are
//! on only when `HIRSEL_TEST_WORK_TALLY` names a file, and append JSON lines to it.
//!
//! - O3b: per step bound, when it closes, the steps it debited (its children's
//!   included), the bytes the bulk primitives touched where they did the work
//!   (`Copy`, `Move` and `compare_bytes`, the MEM-30 copy row's units) and the
//!   bytes map hashing read (a named MEM-30 gap), and the instructions executed.
//!   These count work where it is done, never the units passed to the work
//!   helper, so glue that never calls the helper is still seen. The quanta and the
//!   rule live in the driver (`core/tools/containment_fuzz.py`).
//! - O3c: once per run, every step-free cycle of the lowered module: a block
//!   cycle in one body, or a call-graph cycle, that passes no step point. A step
//!   point is an `Inst::Step` (a source call, loop latch or depth step) or a
//!   `work_steps` call. A call-graph edge counts when the callee is reachable
//!   from the caller's entry along a path with no step point. MEM-30 charges a
//!   row's work before any of it, so a step-free block cycle is reported as
//!   prepaid when its body takes work steps outside the cycle, or when every
//!   direct call of the body follows a `work_steps` call in its block (the
//!   derived `==` glue's element loop is charged this way by its caller). A body
//!   reached only through a function value is not seen as called.
use hirsel_core_ir::*;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;

struct Bound {
    id: u64,
    n: u64,
    instructions: u64,
    bytes: u64,
    hashed: u64,
}

thread_local! {
    static OPEN: RefCell<Vec<Bound>> = const { RefCell::new(Vec::new()) };
}

fn output() -> Option<&'static PathBuf> {
    static OUT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    OUT.get_or_init(|| std::env::var_os("HIRSEL_TEST_WORK_TALLY").map(PathBuf::from))
        .as_ref()
}

pub(crate) fn enabled() -> bool {
    output().is_some()
}

fn emit(line: &str) {
    let path = output().expect("the work tally is on");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("the work tally file opens");
    writeln!(file, "{line}").expect("the work tally file takes a line");
}

/// A bound of `n` opened as account `id`; `instructions` is the run's
/// remaining instruction budget.
pub(crate) fn open(id: u64, n: u64, instructions: u64) {
    OPEN.with_borrow_mut(|open| {
        open.push(Bound {
            id,
            n,
            instructions,
            bytes: 0,
            hashed: 0,
        })
    });
}

/// Bytes a bulk primitive moved or compared, charged to the innermost bound.
pub(crate) fn bytes(n: u64) {
    if !enabled() {
        return;
    }
    OPEN.with_borrow_mut(|open| {
        if let Some(b) = open.last_mut() {
            b.bytes = b.bytes.saturating_add(n);
        }
    });
}

/// Bytes map hashing read (a named MEM-30 gap), charged to the innermost bound.
pub(crate) fn hashed(n: u64) {
    if !enabled() {
        return;
    }
    OPEN.with_borrow_mut(|open| {
        if let Some(b) = open.last_mut() {
            b.hashed = b.hashed.saturating_add(n);
        }
    });
}

/// Account `id` closed with `left` of its steps unused.
pub(crate) fn close(id: u64, left: u64, instructions: u64) {
    let closed = OPEN.with_borrow_mut(|open| {
        let at = open.iter().rposition(|b| b.id == id)?;
        let b = open.remove(at);
        if let Some(parent) = at.checked_sub(1).and_then(|p| open.get_mut(p)) {
            parent.bytes = parent.bytes.saturating_add(b.bytes);
            parent.hashed = parent.hashed.saturating_add(b.hashed);
        }
        Some(b)
    });
    if let Some(b) = closed {
        emit(&format!(
            r#"{{"bound":{},"spent":{},"bytes":{},"hashed":{},"instructions":{}}}"#,
            b.n,
            b.n.saturating_sub(left),
            b.bytes,
            b.hashed,
            b.instructions.saturating_sub(instructions),
        ));
    }
}

fn is_step_point(b: &Body, inst: &Inst) -> bool {
    match *inst {
        Inst::Step { .. } => true,
        Inst::Call { call } => matches!(
            b.calls[call.index()].callee,
            Callee::Runtime(RtOp::WorkSteps)
        ),
        _ => false,
    }
}

fn insts(b: &Body, block: usize) -> &[Inst] {
    b.blocks[block].insts.get(&b.insts).unwrap_or_default()
}

fn successors(b: &Body, block: usize) -> Vec<usize> {
    let mut next: Vec<usize> = insts(b, block)
        .iter()
        .filter_map(|i| i.fail(b))
        .map(|t| t.index())
        .collect();
    let edge = |e: &EdgeId| b.edges[e.index()].target.index();
    match &b.terminators[block] {
        Terminator::Jump(e) => next.push(edge(e)),
        Terminator::Branch { yes, no, .. } => next.extend([edge(yes), edge(no)]),
        Terminator::Switch {
            arms, otherwise, ..
        } => {
            next.extend(
                arms.get(&b.pools.arms)
                    .unwrap_or_default()
                    .iter()
                    .map(|a| edge(&a.edge)),
            );
            next.push(edge(otherwise));
        }
        Terminator::Raise { cleanup, .. } => next.extend(cleanup.iter().map(|c| c.index())),
        Terminator::Return(_) | Terminator::Propagate | Terminator::Unreachable => {}
    }
    next
}

/// Tarjan's strongly connected components of the graph `next` over `0..n`,
/// keeping those that hold a cycle (more than one node, or a self edge).
fn cycles(n: usize, next: &dyn Fn(usize) -> Vec<usize>) -> Vec<Vec<usize>> {
    struct State {
        index: Vec<Option<usize>>,
        low: Vec<usize>,
        on: Vec<bool>,
        stack: Vec<usize>,
        counter: usize,
        found: Vec<Vec<usize>>,
    }
    let mut s = State {
        index: vec![None; n],
        low: vec![0; n],
        on: vec![false; n],
        stack: Vec::new(),
        counter: 0,
        found: Vec::new(),
    };
    // Iterative, so a large body cannot exhaust the evaluator's stack.
    for root in 0..n {
        if s.index[root].is_some() {
            continue;
        }
        let mut work: Vec<(usize, Vec<usize>, usize)> = vec![(root, next(root), 0)];
        s.index[root] = Some(s.counter);
        s.low[root] = s.counter;
        s.counter += 1;
        s.stack.push(root);
        s.on[root] = true;
        while let Some((v, succ, k)) = work.last_mut() {
            let v = *v;
            if let Some(&w) = succ.get(*k) {
                *k += 1;
                match s.index[w] {
                    None => {
                        s.index[w] = Some(s.counter);
                        s.low[w] = s.counter;
                        s.counter += 1;
                        s.stack.push(w);
                        s.on[w] = true;
                        work.push((w, next(w), 0));
                    }
                    Some(i) if s.on[w] => s.low[v] = s.low[v].min(i),
                    Some(_) => {}
                }
                continue;
            }
            work.pop();
            if let Some((parent, _, _)) = work.last() {
                s.low[*parent] = s.low[*parent].min(s.low[v]);
            }
            if Some(s.low[v]) == s.index[v] {
                let mut component = Vec::new();
                loop {
                    let w = s.stack.pop().unwrap();
                    s.on[w] = false;
                    component.push(w);
                    if w == v {
                        break;
                    }
                }
                if component.len() > 1 || next(v).contains(&v) {
                    s.found.push(component);
                }
            }
        }
    }
    s.found
}

/// The direct callees a body reaches from its entry along a path with no step point.
fn unstepped_callees(b: &Body) -> BTreeSet<usize> {
    let mut callees = BTreeSet::new();
    let mut clean = vec![false; b.blocks.len()];
    let mut todo = Vec::new();
    let mut reach = |k: usize, todo: &mut Vec<usize>| {
        if !std::mem::replace(&mut clean[k], true) {
            todo.push(k);
        }
    };
    if !b.blocks.is_empty() {
        reach(0, &mut todo);
    }
    while let Some(k) = todo.pop() {
        let mut stepped = false;
        for inst in insts(b, k) {
            if is_step_point(b, inst) {
                stepped = true;
                break;
            }
            if let Inst::Call { call } = *inst
                && let Callee::Direct(f) = b.calls[call.index()].callee
            {
                callees.insert(f.index());
            }
            if let Some(t) = inst.fail(b) {
                reach(t.index(), &mut todo);
            }
        }
        if !stepped {
            for j in successors(b, k) {
                reach(j, &mut todo);
            }
        }
    }
    callees
}

/// Whether a `work_steps` call comes before instruction `at` of a block, with no
/// other direct call between them.
fn charged_before(b: &Body, block: usize, at: usize) -> bool {
    for inst in insts(b, block)[..at].iter().rev() {
        if let Inst::Call { call } = *inst {
            match b.calls[call.index()].callee {
                Callee::Runtime(RtOp::WorkSteps) => return true,
                Callee::Direct(_) => return false,
                _ => {}
            }
        }
    }
    false
}

/// For each function, whether it has a direct call and every one is charged first.
fn charged_by_callers(m: &Module) -> Vec<bool> {
    let mut sites = vec![(0usize, true); m.functions.len()];
    for b in m.functions.iter().filter_map(|f| f.body.as_ref()) {
        for block in 0..b.blocks.len() {
            for (at, inst) in insts(b, block).iter().enumerate() {
                if let Inst::Call { call } = *inst
                    && let Callee::Direct(g) = b.calls[call.index()].callee
                {
                    let site = &mut sites[g.index()];
                    site.0 += 1;
                    site.1 &= charged_before(b, block, at);
                }
            }
        }
    }
    sites.into_iter().map(|(n, all)| n > 0 && all).collect()
}

fn cycle(function: &str, kind: &str, prepaid: bool, blocks: usize) -> String {
    // `{:?}` quotes and escapes a Rust string; a DefPath holds no characters
    // where that differs from JSON's escaping.
    format!(r#"{{"function":{function:?},"kind":"{kind}","prepaid":{prepaid},"blocks":{blocks}}}"#)
}

/// O3c: one line naming every step-free cycle of the module, written as a run
/// starts when the tally is on. Bounds a stopped run left open are dropped here.
pub(crate) fn module(m: &Module) {
    if !enabled() {
        return;
    }
    OPEN.with_borrow_mut(Vec::clear);
    let name = |f: usize| {
        let key = &m.instances[m.functions[f].instance.index()];
        key.definition.to_string()
    };
    let mut found = Vec::new();
    let callers_pay = charged_by_callers(m);
    for (f, function) in m.functions.iter().enumerate() {
        let Some(b) = &function.body else { continue };
        let stepping: Vec<bool> = (0..b.blocks.len())
            .map(|k| insts(b, k).iter().any(|i| is_step_point(b, i)))
            .collect();
        let free = |k: usize| -> Vec<usize> {
            if stepping[k] {
                return Vec::new();
            }
            successors(b, k)
                .into_iter()
                .filter(|&j| !stepping[j])
                .collect()
        };
        for component in cycles(b.blocks.len(), &free) {
            let inside: BTreeSet<usize> = component.iter().copied().collect();
            let prepaid = callers_pay[f]
                || (0..b.blocks.len()).any(|k| {
                    !inside.contains(&k)
                        && insts(b, k).iter().any(|i| {
                            matches!(*i, Inst::Call { call }
                            if b.calls[call.index()].callee == Callee::Runtime(RtOp::WorkSteps))
                        })
                });
            found.push(cycle(&name(f), "loop", prepaid, component.len()));
        }
    }
    let calls: Vec<BTreeSet<usize>> = m
        .functions
        .iter()
        .map(|f| f.body.as_ref().map(unstepped_callees).unwrap_or_default())
        .collect();
    let edges = |f: usize| -> Vec<usize> { calls[f].iter().copied().collect() };
    for component in cycles(m.functions.len(), &edges) {
        let mut names: Vec<String> = component.iter().map(|&f| name(f)).collect();
        names.sort();
        found.push(cycle(&names.join(" "), "recursion", false, component.len()));
    }
    emit(&format!(
        r#"{{"cycles":[{}],"functions":{}}}"#,
        found.join(","),
        m.functions.len()
    ));
}
