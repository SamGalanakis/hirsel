//! `task.isolate[R](tasks, take body)` and `runner.apply[_mut]` (FAIL-2) over the runtime's
//! `isolate.rs`.
//!
//! The body reads the running task's failure record count, then calls `body`. A
//! successful call returns `Ok(r)`. A failed one has unwound the callback's
//! frames (FAIL-3) when its failure cleanup reaches the `Isolate` terminator,
//! which catches every record since that count and resumes here. The caught
//! chain becomes the `Failure` value innermost first: each record's fields come
//! from the `$intrinsic.Isolate*` entries, and each record but the primary is
//! boxed as the cause of the one before it.
use super::LibraryBody;
use super::arena::{RESULT_ERROR, RESULT_OK, payload, variant};
use super::builder::{Body, Cx};
use super::iter::{drop_local_to, local, option};
use super::slots::{construct, nominal};
use super::tasks::{Intrinsic, argument, intrinsic, invoke, operation, parameter};
use crate::PassError;
use hirsel_core_contract::Passing;
use hirsel_lir::abi_types::CallableShape;
use hirsel_lir::ir::*;
use hirsel_lir::layout_types::LayoutId;
use hirsel_lir::stage::Value;
use hirsel_syntax::DefPath;

type R<T> = Result<T, PassError>;

pub static ISOLATE: LibraryBody = LibraryBody {
    path: || DefPath::core("task.isolate"),
    build: isolate,
};
pub static APPLY: LibraryBody = LibraryBody {
    path: || DefPath::core("runner.apply"),
    build: apply,
};
pub static APPLY_MUT: LibraryBody = LibraryBody {
    path: || DefPath::core("runner.apply_mut"),
    build: apply,
};

/// `FailureKind`'s variants, by the ordinal `$intrinsic.IsolateKind` returns.
const KINDS: u32 = 5;

pub(super) struct Entries {
    /// `IsolateBegin(tasks)` for `task.isolate`, `ApplyBegin()` for `runner.apply`.
    begin: Intrinsic,
    end: Intrinsic,
    count: Intrinsic,
    kind: Intrinsic,
    rule: Intrinsic,
    definition: Intrinsic,
    line: Intrinsic,
    column: Intrinsic,
    spawn: Intrinsic,
}

/// The layouts of a `Failure` record and its parts.
pub(super) struct Shape {
    failure: LayoutId,
    kind: LayoutId,
    site: LayoutId,
    /// `Option[box Failure]` and its `Some` payload, the box.
    cause: LayoutId,
    boxed: LayoutId,
}

fn field(b: &Body<'_, '_>, layout: LayoutId, ordinal: u32) -> R<LayoutId> {
    b.record(layout)
        .fields
        .iter()
        .find(|f| f.ordinal == ordinal)
        .map(|f| f.layout)
        .ok_or_else(|| PassError::Malformed(format!("a Failure part has no field {ordinal}")))
}

fn isolate(cx: &mut Cx<'_>) -> R<LirUnit<Value>> {
    let callback = parameter(cx.types, cx.callable, 1)?;
    let body_abi = cx
        .layouts
        .fn_abi(cx.types, callback, CallableShape::Closure)
        .map_err(|e| PassError::Layout(cx.path.clone(), e))?;
    let returned = cx
        .fn_abi()
        .result
        .map(|r| r.layout)
        .ok_or_else(|| PassError::Malformed("task.isolate returns a Result".into()))?;
    let entries = entries(cx, "IsolateBegin")?;
    let mut b = Body::new(cx)?;
    let &[tasks, body] = b.parameters.as_slice() else {
        return Err(PassError::Malformed("task.isolate parameters".into()));
    };
    let failure = payload(&b, returned, RESULT_ERROR)
        .ok_or_else(|| PassError::Malformed("task.isolate's Result has no error".into()))?;
    let shape = shape(&b, failure)?;
    let tasks = b.read_bits(tasks.address)?;
    let start = begin(&mut b, &entries, Some(tasks))?;
    // The callable dies with this frame on both outcomes.
    let f = b.take(body.address)?;
    let (f_slot, f_address) = local(&mut b, f)?;
    let closure_held = b.held_state();

    // The consumed closure's destruction belongs to the isolated body too.
    // On callback failure, release it before catching the combined chain.
    let normal = b.current();
    let dropping_failed = b.block(BlockKind::FailureCleanup, &[]);
    let catching = b.block(BlockKind::FailureCleanup, &[]);
    let caught = b.block(BlockKind::Body, &[]);
    b.at(catching);
    let resume = b.edge(caught, &[]);
    b.terminate(Terminator::Isolate {
        start,
        caught: resume,
    });
    b.at(dropping_failed);
    drop_local_to(&mut b, f_slot, f_address, Some(catching));
    b.jump(catching, &[]);
    b.at(normal);
    b.restore_held(&closure_held);
    let unwound = b.failure_to(dropping_failed, &[], None);
    let (callee, loan) = b.borrow(f_address, false)?;
    let value = invoke(
        &mut b,
        (Callee::Indirect(callee), Vec::new()),
        body_abi,
        Vec::new(),
        &[loan],
        unwound,
        BlockKind::Body,
    );
    // Dropping the callback can fail. Keep its owned result in the frame's
    // cleanup until that drop succeeds, then transfer it into Ok.
    let held = value.map(|value| local(&mut b, value)).transpose()?;
    drop_local_to(&mut b, f_slot, f_address, Some(catching));
    let ok = match held {
        Some((slot, address)) => {
            let value = b.take(address)?;
            b.push(Inst::StorageDead {
                slot,
                if_live: false,
            });
            vec![value]
        }
        None => match payload(&b, returned, RESULT_OK) {
            Some(unit) => vec![b.constant(unit, ConstantValue::Unit)],
            None => Vec::new(),
        },
    };
    let value = variant(&mut b, returned, "Result", RESULT_OK, &ok);
    returning(&mut b, &entries, value)?;

    b.at(caught);
    let failure = chain(&mut b, &entries, &shape)?;
    let value = variant(&mut b, returned, "Result", RESULT_ERROR, &[failure]);
    returning(&mut b, &entries, value)?;
    Ok(b.finish())
}

/// `runner.apply[_mut](f, c, call, a)` (RUN-3, FAIL-2): call the entry `f` on this task with
/// the other parameters as they arrived, inside a catch that is no scheduling point. The
/// no-failure path is `ApplyBegin`, the indirect call, a status check and `IsolateEnd`; a
/// failure has run `f`'s cleanup (FAIL-3) when the `Isolate` terminator catches it here.
fn apply(cx: &mut Cx<'_>) -> R<LirUnit<Value>> {
    let entry = parameter(cx.types, cx.callable, 0)?;
    let f = super::runners::entry_callable(cx.types, entry)?;
    let f_abi = cx
        .layouts
        .fn_abi(cx.types, f, CallableShape::Closure)
        .map_err(|e| PassError::Layout(cx.path.clone(), e))?;
    let returned = cx
        .fn_abi()
        .result
        .map(|r| r.layout)
        .ok_or_else(|| PassError::Malformed("runner.apply returns a Result".into()))?;
    let entries = entries(cx, "ApplyBegin")?;
    let mut b = Body::new(cx)?;
    let failure = payload(&b, returned, RESULT_ERROR)
        .ok_or_else(|| PassError::Malformed("runner.apply's Result has no error".into()))?;
    let shape = shape(&b, failure)?;
    let parameters = b.parameters.clone();
    let Some((f_parameter, forwarded)) = parameters.split_first() else {
        return Err(PassError::Malformed("runner.apply parameters".into()));
    };
    let start = begin(&mut b, &entries, None)?;
    let held = b.held_state();
    let normal = b.current();
    let catching = b.block(BlockKind::FailureCleanup, &[]);
    let caught = b.block(BlockKind::Body, &[]);
    b.at(catching);
    let resume = b.edge(caught, &[]);
    b.terminate(Terminator::Isolate {
        start,
        caught: resume,
    });
    b.at(normal);
    let unwound = b.failure_to(catching, &[], None);
    let (callee, f_loan) = b.borrow(f_parameter.address, false)?;
    let mut loans = vec![f_loan];
    let mut arguments = Vec::new();
    for (ordinal, p) in forwarded.iter().enumerate() {
        let value = match p.passing {
            Passing::Borrow => {
                let (value, loan) = b.borrow(p.address, false)?;
                loans.push(loan);
                ArgumentValue::Value(value)
            }
            // `apply_mut`'s `c`, prepared at entry: the call defines its next version.
            Passing::Mut => {
                let version = b.version(p.slot);
                b.set_reaching(p.slot, version);
                ArgumentValue::Address {
                    address: p.address,
                    version,
                }
            }
            Passing::Take => {
                return Err(PassError::Malformed(
                    "runner.apply forwards no owned parameter".into(),
                ));
            }
        };
        arguments.push(CallArgument {
            parameter: ordinal as u32,
            passing: p.passing,
            value,
        });
    }
    let value = invoke(
        &mut b,
        (Callee::Indirect(callee), Vec::new()),
        f_abi,
        arguments,
        &loans,
        unwound,
        BlockKind::Body,
    )
    .ok_or_else(|| PassError::Malformed("an applied entry returns its Result".into()))?;
    let value = variant(&mut b, returned, "Result", RESULT_OK, &[value]);
    returning(&mut b, &entries, value)?;

    b.at(caught);
    b.restore_held(&held);
    let failure = chain(&mut b, &entries, &shape)?;
    let value = variant(&mut b, returned, "Result", RESULT_ERROR, &[failure]);
    returning(&mut b, &entries, value)?;
    Ok(b.finish())
}

/// The caught chain's entries, under no actuals, opened by the intrinsic `begin`.
pub(super) fn entries(cx: &mut Cx<'_>, begin: &str) -> R<Entries> {
    let mut entry = |name: &str| intrinsic(cx, name, Vec::new());
    Ok(Entries {
        begin: entry(begin)?,
        end: entry("IsolateEnd")?,
        count: entry("IsolateCount")?,
        kind: entry("IsolateKind")?,
        rule: entry("IsolateRule")?,
        definition: entry("IsolateDefinition")?,
        line: entry("IsolateLine")?,
        column: entry("IsolateColumn")?,
        spawn: entry("IsolateSpawn")?,
    })
}

/// The parts of the `Failure` record at `failure`.
pub(super) fn shape(b: &Body<'_, '_>, failure: LayoutId) -> R<Shape> {
    let cause = field(b, failure, 4)?;
    Ok(Shape {
        failure,
        kind: field(b, failure, 0)?,
        site: field(b, failure, 2)?,
        cause,
        boxed: payload(b, cause, 1)
            .ok_or_else(|| PassError::Malformed("a Failure cause holds no box".into()))?,
    })
}

/// `IsolateBegin(tasks)` or `ApplyBegin()`: the running task's failure record count, the
/// start of a catch.
pub(super) fn begin(b: &mut Body<'_, '_>, entries: &Entries, tasks: Option<ValueId>) -> R<ValueId> {
    let mark = read(b, &entries.begin, tasks)?;
    let at = b.current();
    let held = b.held_state();
    let ending = b.block(BlockKind::FailureCleanup, &[]);
    b.at(ending);
    b.restore_held(&[]);
    end_at(b, entries, BlockKind::FailureCleanup);
    b.terminate(Terminator::Propagate);
    b.at(at);
    b.restore_held(&held);
    b.propagation(Some(ending));
    Ok(mark)
}

fn returning(b: &mut Body<'_, '_>, entries: &Entries, value: ValueId) -> R<()> {
    let (slot, address) = local(b, value)?;
    end(b, entries);
    let value = b.take(address)?;
    b.push(Inst::StorageDead {
        slot,
        if_live: false,
    });
    b.ret(Some(value));
    Ok(())
}

pub(super) fn end(b: &mut Body<'_, '_>, entries: &Entries) {
    end_at(b, entries, BlockKind::Body);
}

fn end_at(b: &mut Body<'_, '_>, entries: &Entries, after: BlockKind) {
    // End is non-failing; its syntactic failure edge must not execute End twice.
    let propagation = b.propagation(None);
    let failure = b.failure(None);
    let callee = operation(b, &entries.end);
    invoke(b, callee, entries.end.abi, Vec::new(), &[], failure, after);
    b.propagation(propagation);
}

/// Call a runtime entry of the caught chain with its one word argument, if any; its outcome
/// propagates.
fn read(b: &mut Body<'_, '_>, entry: &Intrinsic, word: Option<ValueId>) -> R<ValueId> {
    let arguments = word
        .map(|w| argument(0, Passing::Borrow, w))
        .into_iter()
        .collect();
    let failure = b.failure(None);
    let callee = operation(b, entry);
    invoke(
        b,
        callee,
        entry.abi,
        arguments,
        &[],
        failure,
        BlockKind::Body,
    )
    .ok_or_else(|| PassError::Malformed("an isolation entry without its result".into()))
}

/// The caught chain as one `Failure`, built from its last record to its primary.
pub(super) fn chain(b: &mut Body<'_, '_>, entries: &Entries, shape: &Shape) -> R<ValueId> {
    let count = read(b, &entries.count, None)?;
    let none = option(b, shape.cause, 0, &[]);
    let (cause_slot, cause_address) = local(b, none)?;
    let all_ones = b.u64(u64::MAX)?;
    let last = b.add(ArithMode::Wrapping, count, all_ones);
    let u64_layout = b.layout_of(count);
    let index = b.value(u64_layout, Holding::Plain);
    let version = b.version(cause_slot);
    let initial = b.reaching(cause_slot)?;
    let record = b.block(BlockKind::Body, &[index, version]);
    b.jump(record, &[last, initial]);

    b.at(record);
    b.set_reaching(cause_slot, version);
    let zero = b.u64(0)?;
    let primary = b.compare(BinaryOp::Eq, index, zero)?;
    let failure = failure_at(b, entries, shape, index, cause_address)?;
    let done = b.block(BlockKind::Body, &[]);
    let more = b.block(BlockKind::Body, &[]);
    b.branch(primary, done, more);

    b.at(more);
    let boxed = construct(b, shape.boxed, ConstructorKind::Box, &[failure])?;
    let some = option(b, shape.cause, 1, &[boxed]);
    b.store(cause_address, some)?;
    b.hold(cause_slot, cause_address);
    let previous = b.add(ArithMode::Wrapping, index, all_ones);
    let after = b.reaching(cause_slot)?;
    b.jump(record, &[previous, after]);

    b.at(done);
    b.emptied(cause_slot);
    b.push(Inst::StorageDead {
        slot: cause_slot,
        if_live: false,
    });
    Ok(failure)
}

/// Caught record `index` as a `Failure` whose cause is moved out of `cause`.
/// Each construct consumes its fields on both outcomes, so the site is built
/// before the record's other owned parts leave their temporaries.
fn failure_at(
    b: &mut Body<'_, '_>,
    entries: &Entries,
    shape: &Shape,
    index: ValueId,
    cause: AddressId,
) -> R<ValueId> {
    let kind = kind(b, entries, shape, index)?;
    let line = read(b, &entries.line, Some(index))?;
    let column = read(b, &entries.column, Some(index))?;
    let mut held = Vec::new();
    for entry_point in [&entries.rule, &entries.definition, &entries.spawn] {
        let value = read(b, entry_point, Some(index))?;
        held.push(local(b, value)?);
    }
    let take = |b: &mut Body<'_, '_>, (slot, address): (SlotId, AddressId)| -> R<ValueId> {
        let value = b.take(address)?;
        b.push(Inst::StorageDead {
            slot,
            if_live: false,
        });
        Ok(value)
    };
    let definition = take(b, held[1])?;
    let symbol = nominal(b, "SourceSite");
    let site = construct(
        b,
        shape.site,
        ConstructorKind::Record(symbol),
        &[definition, line, column],
    )?;
    let rule = take(b, held[0])?;
    let spawn = take(b, held[2])?;
    let cause = b.take(cause)?;
    let symbol = nominal(b, "Failure");
    construct(
        b,
        shape.failure,
        ConstructorKind::Record(symbol),
        &[kind, rule, site, spawn, cause],
    )
}

/// Caught record `index`'s `FailureKind`, from its ordinal.
fn kind(b: &mut Body<'_, '_>, entries: &Entries, shape: &Shape, index: ValueId) -> R<ValueId> {
    let ordinal = read(b, &entries.kind, Some(index))?;
    let value = b.value(shape.kind, Holding::Plain);
    let joined = b.block(BlockKind::Body, &[value]);
    let arms: Vec<_> = (0..KINDS).map(|_| b.block(BlockKind::Body, &[])).collect();
    let invalid = b.block(BlockKind::Body, &[]);
    let otherwise = b.edge(invalid, &[]);
    let edges: Vec<_> = arms.iter().map(|&arm| b.edge(arm, &[])).collect();
    let switch = PoolRange::push(
        &mut b.ed.unit.pools.switch_arms,
        edges
            .into_iter()
            .enumerate()
            .map(|(value, edge)| SwitchArm {
                value: value as u32,
                edge,
            }),
    );
    b.terminate(Terminator::Switch {
        value: ordinal,
        arms: switch,
        otherwise,
    });
    for (ordinal, arm) in arms.into_iter().enumerate() {
        b.at(arm);
        let kind = variant(b, shape.kind, "FailureKind", ordinal as u32, &[]);
        b.jump(joined, &[kind]);
    }
    b.at(invalid);
    b.terminate(Terminator::InvariantFault {
        reason: "a failure kind ordinal past FailureKind".into(),
    });
    b.at(joined);
    Ok(value)
}
