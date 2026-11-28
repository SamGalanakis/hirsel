use crate::{
    Context,
    abi::hirsel_core_release_buffer_enqueue,
    abi_layout::{FIXED_POINT_NO_INDEX, NODE_PAYLOAD, fixed_point},
    block::{self, Block, Descriptor, Point},
    counted::{self, Worklist},
};
use std::ptr::{self, NonNull};

/// A descriptor with its points inline, as the emitters lay it out.
#[repr(C)]
pub(crate) struct Fixed<const N: usize> {
    pub(crate) descriptor: Descriptor,
    pub(crate) points: [Point; N],
}

impl<const N: usize> Fixed<N> {
    /// Pointer to the leading descriptor with provenance covering the entire
    /// `Fixed<N>` (including inline trailing `points`).
    pub(crate) const fn as_ptr(&self) -> *const Descriptor {
        (self as *const Self).cast()
    }
}

/// Point `k` (1-based) of a counted fixture: no array index.
pub(crate) const fn at(k: u64, cursor: u64, a: u64, b: u64) -> Point {
    Point {
        back: fixed_point(k) as u64,
        cursor,
        a,
        b,
        index: FIXED_POINT_NO_INDEX,
    }
}

unsafe extern "C" fn empty(_: *mut Context, _: *mut u8, _: *mut u64, _: *mut Worklist) -> bool {
    true
}

/// A counted 16-byte node whose payload has nothing to clean.
pub(crate) static LEAF: Fixed<0> = Fixed {
    descriptor: Descriptor {
        recipe: empty,
        charge: 16,
        payload_offset: NODE_PAYLOAD as u64,
    },
    points: [],
};

/// Release one holder of a LEAF node, as emitted glue does.
/// # Safety
/// Transfers one live holder under the Rc/Arc event discipline.
pub(crate) unsafe fn release_leaf(context: *mut Context, node: NonNull<u8>, atomic: bool) {
    // SAFETY: the caller transfers a live holder; the last one owns the node.
    unsafe {
        if counted::decrement(node, atomic) {
            crate::abi::hirsel_core_release(context, node, LEAF.as_ptr(), 0);
        }
    }
}

/// `{next: Rc[Self]}`: the vacated slot and one spare word hold the frame.
static CHAIN: Fixed<1> = Fixed {
    descriptor: Descriptor {
        recipe: chain,
        charge: 16,
        payload_offset: NODE_PAYLOAD as u64,
    },
    points: [at(1, 1, 0, 8)],
};
unsafe extern "C" fn chain(
    _: *mut Context,
    payload: *mut u8,
    cursor: *mut u64,
    work: *mut Worklist,
) -> bool {
    // SAFETY: the payload's first word is one child holder or null.
    unsafe {
        if *cursor == 0 {
            *cursor = 1;
            if let Some(child) = NonNull::new(payload.cast::<*mut u8>().read())
                && counted::decrement(child, false)
            {
                (*work).stage_fixed(child, CHAIN.as_ptr(), 1);
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
    let count = if cfg!(miri) { 256 } else { 100_000 };
    for _ in 0..count {
        // Count, one child word and the spare: 24 native bytes, 16 charged.
        let node = block::allocate(&ctx, ctx.payer(), Some(16), 24, 8, block::PLAIN).unwrap();
        // SAFETY: a freshly allocated count/payload pair receives its sole child.
        unsafe {
            counted::initialize(node, false);
            node.as_ptr().add(8).cast::<*mut u8>().write(head);
        }
        head = node.as_ptr();
    }
    ctx.current.inject(1, true);
    // SAFETY: the fixture transfers the only root holder; chain hands children over.
    unsafe {
        let node = NonNull::new(head).unwrap();
        assert!(counted::decrement(node, false));
        crate::abi::hirsel_core_release(&mut ctx, node, CHAIN.as_ptr(), 0);
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
                    release_leaf(&mut ctx, node, atomic);
                }
                assert_eq!(counted::try_unwrap(node, atomic), release_first);
                if !release_first {
                    release_leaf(&mut ctx, node, atomic);
                    assert!(counted::try_unwrap(node, atomic));
                }
                block::free(node, 16);
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
                match Block::new(&ctx, Some(16), 16, 8) {
                    Ok(node) => {
                        let node = node.into_raw();
                        // SAFETY: new node with one holder and no initialized children.
                        unsafe {
                            counted::initialize(node, false);
                            release_leaf(&mut ctx, node, false);
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
            unsafe { release_leaf(&mut child, self.0, true) };
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
    payer.drain(); // Provider applies the ordered remote-refund event before resume.
    assert_eq!(payer.available(), 64);
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
        spare: u64,
    }
    // Right first, then left (FAIL-8). After right: its slot and the spare;
    // after left: both vacated slots. `id` stays live: the trace reads it.
    static TREE: Fixed<2> = Fixed {
        descriptor: Descriptor {
            recipe: tree,
            charge: 32,
            payload_offset: NODE_PAYLOAD as u64,
        },
        points: [at(1, 1, 16, 24), at(2, 2, 8, 16)],
    };
    unsafe extern "C" fn tree(
        ctx: *mut Context,
        p: *mut u8,
        cursor: *mut u64,
        list: *mut Worklist,
    ) -> bool {
        // SAFETY: each fixture payload contains two live child holders or null;
        // the drain restores the cursor from the point record.
        unsafe {
            let p = &*p.cast::<Payload>();
            while *cursor < 2 {
                let child = if *cursor == 0 { p.right } else { p.left };
                *cursor += 1;
                if let Some(child) = NonNull::new(child)
                    && counted::decrement(child, false)
                {
                    (*list).stage_fixed(child, TREE.as_ptr(), *cursor);
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
        let node = block::allocate(&ctx, ctx.payer(), Some(32), 40, 8, block::PLAIN).unwrap();
        // SAFETY: fresh node receives its count and completely initialized payload.
        unsafe {
            counted::initialize(node, false);
            node.as_ptr().add(8).cast::<Payload>().write(Payload {
                id,
                left,
                right,
                spare: 0,
            });
        }
        node.as_ptr()
    };
    let four = make(4, ptr::null_mut(), ptr::null_mut());
    let two = make(2, four, ptr::null_mut());
    let three = make(3, ptr::null_mut(), ptr::null_mut());
    let one = make(1, two, three);
    // SAFETY: the sole root holder owns this complete acyclic tree.
    unsafe {
        let one = NonNull::new(one).unwrap();
        assert!(counted::decrement(one, false));
        crate::abi::hirsel_core_release(&mut ctx, one, TREE.as_ptr(), 0);
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
            block::free(node, 16);
        }
        assert_eq!(ctx.available(), 16);
    }
}

#[test]
fn remote_refunds_wait_for_the_ordered_handoff_while_local_admission_runs() {
    struct Nodes(Vec<NonNull<u8>>);
    // SAFETY: all nodes contain immutable u64 data; only their atomic counts and
    // prepaid remote-mailbox slots are touched by the receiving task.
    unsafe impl Send for Nodes {}
    impl Nodes {
        fn release(self) {
            let mut remote = Context::new(0);
            for node in self.0 {
                // SAFETY: one transferred Arc holder, with no descendants.
                unsafe { release_leaf(&mut remote, node, true) };
            }
        }
    }
    let mut ctx = Context::new(4096);
    ctx.limit_enter(3200).unwrap();
    let mut nodes = Nodes(Vec::new());
    for _ in 0..200 {
        let node = Block::new(&ctx, Some(16), 16, 8).unwrap().into_raw();
        // SAFETY: this task initializes each fresh single-holder Arc.
        unsafe {
            counted::initialize(node, true);
        }
        nodes.0.push(node);
    }
    ctx.limit_exit();
    let worker = std::thread::spawn(move || nodes.release());
    for _ in 0..1000 {
        let local = Block::new(&ctx, Some(8), 8, 8).unwrap();
        assert_eq!(ctx.available(), 888);
        drop(local);
    }
    worker.join().unwrap();
    assert_eq!(ctx.available(), 896);
    ctx.current.inject(1, true);
    ctx.current.drain();
    assert_eq!(ctx.available(), 4096);
    assert_eq!(ctx.current.attempts(), 0);
}

/// `{kids: List[Rc[Self]]}`: pointer and length stay live while the buffer
/// drains, so the frame takes the capacity word and one spare.
static HOLDER: Fixed<1> = Fixed {
    descriptor: Descriptor {
        recipe: holder,
        charge: 24,
        payload_offset: NODE_PAYLOAD as u64,
    },
    points: [at(1, 1, 16, 24)],
};
unsafe extern "C" fn holder(
    _: *mut Context,
    payload: *mut u8,
    cursor: *mut u64,
    work: *mut Worklist,
) -> bool {
    // SAFETY: the payload is a {data, len, capacity} carrier of child holders.
    unsafe {
        if *cursor == 0 {
            *cursor = 1;
            let capacity = payload.add(16).cast::<u64>().read();
            if capacity != 0 {
                let data = NonNull::new_unchecked(payload.cast::<*mut u8>().read());
                hirsel_core_release_buffer_enqueue(work, data, capacity * 8, payload, kids, 1);
                return false;
            }
        }
        true
    }
}
unsafe extern "C" fn kids(
    _: *mut Context,
    carrier: *mut u8,
    cursor: *mut u64,
    work: *mut Worklist,
) -> bool {
    // SAFETY: the parent pins its carrier; each element is one child holder.
    unsafe {
        let data = carrier.cast::<*const *mut u8>().read();
        let len = carrier.add(8).cast::<u64>().read();
        while *cursor < len {
            let child = NonNull::new_unchecked(data.add(*cursor as usize).read());
            *cursor += 1;
            if counted::decrement(child, false) {
                (*work).stage_fixed(child, HOLDER.as_ptr(), 0);
                return false;
            }
        }
        true
    }
}

#[test]
fn fixed_frames_and_buffer_frames_share_one_nonrecursive_worklist() {
    let mut ctx = Context::new(8_000_000);
    let mut head: Option<NonNull<u8>> = None;
    let depth = if cfg!(miri) { 256 } else { 100_000 };
    for _ in 0..depth {
        let node = block::allocate(&ctx, ctx.payer(), Some(24), 40, 8, block::PLAIN).unwrap();
        // SAFETY: a fresh counted holder receives a one-element GRAPH buffer
        // holding the previous node, or an empty static carrier.
        unsafe {
            counted::initialize(node, false);
            let carrier = node.as_ptr().add(NODE_PAYLOAD).cast::<u64>();
            if let Some(child) = head {
                let data = block::allocate(&ctx, ctx.payer(), Some(8), 8, 8, block::GRAPH).unwrap();
                data.as_ptr().cast::<*mut u8>().write(child.as_ptr());
                carrier.cast::<*mut u8>().write(data.as_ptr());
                carrier.add(1).write(1);
                carrier.add(2).write(1);
            } else {
                carrier
                    .cast::<*mut u8>()
                    .write(ptr::without_provenance_mut(8));
                carrier.add(1).write(0);
                carrier.add(2).write(0);
            }
        }
        head = Some(node);
    }
    ctx.current.inject(1, true);
    // SAFETY: the only root holder is transferred.
    unsafe {
        let node = head.unwrap();
        assert!(counted::decrement(node, false));
        crate::abi::hirsel_core_release(&mut ctx, node, HOLDER.as_ptr(), 0);
    }
    assert_eq!(ctx.available(), 8_000_000);
    assert_eq!(ctx.current.attempts(), 0);
}

/// `{next: Rc[Self], id}` whose only yield is its last step: the point says
/// FIXED_POINT_DONE, so the block is freed when its child is done and its glue
/// never runs again (it would read the words the frame vacated).
static LAST: Fixed<1> = Fixed {
    descriptor: Descriptor {
        recipe: last,
        charge: 16,
        payload_offset: NODE_PAYLOAD as u64,
    },
    points: [at(1, crate::abi_layout::FIXED_POINT_DONE, 0, 8)],
};
unsafe extern "C" fn last(
    _: *mut Context,
    payload: *mut u8,
    cursor: *mut u64,
    work: *mut Worklist,
) -> bool {
    // SAFETY: the payload holds one child holder or null, then the node's id.
    unsafe {
        assert_eq!(*cursor, 0, "a finished block's glue ran again");
        // The deepest node's finalizer fails; cleanup goes on (FAIL-10).
        if payload.add(8).cast::<u64>().read() == 0 {
            (*work).fail();
        }
        match NonNull::new(payload.cast::<*mut u8>().read()) {
            Some(child) if counted::decrement(child, false) => {
                (*work).stage_fixed(child, LAST.as_ptr(), 1);
                false
            }
            _ => true,
        }
    }
}

#[test]
fn finished_points_free_without_resuming_and_deep_failures_reach_the_root() {
    // Deeper than any native bound a drain may use, short of a stack overflow.
    let mut ctx = Context::new(1_600);
    let mut head = ptr::null_mut::<u8>();
    for id in 0..100u64 {
        let node = block::allocate(&ctx, ctx.payer(), Some(16), 24, 8, block::PLAIN).unwrap();
        // SAFETY: a fresh node receives its count, child and id.
        unsafe {
            counted::initialize(node, false);
            node.as_ptr().add(8).cast::<*mut u8>().write(head);
            node.as_ptr().add(16).cast::<u64>().write(id);
        }
        head = node.as_ptr();
    }
    // SAFETY: the sole root holder owns the whole chain.
    let failed = unsafe {
        let node = NonNull::new(head).unwrap();
        assert!(counted::decrement(node, false));
        counted::drain(&mut ctx, node, LAST.as_ptr(), 0)
    };
    assert!(failed);
    assert_eq!(ctx.available(), 1_600);
}
