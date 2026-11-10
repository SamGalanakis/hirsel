//! MODE-6 recorders over the runtime's task-local queue (`hirsel-rt/src/recorders.rs`).
//!
//! A recorder is a resource handle whose close drops the events it still holds. Events
//! cross as `Option[E]` images, as a channel's messages do: `recorder.new` gives the
//! runtime the `None` image an append leaves and the descriptor that drops a queued
//! event; `recorder.append` moves a `Some` image in; `recorder.finish` moves every
//! event, oldest first, into a fresh list and then drops the empty recorder.
use super::LibraryBody;
use super::builder::{Body, Cx, Direct};
use super::iter::{drop_local, local, option};
use super::slots::construct;
use super::sync::{call, none_local, put_image, take_some};
use super::tasks::{Intrinsic, argument, exclusive, intrinsic, option_of};
use super::{R, Request};
use crate::PassError;
use hirsel_core_contract::Passing;
use hirsel_lir::ir::*;
use hirsel_lir::layout_types::LayoutId;
use hirsel_lir::stage::Value;
use hirsel_syntax::DefPath;
use hirsel_types::GenericArgument;

pub static NEW: LibraryBody = LibraryBody {
    path: || DefPath::core("recorder.new"),
    build: new,
};
pub static APPEND: LibraryBody = LibraryBody {
    path: || DefPath::core("recorder.append"),
    build: append,
};
pub static FINISH: LibraryBody = LibraryBody {
    path: || DefPath::core("recorder.finish"),
    build: finish,
};

/// The event's `Option` layout and the named entries.
fn entries(cx: &mut Cx<'_>, names: &[&str]) -> R<(LayoutId, Vec<Intrinsic>)> {
    let event = cx.type_actual(0)?;
    let (option_layout, _) = option_of(cx, event)?;
    let entries = names
        .iter()
        .map(|name| intrinsic(cx, name, vec![GenericArgument::Type(event)]))
        .collect::<R<Vec<_>>>()?;
    Ok((option_layout, entries))
}

/// Keep `value` in a local across the cleanup of other locals; then take it back.
fn keep(b: &mut Body<'_, '_>, value: ValueId) -> R<(SlotId, AddressId)> {
    local(b, value)
}
fn release(b: &mut Body<'_, '_>, (slot, address): (SlotId, AddressId)) -> R<ValueId> {
    let value = b.take(address)?;
    b.emptied(slot);
    b.push(Inst::StorageDead {
        slot,
        if_live: false,
    });
    Ok(value)
}

/// End the recorder's `loan`, if it is active, on entry to every failure cleanup:
/// the calls under it can fail before it ends on the normal path, and the cleanup
/// may drop the recorder it borrows.
fn end_at_failures(b: &mut Body<'_, '_>, loan: LoanId) {
    for (i, block) in b.ed.unit.blocks.iter().enumerate() {
        if block.kind == BlockKind::FailureCleanup {
            b.ed.code[i].insert(
                0,
                Inst::EndLoan {
                    loan,
                    if_active: true,
                },
            );
        }
    }
}

fn ret_unit(b: &mut Body<'_, '_>) {
    let unit =
        b.cx.fn_abi()
            .result
            .map(|r| b.constant(r.layout, ConstantValue::Unit));
    b.ret(unit);
}

/// `recorder.new[E]() -> Recorder[E]`.
fn new(cx: &mut Cx<'_>) -> R<LirUnit<Value>> {
    let (option_layout, entries) = entries(cx, &["RecorderOpen"])?;
    let u32_layout = cx.primitive(hirsel_types::Primitive::U32)?;
    let mut b = Body::new(cx)?;
    let none = option(&mut b, option_layout, 0, &[]);
    let (none_slot, none_address) = local(&mut b, none)?;
    let (none, none_loan) = b.borrow(none_address, false)?;
    let descriptor = b.constant(u32_layout, ConstantValue::Descriptor(option_layout));
    let recorder = call(
        &mut b,
        &entries[0],
        vec![
            argument(0, Passing::Borrow, none),
            argument(1, Passing::Borrow, descriptor),
        ],
        &[none_loan],
    )
    .ok_or_else(|| PassError::Malformed("a recorder open without its handle".into()))?;
    let kept = keep(&mut b, recorder)?;
    drop_local(&mut b, none_slot, none_address);
    let recorder = release(&mut b, kept)?;
    b.ret(Some(recorder));
    Ok(b.finish())
}

/// `recorder.append[E](r, take event)`.
fn append(cx: &mut Cx<'_>) -> R<LirUnit<Value>> {
    let (option_layout, entries) = entries(cx, &["RecorderAppend"])?;
    let mut b = Body::new(cx)?;
    let &[r, event] = b.parameters.as_slice() else {
        return Err(PassError::Malformed("recorder.append parameters".into()));
    };
    let (r, loan) = b.borrow(r.address, false)?;
    let value = b.take(event.address)?;
    put_image(&mut b, &entries[0], Some(r), option_layout, value)?;
    b.end_loan(loan);
    ret_unit(&mut b);
    end_at_failures(&mut b, loan);
    Ok(b.finish())
}

/// `list.push[E]`, called directly.
fn push(cx: &mut Cx<'_>) -> R<Direct> {
    let path = DefPath::core("list.push");
    let event = cx.type_actual(0)?;
    let request = Request::operation(
        cx.program,
        super::body_index(&path).expect("registered list.push"),
        &path,
        vec![GenericArgument::Type(event)],
    )?;
    cx.require(request)
}

/// `recorder.finish[E](take r) -> List[E]`: the events oldest first.
fn finish(cx: &mut Cx<'_>) -> R<LirUnit<Value>> {
    let (option_layout, entries) = entries(cx, &["RecorderCount", "RecorderTake"])?;
    let [count_entry, take_entry] = &entries[..] else {
        unreachable!()
    };
    let push = push(cx)?;
    let list_layout = cx
        .fn_abi()
        .result
        .ok_or_else(|| PassError::Malformed("recorder.finish without a result".into()))?
        .layout;
    let mut b = Body::new(cx)?;
    let &[r] = b.parameters.as_slice() else {
        return Err(PassError::Malformed("recorder.finish parameters".into()));
    };
    let (handle, loan) = b.borrow(r.address, false)?;
    let count = call(
        &mut b,
        count_entry,
        vec![argument(0, Passing::Borrow, handle)],
        &[],
    )
    .ok_or_else(|| PassError::Malformed("a recorder count without its result".into()))?;
    let empty = construct(&mut b, list_layout, ConstructorKind::Sequence, &[])?;
    let (out_slot, out_address) = local(&mut b, empty)?;
    b.seq_reserve(out_address, count)?;
    let zero = b.u64(0)?;
    let one = b.u64(1)?;
    let (index_slot, index_address) = local(&mut b, zero)?;
    let slots = [out_slot, index_slot];
    let versions: Vec<_> = slots.iter().map(|&s| b.version(s)).collect();
    let head = b.block(BlockKind::Body, &versions);
    let next = b.block(BlockKind::Body, &[]);
    let done = b.block(BlockKind::Body, &[]);
    let held = b.held_state();
    let reaching = slots
        .iter()
        .map(|&s| b.reaching(s))
        .collect::<R<Vec<_>>>()?;
    b.jump(head, &reaching);

    b.at(head);
    for (&slot, &version) in slots.iter().zip(&versions) {
        b.set_reaching(slot, version);
    }
    let at = b.take(index_address)?;
    let more = b.compare(BinaryOp::Lt, at, count)?;
    b.branch(more, next, done);

    b.at(next);
    let (event_slot, event_address) = none_local(&mut b, option_layout)?;
    let arguments = vec![
        argument(0, Passing::Borrow, handle),
        exclusive(&mut b, 1, event_slot, event_address),
    ];
    call(&mut b, take_entry, arguments, &[]);
    let event = take_some(&mut b, event_slot, event_address, option_layout)?;
    let (_, out_loan) = b.borrow(out_address, true)?;
    let arguments = vec![
        exclusive(&mut b, 0, out_slot, out_address),
        argument(1, Passing::Take, event),
    ];
    let symbol = b.symbol(push.symbol.clone());
    b.call(
        Callee::Direct(symbol),
        vec![],
        push.abi,
        arguments,
        &[out_loan],
    );
    let after = b.add(ArithMode::Wrapping, at, one);
    b.store(index_address, after)?;
    let reaching = slots
        .iter()
        .map(|&s| b.reaching(s))
        .collect::<R<Vec<_>>>()?;
    b.jump(head, &reaching);

    b.at(done);
    for (&slot, &version) in slots.iter().zip(&versions) {
        b.set_reaching(slot, version);
    }
    b.restore_held(&held);
    b.end_loan(loan);
    // The recorder is empty: its close frees the queue.
    drop_local(&mut b, r.slot, r.address);
    let events = release(&mut b, (out_slot, out_address))?;
    b.ret(Some(events));
    end_at_failures(&mut b, loan);
    Ok(b.finish())
}
