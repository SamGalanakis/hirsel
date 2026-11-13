use crate::{
    Capacity, Context,
    block::Block,
    causes::{self, Cause, Causes},
};
use std::ptr::{self, NonNull};

fn cause(site: u64, kind: u8) -> Cause {
    if kind == causes::FAIL {
        Cause::fail(Default::default(), site)
    } else {
        Cause::new(kind, site)
    }
}
#[derive(Default)]
struct Trace {
    entries: [u8; 32],
    len: usize,
}
impl Trace {
    fn push(&mut self, id: u8) {
        self.entries[self.len] = id;
        self.len += 1;
    }
    fn slice(&self) -> &[u8] {
        &self.entries[..self.len]
    }
}
fn new(ctx: &Context) -> Result<NonNull<u8>, Capacity> {
    // SAFETY: all successful containers are ended through drop_one or destructure.
    unsafe { causes::resource_new(ctx, 8, 0, 8, 8) }
}
fn drop_one(ctx: &mut Context, p: NonNull<u8>, id: u8, fails: bool, trace: &mut Trace) {
    // SAFETY: these are emitter-style finalizer, field cleanup and container-free
    // steps. Failure abandons the body but does not skip finalizer_end or free.
    unsafe {
        causes::finalizer_begin(ctx, p);
        trace.push(id);
        if fails {
            causes::record(ctx, cause(id as u64, causes::FAIL));
        }
        causes::finalizer_end(ctx, p);
        causes::resource_free(p, 8);
    }
}

#[test]
fn all_exit_kinds_pending_results_masks_and_constructor_prefixes() {
    let mut runs = 0;
    for exit_kind in 0..7 {
        for mask in 0..16u8 {
            for later in [false, true] {
                for k in 1..=6 {
                    let mut ctx = Context::new(4096);
                    let root = ctx.current.clone();
                    root.inject(k, later);
                    if causes::task_start(&mut ctx).is_ok() {
                        let mut owners = Vec::new();
                        for _ in 0..4 {
                            match new(&ctx) {
                                Ok(p) => owners.push(p),
                                Err(_) => break,
                            }
                        }
                        let admissions = root.attempts();
                        let mut trace = Trace::default();
                        let initial = owners.len() == 4 && exit_kind >= 5;
                        if initial {
                            // SAFETY: a started task still owns its primary cell.
                            unsafe {
                                causes::record(
                                    &mut ctx,
                                    cause(
                                        0,
                                        if exit_kind == 6 {
                                            causes::CANCELLED
                                        } else {
                                            causes::OVERFLOW
                                        },
                                    ),
                                );
                            }
                        }
                        let expected;
                        if owners.len() == 4 {
                            let mut pending = Some(owners.pop().unwrap());
                            for id in (1..=3).rev() {
                                let fails = mask & (1 << (id - 1)) != 0;
                                drop_one(&mut ctx, owners.pop().unwrap(), id, fails, &mut trace);
                                if fails || initial {
                                    if let Some(p) = pending.take() {
                                        drop_one(&mut ctx, p, 4, mask & 8 != 0, &mut trace);
                                    }
                                }
                            }
                            if let Some(p) = pending {
                                drop_one(&mut ctx, p, 4, mask & 8 != 0, &mut trace);
                            }
                            expected = if initial || mask & 4 != 0 {
                                vec![3, 4, 2, 1]
                            } else if mask & 2 != 0 {
                                vec![3, 2, 4, 1]
                            } else {
                                vec![3, 2, 1, 4]
                            };
                        } else {
                            expected = (1..=owners.len() as u8).rev().collect();
                            for id in expected.iter().copied() {
                                drop_one(
                                    &mut ctx,
                                    owners.pop().unwrap(),
                                    id,
                                    mask & (1 << (id - 1)) != 0,
                                    &mut trace,
                                );
                            }
                        }
                        assert_eq!(trace.slice(), expected);
                        let expected_causes: Vec<_> = initial
                            .then_some(0)
                            .into_iter()
                            .chain(
                                expected
                                    .iter()
                                    .copied()
                                    .filter(|id| mask & (1 << (id - 1)) != 0)
                                    .map(u64::from),
                            )
                            .collect();
                        assert_eq!(
                            ctx.causes.iter().map(|c| c.site).collect::<Vec<_>>(),
                            expected_causes
                        );
                        assert_eq!(root.attempts(), admissions, "cleanup must not admit");
                    }
                    drop(ctx);
                    assert_eq!(root.available(), 4096);
                    runs += 1;
                }
            }
        }
    }
    assert_eq!(runs, 1344);
}

#[test]
fn replacement_installs_before_old_drop_and_refusal_keeps_old_owner() {
    let mut runs = 0;
    for old_fails in [false, true] {
        for later in [false, true] {
            for k in 1..=5 {
                let mut ctx = Context::new(2048);
                let root = ctx.current.clone();
                root.inject(k, later);
                if causes::task_start(&mut ctx).is_ok() {
                    if let Ok(old) = new(&ctx) {
                        // SAFETY: own resource storage holds a byte until it is freed.
                        unsafe {
                            old.as_ptr().write(11);
                        }
                        let mut slot = old;
                        let mut trace = Trace::default();
                        if let Ok(prepared) = new(&ctx) {
                            // SAFETY: initialize the replacement before installation.
                            unsafe {
                                prepared.as_ptr().write(22);
                            }
                            slot = prepared;
                            drop_one(&mut ctx, old, 1, old_fails, &mut trace);
                            // SAFETY: a failed displaced drop preserves the installed owner.
                            unsafe {
                                assert_eq!(slot.as_ptr().read(), 22);
                            }
                        } else {
                            // SAFETY: preparation refused, leaving the original value untouched.
                            unsafe {
                                assert_eq!(slot.as_ptr().read(), 11);
                            }
                        }
                        drop_one(&mut ctx, slot, 2, false, &mut trace);
                    }
                }
                drop(ctx);
                assert_eq!(root.available(), 2048);
                runs += 1;
            }
        }
    }
    assert_eq!(runs, 20);
}

#[test]
fn whole_destructure_refunds_container_cell_and_unused_grant_only() {
    let ctx = Context::new(1024);
    let field = Block::new(&ctx, Some(24), 24, 8).unwrap();
    // SAFETY: transfer the field out, then wholly destructure its resource container.
    unsafe {
        let resource = causes::resource_new(&ctx, 16, 256, 16, 8).unwrap();
        assert_eq!(ctx.available(), 696);
        causes::resource_free(resource, 16);
    }
    assert_eq!(ctx.available(), 1000);
    drop(field);
    assert_eq!(ctx.available(), 1024);
}

fn nested(ctx: &mut Context, depth: u8, grant: u64, trace: &mut Trace) -> bool {
    // SAFETY: nested resources are admitted from the active grant; each is
    // finalized and freed once, even after an admission or child finalizer failure.
    unsafe {
        if depth != 0 {
            let child_grant = grant.saturating_sub(40);
            match causes::resource_new(ctx, 8, child_grant, 8, 8) {
                Ok(child) => {
                    causes::finalizer_begin(ctx, child);
                    let failed = nested(ctx, depth - 1, child_grant, trace);
                    causes::finalizer_end(ctx, child);
                    causes::resource_free(child, 8);
                    if failed {
                        return true;
                    }
                }
                Err(_) => {
                    causes::record(ctx, cause(depth as u64, causes::CAPACITY));
                    return true;
                }
            }
        }
        trace.push(depth);
        false
    }
}

#[test]
fn nested_finalizers_subdivide_only_prepaid_grants_under_every_refusal() {
    let mut runs = 0;
    for depth in 0..=8 {
        for grant in [0, 40, 80, 160, 320] {
            for later in [false, true] {
                for k in 1..=12 {
                    let mut ctx = Context::new(8192);
                    let root = ctx.current.clone();
                    root.inject(k, later);
                    if causes::task_start(&mut ctx).is_ok() {
                        // SAFETY: the fixture cleans the successful resource on every path.
                        if let Ok(resource) = unsafe { causes::resource_new(&ctx, 8, grant, 8, 8) }
                        {
                            let mut trace = Trace::default();
                            // SAFETY: start and settle exactly this prepaid finalizer.
                            unsafe {
                                causes::finalizer_begin(&mut ctx, resource);
                            }
                            let failed = nested(&mut ctx, depth, grant, &mut trace);
                            // SAFETY: body returned or propagated grant failure; fields are empty.
                            unsafe {
                                causes::finalizer_end(&mut ctx, resource);
                                causes::resource_free(resource, 8);
                            }
                            assert!(ctx.causes.iter().all(|c| c.kind() == causes::CAPACITY));
                            assert!(ctx.causes.iter().count() <= 1);
                            if failed {
                                assert!(
                                    trace.slice().is_empty(),
                                    "failure abandons every active finalizer body"
                                );
                            }
                        }
                    }
                    drop(ctx);
                    assert_eq!(root.available(), 8192);
                    runs += 1;
                }
            }
        }
    }
    assert_eq!(runs, 1080);
}

#[test]
fn fail_message_and_joined_cells_outlive_the_task_and_keep_funding() {
    let mut child = Context::new(1024);
    let payer = child.current.clone();
    causes::task_start(&mut child).unwrap();
    let message = Block::new(&child, Some(24), 24, 8).unwrap().into_raw();
    // SAFETY: task primary takes sole custody of the String storage.
    unsafe {
        causes::record(
            &mut child,
            Cause::fail(
                causes::StringMessage {
                    data: message.as_ptr(),
                    len: 24,
                    capacity: 24,
                },
                7,
            ),
        );
    }
    let joined = std::mem::take(&mut child.causes).into_raw();
    drop(child);
    assert_eq!(payer.available(), 968);
    let mut parent = Context::new(1024);
    causes::task_start(&mut parent).unwrap();
    // SAFETY: join delivers the child's existing cells; parent's primary is independent.
    unsafe {
        causes::record(&mut parent, cause(9, causes::OVERFLOW));
    }
    // SAFETY: the join result transfers its complete owned chain to this owner.
    let joined = unsafe { Causes::from_raw(joined) };
    assert_eq!(joined.iter().map(|c| c.site).collect::<Vec<_>>(), vec![7]);
    assert_eq!(
        parent.causes.iter().map(|c| c.site).collect::<Vec<_>>(),
        vec![9]
    );
    drop(joined);
    assert_eq!(payer.available(), 1024);
}

#[test]
fn grant_growth_overrides_original_payer_and_hands_survivors_back() {
    let mut ctx = Context::new(1024);
    let original = Block::new(&ctx, Some(16), 16, 8).unwrap();
    // SAFETY: finalized once; the replacement is moved outward before grant settlement.
    unsafe {
        let resource = causes::resource_new(&ctx, 8, 64, 8, 8).unwrap();
        causes::finalizer_begin(&mut ctx, resource);
        let replacement = original.replacement(&ctx, Some(32), 32, 8).unwrap();
        assert_eq!(ctx.available(), 32);
        drop(original);
        assert_eq!(ctx.available(), 32);
        causes::finalizer_end(&mut ctx, resource);
        causes::resource_free(resource, 8);
        assert_eq!(ctx.available(), 992);
        drop(replacement);
    }
    assert_eq!(ctx.available(), 1024);
}

#[test]
fn bound_handover_keeps_cells_own_counted_and_region_bytes_within_parent() {
    use crate::{counted, region::Region};
    unsafe extern "C" fn empty(
        _: *mut Context,
        _: *mut u8,
        _: *mut u64,
        _: *mut counted::Worklist,
    ) -> bool {
        true
    }
    let mut runs = 0;
    for chunk in [128, 65536] {
        for later in [false, true] {
            for k in 1..=9 {
                let mut ctx = Context::new(2048);
                let root = ctx.current.clone();
                root.inject(k, later);
                let mut region = Region::new(chunk);
                if causes::task_start(&mut ctx).is_ok() && ctx.limit_enter(1024).is_ok() {
                    let own = Block::new(&ctx, Some(24), 24, 8).ok();
                    let node = Block::graph(&ctx, Some(16), 16, 8)
                        .ok()
                        .map(Block::into_raw);
                    if let Some(node) = node {
                        // SAFETY: initialize a newly admitted immutable counted node.
                        unsafe {
                            counted::initialize(node, false);
                        }
                    }
                    let _stable = region.allocate(&ctx, 13, 8, Some(16));
                    let resource = new(&ctx).ok();
                    if ctx.limit_enter(128).is_ok() {
                        ctx.limit_exit();
                    }
                    ctx.limit_exit();
                    assert!(root.available() <= 2048);
                    if let Some(p) = resource {
                        drop_one(&mut ctx, p, 1, true, &mut Trace::default());
                    }
                    if let Some(node) = node {
                        // SAFETY: transfer the one initialized holder after hand-over.
                        unsafe {
                            counted::release(&mut ctx, node, false, 16, empty);
                        }
                    }
                    drop(own);
                }
                drop(ctx); // region and used cells retain their original funding.
                drop(region);
                assert_eq!(root.available(), 2048);
                runs += 1;
            }
        }
    }
    assert_eq!(runs, 36);
}

#[test]
fn causes_and_resources_c_abi_preserve_complete_chain_and_container_refunds() {
    use crate::abi::*;
    // SAFETY: these steps reproduce generated root startup, failure propagation,
    // a failing resource drop, and chain delivery, with exact ownership transfers.
    unsafe {
        let ctx = hirsel_core_context_new(1024);
        let payer = (*ctx).current.clone();
        assert!(!hirsel_core_task_start(ctx).pointer.is_null());
        let resource = hirsel_core_resource_alloc(ctx, 8, 8, 8, 64);
        let pointer = NonNull::new(resource.pointer).unwrap();
        hirsel_core_raise(ctx, cause(1, causes::OVERFLOW));
        hirsel_core_finalizer_begin(ctx, pointer);
        let refusal = hirsel_core_alloc(
            ctx,
            ptr::null_mut(),
            128,
            8,
            128,
            false,
            crate::block::PLAIN,
        );
        assert!(refusal.pointer.is_null());
        assert_eq!(
            refusal.capacity,
            Capacity {
                requested: 128,
                available: 64
            }
        );
        hirsel_core_raise(ctx, cause(2, causes::CAPACITY));
        hirsel_core_finalizer_end(ctx, pointer);
        hirsel_core_resource_free(pointer, 8);
        let chain = hirsel_core_causes_take(ctx);
        assert_eq!((*hirsel_core_cause_value(chain)).site, 1);
        let next = hirsel_core_cause_next(chain);
        assert_eq!((*hirsel_core_cause_value(next)).site, 2);
        assert!(hirsel_core_cause_next(next).is_null());
        hirsel_core_context_free(ctx);
        assert_eq!(payer.available(), 960);
        hirsel_core_causes_drop(chain);
        assert_eq!(payer.available(), 1024);
    }
}

/// Bug: a refusal by a bound scope entered inside a finalizer is taken for the grant's
/// Capacity failure, though the bound is the current limit and refuses with a value
/// (MEM-19, MEM-25); only the grant itself refuses with a failure (MEM-27).
#[test]
fn inside_a_finalizer_only_the_grant_refuses_with_failure() {
    use crate::abi::{hirsel_core_alloc, hirsel_core_allocation_status};
    let mut ctx = Context::new(4096);
    // SAFETY: one resource, finalized and freed once; every allocation is refused.
    unsafe {
        let p = causes::resource_new(&ctx, 8, 32, 8, 8).unwrap();
        causes::finalizer_begin(&mut ctx, p);
        ctx.limit_enter(16).unwrap();
        let a = hirsel_core_alloc(&ctx, ptr::null_mut(), 8, 8, 24, false, crate::block::PLAIN);
        assert!(a.pointer.is_null());
        assert_eq!(hirsel_core_allocation_status(&ctx, a.pointer), 0);
        ctx.limit_exit();
        let b = hirsel_core_alloc(&ctx, ptr::null_mut(), 8, 8, 48, false, crate::block::PLAIN);
        assert!(b.pointer.is_null());
        assert_eq!(hirsel_core_allocation_status(&ctx, b.pointer), 1);
        causes::finalizer_end(&mut ctx, p);
        causes::resource_free(p, 8);
    }
}
