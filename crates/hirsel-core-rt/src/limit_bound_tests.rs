use super::*;
use crate::{Context, block::Block};

fn snapshot(context: &Context) -> Vec<String> {
    let payer = context.current.payer;
    let d = payer.domain();
    let f = payer.forest();
    let slot_id = |p: Option<NonNull<Slot>>| p.map(|p| unsafe { p.as_ref().id });
    let mut rows = vec![format!(
        "current={} depth={} handles={} length={} free={:?} retired={} zeros={} pending={} attempts={} fail={:?}",
        payer.slot().id,
        context.bound_depth,
        d.handles.get(),
        f.length,
        slot_id(f.free),
        f.retired_bytes,
        f.zeros,
        d.pending.load(Ordering::Acquire),
        f.attempts,
        f.fail
    )];
    for id in 0..f.length {
        let s = f.slot(id);
        let r = f.record(id);
        rows.push(format!(
            "{id}: available={} outstanding={} refs={} parent={:?} rank={} owner={} capacity={} lexical={:?} inbox={:?} free={:?} occupied={} pending={} zeros={} next={:?}",
            r.available, r.outstanding, r.refs, r.parent, r.rank, r.live.owner, r.live.capacity,
            r.live.parent.map(|p| p.slot().id),
            r.occupied.then(|| unsafe { r.live.inbox.as_ref().debug_state() }),
            slot_id(r.next_free), r.occupied, s.pending.load(Ordering::Relaxed),
            s.pending_zeros.load(Ordering::Relaxed), slot_id(unsafe { *s.next_pending.get() })
        ));
    }
    rows.push(format!(
        "active={:?} reap={:?} remote={:?}",
        unsafe { d.active_inbox.get().as_ref().debug_state() },
        d.reap_inboxes
            .get()
            .map(|p| unsafe { p.as_ref().debug_state() }),
        d.remote.lock().unwrap().debug_state()
    ));
    rows
}

fn exit(context: &mut Context, general: bool) {
    if general {
        context.bound_depth -= 1;
        let r = unsafe { &*context.current.raw_local() };
        if r.refs == 1 && r.available == r.live.capacity {
            context.current.exit();
        } else {
            context.current.exit_general();
        }
    } else {
        context.limit_exit();
    }
}

#[test]
fn rank_zero_exit_matches_every_general_record_pin_rank_and_inbox() {
    fn run(general: bool, n: usize, zero: bool, nested: bool, foreign: bool) -> Vec<Vec<String>> {
        let mut ctx = Context::new(8192);
        let outer = Block::new(&ctx, Some(16), 16, 8).unwrap();
        ctx.limit_enter(4096).unwrap();
        let mut blocks: Vec<_> = (0..n)
            .map(|_| Block::new(&ctx, Some(8), 8, 8).unwrap())
            .collect();
        let zero = zero.then(|| Block::new(&ctx, Some(0), 1, 8).unwrap());
        if nested {
            ctx.limit_enter(1024).unwrap();
            blocks.push(Block::new(&ctx, Some(8), 8, 8).unwrap());
            exit(&mut ctx, general);
        }
        if foreign {
            ctx.current.task_owner(7);
        }
        let mut states = vec![snapshot(&ctx)];
        exit(&mut ctx, general);
        states.push(snapshot(&ctx));
        // Grow while a different bound is current. Admission belongs to the survivor's live payer.
        ctx.limit_enter(512).unwrap();
        if let Some(block) = blocks.first() {
            let replacement = block.replacement(&ctx, Some(24), 24, 8).unwrap();
            states.push(snapshot(&ctx));
            drop(replacement);
        }
        exit(&mut ctx, general);
        states.push(snapshot(&ctx));
        drop(blocks);
        drop(zero);
        ctx.current.drain();
        states.push(snapshot(&ctx));
        drop(outer);
        states.push(snapshot(&ctx));
        states
    }
    for n in [0, 1, 2, 8, 128] {
        for zero in [false, true] {
            for nested in [false, true] {
                for foreign in [false, true] {
                    assert_eq!(
                        run(false, n, zero, nested, foreign),
                        run(true, n, zero, nested, foreign),
                        "n={n} zero={zero} nested={nested} foreign={foreign}"
                    );
                }
            }
        }
    }
}

#[test]
fn charged_parent_boundary_and_outward_growth_keep_exact_room() {
    let mut ctx = Context::new(128);
    let outer = Block::new(&ctx, Some(16), 16, 8).unwrap();
    ctx.limit_enter(64).unwrap();
    let survivor = Block::new(&ctx, Some(8), 8, 8).unwrap();
    assert_eq!(ctx.available(), 56);
    ctx.limit_exit();
    assert_eq!(ctx.available(), 104);
    drop(survivor);
    assert_eq!(ctx.available(), 112);
    drop(outer);
    assert_eq!(ctx.available(), 128);
}

struct Foreign(Block);
// SAFETY: exclusive block custody transfers to a thread that only enqueues its refund.
unsafe impl Send for Foreign {}
impl Foreign {
    fn release(self) {
        drop(self.0);
    }
}

#[test]
fn settlement_resolves_again_after_owner_event_and_outer_rank_handover() {
    fn run(general: bool, publish_before_exit: bool) -> Vec<Vec<String>> {
        let mut ctx = Context::new(1024);
        ctx.limit_enter(768).unwrap();
        ctx.limit_enter(512).unwrap();
        ctx.limit_enter(256).unwrap();
        let kept = Block::new(&ctx, Some(8), 8, 8).unwrap();
        let sent = Foreign(Block::new(&ctx, Some(16), 16, 8).unwrap());
        // First lookup resolves to the inner scope. It cannot be retained across the next events.
        let _ = kept.payer().canonical();
        if publish_before_exit {
            std::thread::spawn(move || sent.release()).join().unwrap();
        } else {
            exit(&mut ctx, general);
            std::thread::spawn(move || sent.release()).join().unwrap();
        }
        let mut states = vec![snapshot(&ctx)];
        ctx.current.drain();
        states.push(snapshot(&ctx));
        if publish_before_exit {
            exit(&mut ctx, general);
        }
        exit(&mut ctx, general);
        exit(&mut ctx, general); // repeated outward handover keeps the rank winner
        states.push(snapshot(&ctx));
        let grown = kept.replacement(&ctx, Some(32), 32, 8).unwrap();
        states.push(snapshot(&ctx));
        drop(kept);
        drop(grown);
        states.push(snapshot(&ctx));
        assert_eq!(ctx.available(), 1024);
        states
    }
    for before in [false, true] {
        assert_eq!(run(false, before), run(true, before));
    }
}

#[test]
fn forwarded_lexical_parent_uses_the_unchanged_general_union() {
    fn run(general: bool) -> Vec<Vec<String>> {
        let root = Funding::root(1024);
        let outer = root.reserve(512).unwrap();
        let child = outer.reserve(256).unwrap();
        outer.end();
        let mut ctx = Context::with_funding(child);
        ctx.bound_depth = 1;
        let kept = Block::new(&ctx, Some(8), 8, 8).unwrap();
        let mut states = vec![snapshot(&ctx)];
        exit(&mut ctx, general);
        states.push(snapshot(&ctx));
        drop(kept);
        states.push(snapshot(&ctx));
        assert_eq!(root.available(), 1024);
        states
    }
    assert_eq!(run(false), run(true));
}
