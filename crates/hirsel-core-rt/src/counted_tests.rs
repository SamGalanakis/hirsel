use crate::{
    Context,
    block::Block,
    counted::{self, Worklist},
};
use std::ptr::{self, NonNull};

unsafe extern "C" fn empty(_: *mut Context, _: *mut u8, _: *mut u64, _: *mut Worklist) -> bool {
    true
}

unsafe extern "C" fn chain(
    _: *mut Context,
    payload: *mut u8,
    cursor: *mut u64,
    work: *mut Worklist,
) -> bool {
    // SAFETY: this fixture's payload is one initialized child pointer; a child
    // holder transfers to the intrusive worklist rather than recursive release.
    unsafe {
        if *cursor == 0 {
            *cursor = 1;
            if let Some(child) = NonNull::new(payload.cast::<*mut u8>().read()) {
                counted::enqueue(&mut *work, child, false, 16, chain);
                return false;
            }
        }
        true
    }
}

#[test]
fn counted_chain_releases_with_constant_stack_and_no_admission() {
    let mut ctx = Context::new(2_000_000);
    let mut head = ptr::null_mut();
    for _ in 0..100_000 {
        let node = Block::new(&ctx, Some(16), 16, 8).unwrap().into_raw();
        // SAFETY: a freshly allocated count/payload pair receives its sole child.
        unsafe {
            counted::initialize(node, false);
            node.as_ptr().add(8).cast::<*mut u8>().write(head);
        }
        head = node.as_ptr();
    }
    ctx.current.inject(1, true);
    // SAFETY: the fixture transfers the only root holder; chain enqueues children.
    unsafe {
        counted::release(&mut ctx, NonNull::new(head).unwrap(), false, 16, chain);
    }
    assert_eq!(ctx.available(), 2_000_000);
    assert_eq!(ctx.current.attempts(), 0);
}

#[test]
fn exact_rc_and_arc_share_release_and_both_unwrap_event_orders() {
    let mut runs = 0;
    for atomic in [false, true] {
        for release_first in [false, true] {
            let mut ctx = Context::new(64);
            let node = Block::new(&ctx, Some(16), 16, 8).unwrap().into_raw();
            // SAFETY: the two explicit holders follow each prescribed event order.
            unsafe {
                counted::initialize(node, atomic);
                counted::share(node, atomic);
                if release_first {
                    counted::release(&mut ctx, node, atomic, 16, empty);
                }
                assert_eq!(counted::try_unwrap(node, atomic), release_first);
                if !release_first {
                    counted::release(&mut ctx, node, atomic, 16, empty);
                    assert!(counted::try_unwrap(node, atomic));
                }
                crate::block::free(node, 16);
            }
            assert_eq!(ctx.available(), 64);
            runs += 1;
        }
    }
    assert_eq!(runs, 4);
}

#[test]
fn counted_creation_refusals_preserve_source_and_descendant_charges() {
    let mut runs = 0;
    for later in [false, true] {
        for k in 1..=4 {
            let mut ctx = Context::new(128);
            ctx.current.inject(k, later);
            if let Ok(source) = Block::new(&ctx, Some(24), 24, 8) {
                let before = ctx.available();
                let node = Block::new(&ctx, Some(16), 16, 8);
                match node {
                    Ok(node) => {
                        let node = node.into_raw();
                        // SAFETY: new node with one holder and no initialized children.
                        unsafe {
                            counted::initialize(node, false);
                            counted::release(&mut ctx, node, false, 16, empty);
                        }
                    }
                    Err(_) => assert_eq!(ctx.available(), before),
                }
                drop(source);
            }
            assert_eq!(ctx.available(), 128);
            runs += 1;
        }
    }
    assert_eq!(runs, 8);
}

#[test]
fn arc_last_release_on_another_task_refunds_the_original_domain() {
    struct Holder(NonNull<u8>);
    // SAFETY: this holder contains only a task-sendable immutable u64 payload;
    // Arc synchronizes its count and the funding forest synchronizes refunds.
    unsafe impl Send for Holder {}
    impl Holder {
        fn release(self) {
            let mut child = Context::new(0);
            // SAFETY: the moved holder is live; its payload has no descendants.
            unsafe {
                counted::release(&mut child, self.0, true, 16, empty);
            }
        }
    }
    let mut ctx = Context::new(64);
    let payer = ctx.current.clone();
    ctx.limit_enter(32).unwrap();
    let node = Block::new(&ctx, Some(16), 16, 8).unwrap().into_raw();
    // SAFETY: initialize a single live holder before moving it to another task.
    unsafe {
        counted::initialize(node, true);
    }
    ctx.limit_exit();
    drop(ctx);
    let holder = Holder(node);
    std::thread::spawn(move || holder.release()).join().unwrap();
    assert_eq!(payer.available(), 64);
}

#[test]
fn counted_c_abi_drops_the_last_holder_only() {
    use crate::abi::*;
    // SAFETY: C fixtures own two holders, releasing each exactly once.
    unsafe {
        let ctx = hirsel_core_context_new(64);
        let allocation = hirsel_core_alloc(ctx, ptr::null_mut(), 16, 8, 16, false);
        let node = NonNull::new(allocation.pointer).unwrap();
        hirsel_core_counted_init(node, false);
        hirsel_core_share(node, false);
        assert!(!hirsel_core_try_unwrap(node, false));
        hirsel_core_release(ctx, node, false, 16, empty);
        assert!(hirsel_core_try_unwrap(node, false));
        hirsel_core_release(ctx, node, false, 16, empty);
        assert_eq!((*ctx).available(), 64);
        hirsel_core_context_free(ctx);
    }
}

#[test]
fn resumed_drop_glue_cleans_descendants_before_siblings_and_parent_refunds() {
    use std::cell::RefCell;
    thread_local! { static TRACE: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) }; }
    #[repr(C)]
    struct Payload {
        id: u64,
        left: *mut u8,
        right: *mut u8,
    }
    unsafe extern "C" fn tree(
        ctx: *mut Context,
        p: *mut u8,
        cursor: *mut u64,
        list: *mut Worklist,
    ) -> bool {
        // SAFETY: each fixture payload contains two live child holders or null;
        // cursor records which field has completed across the trampoline.
        unsafe {
            let p = &*p.cast::<Payload>();
            while *cursor < 2 {
                let child = if *cursor == 0 { p.right } else { p.left };
                *cursor += 1;
                if let Some(child) = NonNull::new(child) {
                    counted::enqueue(&mut *list, child, false, 32, tree);
                    return false;
                }
            }
            TRACE.with(|t| {
                let mut t = t.borrow_mut();
                assert_eq!((*ctx).available(), t.len() as u64 * 32);
                t.push(p.id);
            });
            true
        }
    }
    TRACE.with(|t| *t.borrow_mut() = Vec::with_capacity(4));
    let mut ctx = Context::new(128);
    let make = |id, left, right| {
        let node = Block::new(&ctx, Some(32), 32, 8).unwrap().into_raw();
        // SAFETY: fresh node receives its count and completely initialized payload.
        unsafe {
            counted::initialize(node, false);
            node.as_ptr()
                .add(8)
                .cast::<Payload>()
                .write(Payload { id, left, right });
        }
        node.as_ptr()
    };
    let four = make(4, ptr::null_mut(), ptr::null_mut());
    let two = make(2, four, ptr::null_mut());
    let three = make(3, ptr::null_mut(), ptr::null_mut());
    let one = make(1, two, three);
    // SAFETY: the sole root holder owns this complete acyclic tree.
    unsafe {
        counted::release(&mut ctx, NonNull::new(one).unwrap(), false, 32, tree);
    }
    TRACE.with(|t| assert_eq!(*t.borrow(), [3, 4, 2, 1]));
    assert_eq!(ctx.available(), 128);
}

#[test]
fn full_u64_count_is_representable_without_immortalizing_the_node() {
    for atomic in [false, true] {
        let ctx = Context::new(16);
        let node = Block::new(&ctx, Some(16), 16, 8).unwrap().into_raw();
        // SAFETY: symbolic boundary fixture supplies a live count, then returns
        // its synthetic holders before freeing the otherwise empty node.
        unsafe {
            counted::initialize(node, atomic);
            node.cast::<u64>().as_ptr().write(u64::MAX - 1);
            counted::share(node, atomic);
            assert_eq!(node.cast::<u64>().as_ptr().read(), u64::MAX);
            assert!(!counted::try_unwrap(node, atomic));
            crate::block::free(node, 16);
        }
        assert_eq!(ctx.available(), 16);
    }
}
