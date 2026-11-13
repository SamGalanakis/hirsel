use super::*;
use crate::{Context, block::Block};

struct Foreign(Vec<Block>);
// SAFETY: sole affine custody moves to the worker; its only operation is drop,
// which publishes refunds under the domain mutex without touching live records.
unsafe impl Send for Foreign {}
impl Foreign {
    fn release(self) {
        drop(self.0);
    }
}

#[test]
fn dirty_inbox_aggregates_bytes_and_zero_pins_among_many_clean_records() {
    let context = Context::new(64);
    let clean: Vec<_> = (0..512)
        .map(|_| context.current.reserve(0).unwrap())
        .collect();
    let foreign = Foreign(vec![
        Block::new(&context, Some(8), 8, 8).unwrap(),
        Block::new(&context, Some(8), 8, 8).unwrap(),
        Block::new(&context, Some(0), 1, 8).unwrap(),
    ]);
    std::thread::spawn(move || foreign.release())
        .join()
        .unwrap();
    assert_eq!(context.available(), 48);
    let domain = context.current.payer.domain();
    assert_eq!(domain.remote.lock().unwrap().queued_slots(), 1);
    context.current.drain();
    assert_eq!(context.available(), 64);
    assert!(!domain.remote.lock().unwrap().pending());
    assert_eq!(context.current.payer.forest().zeros, 0);
    drop(clean);
}

#[test]
fn owner_notifications_and_handover_preserve_the_next_owner_event() {
    let root = Funding::root(64);
    let child = root.reserve(32).unwrap();
    child.task_owner(1);
    let context = Context::with_funding(child);
    let root_signal = AtomicBool::new(false);
    let child_signal = AtomicBool::new(false);
    // SAFETY: subscriptions end before their pinned stack flags and live funding.
    let _root_subscription = unsafe { root.subscribe_refunds(NonNull::from(&root_signal)) };
    // SAFETY: same lifetime/custody rule; child uses owner 1 despite caller 0.
    let _child_subscription = unsafe {
        context
            .current
            .subscribe_refunds(NonNull::from(&child_signal))
    };
    drop(Block::new(&context, Some(8), 8, 8).unwrap());
    assert!(!root_signal.load(Ordering::Acquire));
    assert!(child_signal.load(Ordering::Acquire));
    root.drain();
    assert_eq!((root.available(), context.available()), (32, 24));
    context.current.end();
    assert_eq!(root.available(), 56); // Unused reservation returns at handover.
    assert!(root_signal.load(Ordering::Acquire));
    assert!(!child_signal.load(Ordering::Acquire));
    root.drain();
    assert_eq!(root.available(), 64);
    assert!(!root_signal.load(Ordering::Acquire));
}

#[test]
fn foreign_thread_routing_and_owner_reassignment_leave_no_stale_signal() {
    let root = Funding::root(64);
    let child = root.reserve(32).unwrap();
    child.task_owner(1);
    let context = Context::with_funding(child);
    let root_signal = AtomicBool::new(false);
    let child_signal = AtomicBool::new(false);
    // SAFETY: both flags stay pinned until their subscriptions leave scope.
    let _root_subscription = unsafe { root.subscribe_refunds(NonNull::from(&root_signal)) };
    // SAFETY: owner registration borrows the same live domain and pinned flag.
    let _child_subscription = unsafe {
        context
            .current
            .subscribe_refunds(NonNull::from(&child_signal))
    };
    let foreign = Foreign(vec![Block::new(&context, Some(8), 8, 8).unwrap()]);
    std::thread::spawn(move || foreign.release())
        .join()
        .unwrap();
    assert!(root_signal.load(Ordering::Acquire));
    root.drain(); // Routes the OS-thread publication, but cannot deliver owner 1.
    assert!(!root_signal.load(Ordering::Acquire));
    assert!(child_signal.load(Ordering::Acquire));
    assert_eq!(context.available(), 24);
    context.current.task_owner(0);
    assert_eq!(context.available(), 24); // Reassignment routes; it never refunds.
    assert!(root_signal.load(Ordering::Acquire));
    root.drain();
    assert_eq!(context.available(), 32);
    assert!(!root_signal.load(Ordering::Acquire));
    context.current.end();
    assert_eq!(root.available(), 64);
}

#[test]
fn finished_owner_inboxes_are_reclaimed_under_task_churn() {
    let root = Funding::root(64);
    for owner in 1..1000 {
        let child = root.reserve(0).unwrap();
        child.task_owner(owner);
        child.end();
        drop(child);
        root.drain_notified();
        assert_eq!(root.available(), 64);
        assert_eq!(root.payer.domain().remote.lock().unwrap().inbox_count(), 1);
    }
}
