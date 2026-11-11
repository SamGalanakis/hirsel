//! A charged instance (STORE-11 energy) passes C exactly when its unit with
//! the charges removed passes as its source body does, with every charge the
//! charging rule requires where it puts them and calls only to charged
//! instances; no other unit may carry a charge.
use crate::rc_support::{lower_valid, manifest, path, target, unit_named};
use hirsel_layout::operation::OperationRegistry;
use hirsel_lir::ir::*;
use hirsel_lir::operation::OperationDeclarations;
use hirsel_lir::stage::Value;

const SOURCE: &str = r#"fn inner(x: u64) -> u64 { x + 1u64 }
fn other(x: u64) -> u64 { x + 2u64 }
pub fn outer(x: u64) -> u64 { inner(x) * 2u64 + other(x) }
pub fn apply(f: fn(u64) -> u64, x: u64) -> u64 { f(x) }
pub fn sum(n: u64) -> u64 {
  let mut i = 0u64
  let mut s = 0u64
  while i < n { s = s + i; i = i + 1u64 }
  s
}
const GREETING: String = "hello"
pub fn greet() -> String { GREETING }"#;

/// `unit` as its charged instance, naming the charged instance of every
/// source function it calls or forms, as the importer does. Constants it
/// reads are left as they are.
fn twin(unit: &LirUnit<Value>, operations: &OperationRegistry) -> LirUnit<Value> {
    let mut out = unit.clone();
    out.header.instance = hirsel_lir::energy::charged_digest(unit.header.instance);
    out.header.charged = true;
    for symbol in &mut out.tables.symbols {
        if let Some(instance) = symbol.instance
            && symbol.kind == SymbolKind::Function
            && operations
                .invocation_body(&symbol.definition, instance)
                .is_some()
        {
            symbol.instance = Some(hirsel_lir::energy::charged_digest(instance));
            symbol.charged = true;
        }
    }
    out
}

/// `unit` with `energy` charged just before the first failing instruction of
/// `block`, on a copy of that instruction's failure path.
fn charge(unit: &LirUnit<Value>, block: usize, energy: Energy) -> LirUnit<Value> {
    let mut out = unit.clone();
    let mut instructions = out.blocks[block]
        .instructions
        .get(&out.instructions)
        .unwrap()
        .to_vec();
    let (position, template) = instructions
        .iter()
        .enumerate()
        .find_map(|(p, i)| i.failure().map(|f| (p, f)))
        .expect("the block has a failing instruction");
    let mut copy = out.tables.failures[template.index()].clone();
    copy.rule = Some(hirsel_core_contract::ENERGY_RULE.to_string());
    let edge = out.tables.edges[copy.cleanup.index()].clone();
    copy.cleanup = EdgeId(out.tables.edges.len() as u32);
    out.tables.edges.push(edge);
    let failure = FailureId(out.tables.failures.len() as u32);
    out.tables.failures.push(copy);
    instructions.insert(position, Inst::Charge { energy, failure });
    let start = out.instructions.len() as u32;
    out.instructions.extend(instructions);
    out.blocks[block].instructions = PoolRange::new(start, out.instructions.len() as u32 - start);
    out
}

/// The block holding `unit`'s first call admission.
fn admitting(unit: &LirUnit<Value>) -> usize {
    (0..unit.blocks.len())
        .find(|&b| {
            unit.blocks[b]
                .instructions
                .get(&unit.instructions)
                .unwrap()
                .iter()
                .any(|i| matches!(i, Inst::FrameEnter { .. }))
        })
        .expect("the unit admits a call")
}

/// `unit` with its calls made through an adapter, which adds an activation
/// without admitting one.
fn adapter_only(unit: &LirUnit<Value>) -> LirUnit<Value> {
    let mut out = unit.clone();
    for call in &mut out.tables.calls {
        call.forward_caller = true;
    }
    let mut pool = Vec::new();
    for block in &mut out.blocks {
        let start = pool.len() as u32;
        pool.extend(
            block
                .instructions
                .get(&unit.instructions)
                .unwrap()
                .iter()
                .filter(|i| !matches!(i, Inst::FrameEnter { .. }))
                .cloned(),
        );
        block.instructions = PoolRange::new(start, pool.len() as u32 - start);
    }
    out.instructions = pool;
    out
}

/// The block whose jump closes `unit`'s loop: an edge back to an earlier block.
fn back_edge(unit: &LirUnit<Value>) -> usize {
    (0..unit.blocks.len())
        .find(|&b| {
            unit.terminators[b]
                .normal_edges(&unit.pools)
                .iter()
                .any(|e| unit.tables.edges[e.index()].target.index() <= b)
        })
        .expect("the unit has a loop")
}

#[test]
fn a_twin_is_checked_as_its_source_with_its_charges_stripped() {
    let (program, lowered) = lower_valid(SOURCE);
    let manifest = manifest(&program);
    let target = target();
    let operations = lowered.operation_registry(&program);
    let inputs = hirsel_lir::validate::Inputs {
        memo: Default::default(),
        discharge: None,
        operations: &operations,
        target: &target,
        layouts: &lowered.table,
        manifest: &manifest,
    };
    let check = |unit: &LirUnit<Value>| hirsel_lir::validate::check(unit, &inputs);
    let outer = unit_named(&lowered, "outer");
    let charged = charge(&twin(outer, &operations), admitting(outer), Energy::Entry);
    assert_eq!(check(&charged), vec![], "the stripped twin is outer itself");

    // One non-charge difference: the first call names another definition.
    // The twin is refused exactly as the source would be.
    let retarget = |unit: &mut LirUnit<Value>| {
        let Callee::Direct(symbol) = unit.tables.calls[0].callee else {
            panic!("outer calls inner directly");
        };
        let named = &mut unit.tables.symbols[symbol.index()].definition;
        *named = if *named == path("inner") {
            path("other")
        } else {
            path("inner")
        };
    };
    let mut source = outer.clone();
    retarget(&mut source);
    let mut forged = charged.clone();
    retarget(&mut forged);
    assert!(
        !check(&source).is_empty(),
        "the retargeted source is refused"
    );
    assert!(
        !check(&forged).is_empty(),
        "C accepted a twin that differs from its source"
    );

    // Charged code calling the uncharged instance of a source function.
    let mut uncharged_call = charge(outer, admitting(outer), Energy::Entry);
    uncharged_call.header.instance = charged.header.instance;
    uncharged_call.header.charged = true;
    assert!(
        !check(&uncharged_call).is_empty(),
        "C accepted a twin calling uncharged code"
    );

    // A twin that admits a call without its entry charge.
    assert!(
        !check(&twin(outer, &operations)).is_empty(),
        "C accepted a missing entry charge"
    );

    // A unit whose only call goes through an adapter still transfers control.
    // Removing the admission orphans its failure path, so this case runs the
    // charge rule C applies, not the whole of C.
    let apply = unit_named(&lowered, "apply");
    let forwarding = adapter_only(&twin(apply, &operations));
    let refused = hirsel_lir::energy::check(&forwarding);
    assert!(
        refused.is_err(),
        "an adapter-only unit needs no entry charge"
    );

    // A charge outside any loop is no iteration charge.
    let misplaced = charge(&charged, admitting(outer), Energy::Iteration);
    assert!(
        !check(&misplaced).is_empty(),
        "C accepted a misplaced charge"
    );

    // The uncharged instance carries no charge.
    let leaked = charge(outer, admitting(outer), Energy::Entry);
    assert!(
        !check(&leaked).is_empty(),
        "C accepted a charge in uncharged code"
    );

    // A loop charges each iteration; without that charge it is refused.
    let sum = unit_named(&lowered, "sum");
    let looping = charge(&twin(sum, &operations), back_edge(sum), Energy::Iteration);
    assert_eq!(check(&looping), vec![], "the charged loop is sum itself");
    assert!(
        !check(&twin(sum, &operations)).is_empty(),
        "C accepted an uncharged loop"
    );

    // Charged code reading a source constant names its charged copy; the
    // uncharged constant is refused by its symbol alone.
    let greet = unit_named(&lowered, "greet");
    assert!(
        greet
            .tables
            .symbols
            .iter()
            .any(|s| s.kind == SymbolKind::Constant),
        "greet reads its constant"
    );
    assert!(
        !check(&twin(greet, &operations)).is_empty(),
        "C accepted an uncharged constant"
    );
}

/// The charged instance's `Span` charges, as `(block, position)`.
fn spans(unit: &LirUnit<Value>) -> Vec<(usize, usize)> {
    (0..unit.blocks.len())
        .flat_map(|b| {
            unit.blocks[b]
                .instructions
                .get(&unit.instructions)
                .unwrap()
                .iter()
                .enumerate()
                .filter(|(_, i)| {
                    matches!(
                        i,
                        Inst::Charge {
                            energy: Energy::Span { .. },
                            ..
                        }
                    )
                })
                .map(move |(p, _)| (b, p))
        })
        .collect()
}

/// `unit` with the energy of its charge at `at` replaced by `edit`'s.
fn respan(
    unit: &LirUnit<Value>,
    (block, position): (usize, usize),
    edit: impl Fn(&mut Energy),
) -> LirUnit<Value> {
    let mut out = unit.clone();
    let start = out.blocks[block].instructions.start as usize;
    let Inst::Charge { energy, .. } = &mut out.instructions[start + position] else {
        panic!("a charge");
    };
    edit(energy);
    out
}

#[test]
fn a_counted_loop_charges_its_trip_count_once_only_when_its_test_is_that_amount() {
    // Ruling 1 (2026-11-11): a counted loop's one charge at entry stands only
    // when its exit test compares the counter, stepped by one, with the
    // charged amount. `count` has a `table.migrate` row function's shape,
    // so its Entry formation charges it.
    let (program, lowered) = lower_valid(
        r#"pub fn count(c: (), n: u64, m: u64) -> u64 {
  let mut s = 0u64
  for i in 0u64..n { s = s + i * m }
  for j in 1u64..=m { s = s + j }
  s
}
pub fn root() -> u64 { let e: Entry[fn(c: (), n: u64, m: u64) -> u64] = count
 0 }"#,
    );
    let manifest = manifest(&program);
    let target = target();
    let operations = lowered.operation_registry(&program);
    let inputs = hirsel_lir::validate::Inputs {
        memo: Default::default(),
        discharge: None,
        operations: &operations,
        target: &target,
        layouts: &lowered.table,
        manifest: &manifest,
    };
    let check = |unit: &LirUnit<Value>| hirsel_lir::validate::check(unit, &inputs);
    let charged = crate::rc_support::instances_of(&lowered, "count")
        .into_iter()
        .map(|id| &lowered.units[id.index()])
        .find(|unit| unit.header.charged)
        .expect("the Entry formation charges count");
    let spans = spans(charged);
    assert_eq!(spans.len(), 2, "both loops charge once at entry");
    assert_eq!(
        check(charged),
        vec![],
        "the importer's counted loops pass C"
    );

    for &at in &spans {
        // The amount is not the loop's bound: `from..from` charges nothing.
        let short = respan(charged, at, |energy| {
            if let Energy::Span { from, to, .. } = energy {
                *to = *from;
            }
        });
        assert!(
            !check(&short).is_empty(),
            "C accepted a Span amount that is not the exit bound"
        );
        // `a..b` charged for `a..=b` or the other way round.
        let flipped = respan(charged, at, |energy| {
            if let Energy::Span { inclusive, .. } = energy {
                *inclusive = !*inclusive;
            }
        });
        assert!(
            !check(&flipped).is_empty(),
            "C accepted a Span of the other range kind"
        );
    }
}

/// The charged instance of `name` lowered from `source`, which forms it as an
/// `Entry` so lowering charges it.
fn charged_unit(source: &str, name: &str) -> LirUnit<Value> {
    let (_, lowered) = lower_valid(source);
    crate::rc_support::instances_of(&lowered, name)
        .into_iter()
        .map(|id| lowered.units[id.index()].clone())
        .find(|unit| unit.header.charged)
        .expect("the Entry formation charges the function")
}

/// `unit`'s blocks reached in one step from `block`.
fn successors(unit: &LirUnit<Value>, block: usize) -> Vec<usize> {
    unit.terminators[block]
        .normal_edges(&unit.pools)
        .iter()
        .map(|e| unit.tables.edges[e.index()].target.index())
        .collect()
}

/// Whether some cycle of `unit` passes `block` and avoids every block in `cut`.
fn on_cycle_avoiding(unit: &LirUnit<Value>, block: usize, cut: &[usize]) -> bool {
    let mut seen = vec![false; unit.blocks.len()];
    let mut pending = successors(unit, block);
    while let Some(b) = pending.pop() {
        if b == block {
            return true;
        }
        if cut.contains(&b) || std::mem::replace(&mut seen[b], true) {
            continue;
        }
        pending.extend(successors(unit, b));
    }
    false
}

/// The blocks of `unit` holding a charge of `kind`.
fn charged_blocks(unit: &LirUnit<Value>, kind: fn(&Energy) -> bool) -> Vec<usize> {
    (0..unit.blocks.len())
        .filter(|&b| {
            unit.blocks[b]
                .instructions
                .get(&unit.instructions)
                .unwrap()
                .iter()
                .any(|i| matches!(i, Inst::Charge { energy, .. } if kind(energy)))
        })
        .collect()
}

/// `unit` with the charges of `block` removed.
fn uncharge(unit: &LirUnit<Value>, block: usize) -> LirUnit<Value> {
    let mut out = unit.clone();
    let kept: Vec<_> = out.blocks[block]
        .instructions
        .get(&out.instructions)
        .unwrap()
        .iter()
        .filter(|i| !matches!(i, Inst::Charge { .. }))
        .cloned()
        .collect();
    let start = out.instructions.len() as u32;
    out.instructions.extend(kept);
    out.blocks[block].instructions = PoolRange::new(start, out.instructions.len() as u32 - start);
    out
}

#[test]
fn a_self_tail_loop_is_paid_by_its_entry_charge_on_every_trip() {
    // A self tail call becomes a loop whose header is the body's entry
    // block, which holds the entry charge: every trip passes it, as every
    // activation would (std ordered_map `find`, which STORE-3 reaches).
    let unit = charged_unit(
        r#"fn step(x: u64) -> u64 { x + 1u64 }
pub fn down(c: (), n: u64, acc: u64) -> u64 {
  if n == 0u64 { return acc }
  down(c, n - 1u64, step(acc))
}
pub fn root() -> u64 { let e: Entry[fn(c: (), n: u64, acc: u64) -> u64] = down
 0 }"#,
        "down",
    );
    let entry = charged_blocks(&unit, |e| matches!(e, Energy::Entry));
    assert_eq!(entry.len(), 1, "one entry charge");
    assert!(
        on_cycle_avoiding(&unit, entry[0], &[]),
        "the entry charge is on the tail loop"
    );
    assert_eq!(hirsel_lir::energy::check(&unit), Ok(()));
    // The same loop without its entry charge pays nothing per trip.
    assert!(hirsel_lir::energy::check(&uncharge(&unit, entry[0])).is_err());
}

#[test]
fn a_cycle_charged_on_one_arm_only_is_refused() {
    let unit = charged_unit(
        r#"fn step(x: u64) -> u64 { x + 1u64 }
pub fn walk(c: (), n: u64, m: u64) -> u64 {
  let mut i = 0u64
  let mut s = step(m)
  while i < n {
    if i < m { s = s + i } else { s = s * m }
    i = i + 1u64
  }
  s
}
pub fn root() -> u64 { let e: Entry[fn(c: (), n: u64, m: u64) -> u64] = walk
 0 }"#,
        "walk",
    );
    assert_eq!(hirsel_lir::energy::check(&unit), Ok(()));
    let header = charged_blocks(&unit, |e| matches!(e, Energy::Iteration));
    assert_eq!(header.len(), 1, "one iteration charge");
    let bare = uncharge(&unit, header[0]);
    // The loop's two arms: both targets of a branch on the loop.
    let arms = (0..unit.blocks.len())
        .map(|b| successors(&unit, b))
        .find(|s| s.len() == 2 && s.iter().all(|&a| on_cycle_avoiding(&unit, a, &[])))
        .expect("a branch inside the loop");
    for &arm in &arms {
        assert!(
            on_cycle_avoiding(&unit, header[0], &[arm]),
            "a cycle avoids each arm"
        );
        let one = charge(&bare, arm, Energy::Iteration);
        assert!(
            hirsel_lir::energy::check(&one).is_err(),
            "accepted a loop with one arm uncharged"
        );
    }
    let both = charge(
        &charge(&bare, arms[0], Energy::Iteration),
        arms[1],
        Energy::Iteration,
    );
    assert_eq!(hirsel_lir::energy::check(&both), Ok(()));
}

#[test]
fn a_nested_loop_whose_inner_cycle_is_uncharged_is_refused() {
    let unit = charged_unit(
        r#"fn step(x: u64) -> u64 { x + 1u64 }
pub fn grid(c: (), n: u64, m: u64) -> u64 {
  let mut i = 0u64
  let mut s = step(m)
  while i < n {
    let mut j = 0u64
    while j < m { s = s + j; j = j + 1u64 }
    i = i + 1u64
  }
  s
}
pub fn root() -> u64 { let e: Entry[fn(c: (), n: u64, m: u64) -> u64] = grid
 0 }"#,
        "grid",
    );
    assert_eq!(hirsel_lir::energy::check(&unit), Ok(()));
    let headers = charged_blocks(&unit, |e| matches!(e, Energy::Iteration));
    assert_eq!(headers.len(), 2, "both loops charge each trip");
    // Removing a loop's charge leaves the unit paid exactly when every cycle
    // through that block passes the other charge; the inner loop's cycle
    // avoids the outer charge, so removing the inner charge is refused.
    let mut refused = 0;
    for &h in &headers {
        let other = *headers.iter().find(|&&o| o != h).unwrap();
        let unpaid = on_cycle_avoiding(&unit, h, &[other]);
        refused += usize::from(unpaid);
        assert_eq!(
            hirsel_lir::energy::check(&uncharge(&unit, h)).is_err(),
            unpaid,
            "block {h}: a cycle through it avoids the other charge: {unpaid}"
        );
    }
    assert!(
        refused >= 1,
        "the inner loop's cycle avoids the outer charge"
    );
}

#[test]
fn a_charged_copy_whose_push_uses_reserved_room_needs_no_entry_charge() {
    // std `bytes.from_inline`, which STORE-3 reaches: FACT-3 retargets its
    // push into reserved room to `$native.list_push_spare`, still the leaf
    // `list.push` that lowering saw, so the unit makes no call that adds an
    // activation and carries no entry charge.
    let (_, lowered) = crate::rc_support::lower(
        r#"pub fn inlined[n: size](xs: Inline[u8, n]) -> unique Bytes uses {} {
  let mut out: List[u8] = []
  core@1/list.reserve(mut out, n)
  for b in xs {
    core@1/list.push(mut out, b)
  }
  core@1/bytes.from_list(take out)
}
pub fn spill(c: (), n: u64, m: u64) -> u64 {
  let xs: Inline[u8, 4] = [1u8; 4]
  let b = inlined(xs)
  bytes.len(b)
}
pub fn root() -> u64 { let e: Entry[fn(c: (), n: u64, m: u64) -> u64] = spill
 0 }"#,
    );
    let unit = crate::rc_support::instances_of(&lowered, "inlined")
        .into_iter()
        .map(|id| &lowered.units[id.index()])
        .find(|unit| unit.header.charged)
        .expect("the charged spill charges inlined");
    let spare = unit.tables.calls.iter().any(|call| match call.callee {
        Callee::Direct(s) => unit.tables.symbols[s.index()]
            .definition
            .is_core("$native.list_push_spare"),
        _ => false,
    });
    assert!(spare, "the push is retargeted to the reserved room");
    assert!(charged_blocks(unit, |e| matches!(e, Energy::Entry)).is_empty());
    assert_eq!(hirsel_lir::energy::check(unit), Ok(()));
}
