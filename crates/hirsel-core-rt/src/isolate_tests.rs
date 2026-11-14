use super::isolate::*;
use crate::{
    Context,
    causes::{self, Cause, Causes},
};

fn started(limit: u64) -> Context {
    let mut context = Context::new(limit);
    causes::task_start(&mut context).unwrap();
    context
}

fn drop_caught(raw: *mut causes::Cell) {
    // SAFETY: isolate_catch transfers the complete owned chain exactly once.
    if !raw.is_null() {
        unsafe { drop(Causes::from_raw(raw)) };
    }
}

#[test]
fn returned_body_refunds_the_isolate_cell() {
    let mut context = started(1024);
    let available = context.available();
    let entered = unsafe { hirsel_core_isolate_enter(&mut context) };
    assert_eq!((entered.entered, entered.available), (true, 0));
    assert_eq!(context.available(), available - 32);
    unsafe { hirsel_core_isolate_leave(&mut context) };
    assert_eq!(
        context.available(),
        available,
        "before={available}, after={}",
        context.available()
    );
}

#[test]
fn refusal_returns_room_and_changes_nothing() {
    let mut context = Context::new(31);
    let entered = unsafe { hirsel_core_isolate_enter(&mut context) };
    assert_eq!((entered.entered, entered.available), (false, 31));
    assert_eq!(context.available(), 31);
    assert!(context.isolates.is_empty());
}

#[test]
fn held_reservation_leaves_thirty_nine_bytes_at_limit_seventy_one() {
    let mut context = Context::new(71);
    let entered = unsafe { hirsel_core_isolate_enter(&mut context) };
    assert_eq!((entered.entered, entered.available), (true, 0));
    assert_eq!(context.available(), 39);
    let nested = unsafe { hirsel_core_isolate_enter(&mut context) };
    assert_eq!((nested.entered, nested.available), (true, 0));
    assert_eq!(context.available(), 7);
    unsafe { hirsel_core_isolate_leave(&mut context) };
    unsafe { hirsel_core_isolate_leave(&mut context) };
    assert_eq!(context.available(), 71);
}

#[test]
fn caught_primary_keeps_exactly_one_reserved_cell() {
    let mut context = started(1024);
    let before = context.available();
    assert!(unsafe { hirsel_core_isolate_enter(&mut context) }.entered);
    unsafe { causes::record(&mut context, Cause::new(causes::UNWRAP, 7)) };
    let caught = unsafe { hirsel_core_isolate_catch(&mut context) };
    assert_eq!(caught.status, 0);
    assert!(!caught.causes.is_null());
    assert!(context.primary.is_some());
    assert_eq!(context.available(), before - 32);
    drop_caught(caught.causes);
    assert_eq!(context.available(), before);
}

#[test]
fn injected_capacity_refusal_uses_the_primary_and_keeps_the_reserved_cell() {
    let mut context = started(1_000_000);
    context.current.inject(2, false);
    let before = context.available();
    assert!(unsafe { hirsel_core_isolate_enter(&mut context) }.entered);
    let allocation = unsafe {
        crate::abi::hirsel_core_alloc(
            &context,
            std::ptr::null_mut(),
            8,
            8,
            40,
            false,
            crate::block::PLAIN,
        )
    };
    assert!(allocation.pointer.is_null());
    unsafe { causes::record(&mut context, Cause::new(causes::UNWRAP, 13)) };
    let caught = unsafe { hirsel_core_isolate_catch(&mut context) };
    assert_eq!(caught.status, 0);
    assert_eq!(context.available(), before - 32);
    drop_caught(caught.causes);
    drop(context.primary.take());
}

#[test]
fn nested_isolates_replace_the_current_primary_in_lifo_order() {
    let mut context = started(2048);
    assert!(unsafe { hirsel_core_isolate_enter(&mut context) }.entered);
    assert!(unsafe { hirsel_core_isolate_enter(&mut context) }.entered);
    unsafe { causes::record(&mut context, Cause::new(causes::BOUNDS, 9)) };
    let inner = unsafe { hirsel_core_isolate_catch(&mut context) };
    assert_eq!(inner.status, 0);
    drop_caught(inner.causes);
    unsafe { causes::record(&mut context, Cause::new(causes::UNWRAP, 10)) };
    let outer = unsafe { hirsel_core_isolate_catch(&mut context) };
    assert_eq!(outer.status, 0);
    assert!(
        context.primary.is_some(),
        "outer reservation must replace the used primary"
    );
    drop_caught(outer.causes);
    assert!(context.primary.as_ref().is_some_and(|cell| cell.reserved));
    drop(context.primary.take());
}

#[test]
fn cancelled_primary_is_refunded_and_rejoined_with_saved_chain() {
    let mut context = started(1024);
    assert!(unsafe { hirsel_core_isolate_enter(&mut context) }.entered);
    unsafe { causes::record(&mut context, Cause::new(causes::CANCELLED, 11)) };
    let caught = unsafe { hirsel_core_isolate_catch(&mut context) };
    assert_eq!((caught.causes.is_null(), caught.status), (true, 1));
    assert_eq!(
        context.causes.iter().next().unwrap().kind(),
        causes::CANCELLED
    );
}

#[test]
fn finalizer_uses_the_grant_for_the_reservation_and_transfers_its_cell() {
    let mut context = started(2048);
    let resource = unsafe { causes::resource_new(&context, 8, 72, 8, 8).unwrap() };
    unsafe {
        causes::finalizer_begin(&mut context, resource);
        let before = context.available();
        assert!(hirsel_core_isolate_enter(&mut context).entered);
        causes::record(&mut context, Cause::new(causes::UNWRAP, 12));
        let caught = hirsel_core_isolate_catch(&mut context);
        assert_eq!(caught.status, 0);
        drop_caught(caught.causes);
        assert_eq!(context.available(), before - 32);
        causes::finalizer_end(&mut context, resource);
        causes::resource_free(resource, 8);
    }
}

#[test]
fn finalizer_entry_refusal_is_a_capacity_value() {
    let mut context = started(1024);
    let resource = unsafe { causes::resource_new(&context, 8, 72, 8, 8).unwrap() };
    unsafe {
        causes::finalizer_begin(&mut context, resource);
        // SAFETY: the fixture owns the active finalizer grant exclusively.
        (*context.limit_record()).available = 31;
        let entered = hirsel_core_isolate_enter(&mut context);
        assert_eq!((entered.entered, entered.available), (false, 31));
        causes::finalizer_end(&mut context, resource);
        causes::resource_free(resource, 8);
    }
}
