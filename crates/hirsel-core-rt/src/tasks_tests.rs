use crate::{Context, abi::*, block::Block, causes, channel::*, tasks::*};
use std::ptr;
unsafe extern "C" fn inert(_: *mut Context, _: *mut u8) -> u8 {
    0
}
fn value<T>(logical: u64) -> ValueLayout {
    ValueLayout {
        bytes: size_of::<T>(),
        align: align_of::<T>(),
        logical,
        drop: inert,
    }
}
fn layout<X, R>(result: u64) -> TaskLayout {
    TaskLayout {
        input: value::<X>(0),
        result: value::<R>(result),
    }
}
fn run<X: Copy>(body: Body, mut input: X) -> (u64, Vec<Event>) {
    let mut events = Vec::<Event>::new();
    unsafe extern "C" fn event(state: *mut u8, event: Event) {
        // SAFETY: run pins this exclusively accessed test event vector.
        unsafe {
            (&mut *state.cast::<Vec<Event>>()).push(event);
        }
    }
    let mut context = Context::new(1 << 20);
    causes::task_start(&mut context).unwrap();
    let available = context.available();
    let mut result = 0u64;
    // SAFETY: root thunk owns input/destination; host event callback only records.
    unsafe {
        assert_eq!(
            run_root(
                &mut context,
                body,
                (&mut input as *mut X).cast(),
                (&mut result as *mut u64).cast(),
                size_of::<X>(),
                8,
                SchedulerProvider {
                    state: (&mut events as *mut Vec<Event>).cast(),
                    event: Some(event),
                    choose: None
                }
            ),
            0
        );
    }
    assert_eq!(
        context.available(),
        available,
        "every task/queue/result charge returned"
    );
    (result, events)
}
unsafe extern "C" fn add(_: *mut Context, destination: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: this task's X/R are u64.
    unsafe {
        destination
            .cast::<u64>()
            .write(input.cast::<u64>().read() + 1);
    }
    0
}
unsafe extern "C" fn spawn_join(context: *mut Context, destination: *mut u8, _: *mut u8) -> u8 {
    // SAFETY: owns every task handle and result slot on this root stack.
    unsafe {
        let mut input = 41u64;
        let before = (*context).available();
        let h = hirsel_core_spawn(
            context,
            0,
            (&mut input as *mut u64).cast(),
            add,
            layout::<u64, u64>(8),
        );
        assert!(!h.task.is_null());
        assert_eq!((*context).available(), before - 296);
        (*context).current.inject(1, true);
        let joined = hirsel_core_join(context, h.task, destination, 1);
        assert_eq!((joined.kind, joined.status), (0, 0));
        assert!(joined.causes.is_null());
        assert_eq!((*context).current.attempts(), 0);
        assert_eq!((*context).available(), before);
        let refused = hirsel_core_spawn(
            context,
            0,
            (&mut input as *mut u64).cast(),
            add,
            layout::<u64, u64>(8),
        );
        assert!(refused.task.is_null());
        assert_eq!(input, 41);
    }
    0
}
#[test]
fn spawn_admits_once_join_admits_nothing_and_refusal_returns_input() {
    assert_eq!(run(spawn_join, ()).0, 42);
}
#[derive(Clone, Copy)]
struct Producer {
    tx: *mut Channel,
    n: u64,
}
#[derive(Clone, Copy)]
struct Relay {
    rx: *mut Channel,
    tx: *mut Channel,
}
unsafe extern "C" fn producer(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: input owns its sender; each published scalar transfers exactly once.
    unsafe {
        let input = input.cast::<Producer>().read();
        for mut n in 0..input.n {
            let sent = hirsel_core_send(context, input.tx, (&mut n as *mut u64).cast(), 10);
            assert_eq!(sent.status, 0);
            assert!(sent.transferred);
        }
        hirsel_core_sender_drop(input.tx);
    }
    0
}
unsafe extern "C" fn relay(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: input owns one receiver and sender, both cleaned before return.
    unsafe {
        let input = input.cast::<Relay>().read();
        loop {
            let mut n = 0u64;
            let got = hirsel_core_receive(context, input.rx, (&mut n as *mut u64).cast(), 11);
            assert_eq!(got.status, 0);
            if !got.transferred {
                break;
            }
            let sent = hirsel_core_send(context, input.tx, (&mut n as *mut u64).cast(), 12);
            assert!(sent.transferred);
            assert_eq!(sent.status, 0);
        }
        assert_eq!(hirsel_core_receiver_drop(context, input.rx), 0);
        hirsel_core_sender_drop(input.tx);
    }
    0
}
unsafe extern "C" fn consumer(context: *mut Context, destination: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: input owns its receiver and destination is u64.
    unsafe {
        let rx = input.cast::<*mut Channel>().read();
        let mut sum = 0u64;
        let mut expected = 0u64;
        loop {
            let mut n = 0u64;
            let got = hirsel_core_receive(context, rx, (&mut n as *mut u64).cast(), 13);
            assert_eq!(got.status, 0);
            if !got.transferred {
                break;
            }
            assert_eq!(n, expected);
            expected += 1;
            sum += n;
        }
        destination.cast::<u64>().write(sum);
        assert_eq!(hirsel_core_receiver_drop(context, rx), 0);
    }
    0
}
unsafe extern "C" fn pipeline(context: *mut Context, destination: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: channels transfer into three child inputs; handles stay in root.
    unsafe {
        let n = input.cast::<u64>().read();
        let a = hirsel_core_channel_new(context, 1, value::<u64>(8), 1);
        let b = hirsel_core_channel_new(context, 1, value::<u64>(8), 2);
        let mut p = Producer { tx: a.sender, n };
        let mut r = Relay {
            rx: a.receiver,
            tx: b.sender,
        };
        let mut c = b.receiver;
        let producer = hirsel_core_spawn(
            context,
            1024,
            (&mut p as *mut Producer).cast(),
            producer,
            layout::<Producer, ()>(0),
        );
        let relay = hirsel_core_spawn(
            context,
            1024,
            (&mut r as *mut Relay).cast(),
            relay,
            layout::<Relay, ()>(0),
        );
        let consumer = hirsel_core_spawn(
            context,
            1024,
            (&mut c as *mut *mut Channel).cast(),
            consumer,
            layout::<*mut Channel, u64>(8),
        );
        assert!(!producer.task.is_null() && !relay.task.is_null() && !consumer.task.is_null());
        let mut unit = 0u8;
        assert_eq!(
            hirsel_core_join(context, producer.task, &mut unit, 20).kind,
            0
        );
        assert_eq!(hirsel_core_join(context, relay.task, &mut unit, 21).kind, 0);
        assert_eq!(
            hirsel_core_join(context, consumer.task, destination, 22).kind,
            0
        );
    }
    0
}
#[test]
fn three_tasks_backpressure_fifo_and_closure_return_every_charge() {
    for n in [0u64, 1, 2, 31, 1024] {
        let (sum, events) = run(pipeline, n);
        assert_eq!(sum, n * (n.saturating_sub(1)) / 2);
        assert_eq!(
            events.iter().filter(|e| e.kind == PUBLISH).count(),
            2 * n as usize
        );
        assert_eq!(
            events.iter().filter(|e| e.kind == RECEIVE).count(),
            2 * n as usize
        );
        assert_eq!(events.iter().filter(|e| e.kind == SPAWN).count(), 3);
    }
}
unsafe extern "C" fn cancelled(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: receiver and resource are uniquely owned by this child.
    unsafe {
        let rx = input.cast::<*mut Channel>().read();
        let resource = hirsel_core_resource_alloc(context, 1, 8, 0, 0).pointer;
        assert!(!resource.is_null());
        let mut n = 0u64;
        assert_eq!(
            hirsel_core_receive(context, rx, (&mut n as *mut u64).cast(), 31).status,
            1
        );
        hirsel_core_finalizer_begin(context, std::ptr::NonNull::new(resource).unwrap());
        hirsel_core_raise(context, causes::Cause::new(causes::OVERFLOW, 32));
        hirsel_core_finalizer_end(context, std::ptr::NonNull::new(resource).unwrap());
        hirsel_core_resource_free(std::ptr::NonNull::new(resource).unwrap(), 0);
        hirsel_core_receiver_drop(context, rx);
    }
    1
}
unsafe extern "C" fn cancelled_join(context: *mut Context, destination: *mut u8, _: *mut u8) -> u8 {
    // SAFETY: root owns task handle and sender; join returns the child's complete chain.
    unsafe {
        let c = hirsel_core_channel_new(context, 1, value::<u64>(8), 1);
        let mut rx = c.receiver;
        let child = hirsel_core_spawn(
            context,
            64,
            (&mut rx as *mut *mut Channel).cast(),
            cancelled,
            layout::<*mut Channel, ()>(0),
        );
        assert!(!child.task.is_null());
        for _ in 0..POLLS_PER_TURN {
            assert_eq!(hirsel_core_check_cancel(context, 0), 0);
        } // The poll allowance expires and the child reaches its blocking receive.
        hirsel_core_cancel(context, child.task);
        (*context).current.inject(1, true);
        let mut unit = 0u8;
        let join = hirsel_core_join(context, child.task, &mut unit, 33);
        assert_eq!((join.kind, join.status), (2, 0));
        let chain = causes::Causes::from_raw(join.causes);
        let seen: Vec<_> = chain.iter().map(|c| (c.kind(), c.site)).collect();
        assert_eq!(seen, [(causes::CANCELLED, 31), (causes::OVERFLOW, 32)]);
        drop(chain);
        assert_eq!((*context).current.attempts(), 0);
        hirsel_core_sender_drop(c.sender);
        assert!(
            (*context).primary.is_some(),
            "join must leave the parent primary prepaid"
        );
        destination.cast::<u64>().write(2);
    }
    0
}
#[test]
fn cancelled_join_preserves_cleanup_causes_without_borrowing_parent_primary() {
    assert_eq!(run(cancelled_join, ()).0, 2);
}
unsafe extern "C" fn abandoned(context: *mut Context, destination: *mut u8, _: *mut u8) -> u8 {
    // SAFETY: parent owns handle; dropping it must wait for child cleanup shielded.
    unsafe {
        let c = hirsel_core_channel_new(context, 1, value::<u64>(8), 1);
        let mut rx = c.receiver;
        let child = hirsel_core_spawn(
            context,
            64,
            (&mut rx as *mut *mut Channel).cast(),
            cancelled,
            layout::<*mut Channel, ()>(0),
        );
        assert_eq!(hirsel_core_check_cancel(context, 0), 0);
        hirsel_core_task_drop(context, child.task);
        assert_eq!((*context).causes.iter().count(), 0);
        hirsel_core_sender_drop(c.sender);
        destination.cast::<u64>().write(1);
    }
    0
}
#[test]
fn abandoned_handles_cancel_wait_and_clean_the_entire_child_chain() {
    assert_eq!(run(abandoned, ()).0, 1);
}
#[derive(Clone, Copy)]
struct Resource {
    pointer: *mut u8,
    site: u64,
}
unsafe extern "C" fn drop_resource(context: *mut Context, value: *mut u8) -> u8 {
    // SAFETY: queue transfers a uniquely owned resource into this generated drop recipe.
    unsafe {
        let value = value.cast::<Resource>().read();
        let resource = std::ptr::NonNull::new(value.pointer).unwrap();
        hirsel_core_finalizer_begin(context, resource);
        hirsel_core_raise(context, causes::Cause::fail(Default::default(), value.site));
        hirsel_core_finalizer_end(context, resource);
        hirsel_core_resource_free(resource, 0);
    }
    1
}
#[test]
fn try_send_returns_owner_and_receiver_cleans_reverse_fifo_despite_failures() {
    let mut context = Context::new(1024);
    causes::task_start(&mut context).unwrap();
    let before = context.available();
    // SAFETY: this fixture owns all resource/queue handles; drops consume each once.
    unsafe {
        let item = ValueLayout {
            drop: drop_resource,
            ..value::<Resource>(16)
        };
        let c = hirsel_core_channel_new(&mut context, 2, item, 1);
        assert!(!c.sender.is_null());
        let second = hirsel_core_channel_sender(c.sender);
        let mut resources = Vec::new();
        for site in 1..=3 {
            resources.push(Resource {
                pointer: hirsel_core_resource_alloc(&context, 1, 8, 0, 0).pointer,
                site,
            });
        }
        for resource in &mut resources[..2] {
            assert_eq!(
                hirsel_core_try_send(c.sender, (resource as *mut Resource).cast()),
                0
            );
        }
        // FAIL-3: a refused publication returns the exact original owner, untouched.
        let retained = (resources[2].pointer, resources[2].site);
        assert_eq!(
            hirsel_core_try_send(c.sender, (&mut resources[2] as *mut Resource).cast()),
            1
        );
        assert_eq!((resources[2].pointer, resources[2].site), retained);
        context.current.inject(1, true);
        assert_eq!(hirsel_core_receiver_drop(&mut context, c.receiver), 1);
        assert_eq!(
            hirsel_core_try_send(c.sender, (&mut resources[2] as *mut Resource).cast()),
            2
        );
        assert_eq!((resources[2].pointer, resources[2].site), retained);
        assert_eq!(
            drop_resource(&mut context, (&mut resources[2] as *mut Resource).cast()),
            1
        );
        let chain = causes::Causes::from_raw(hirsel_core_causes_take(&mut context));
        assert_eq!(chain.iter().map(|c| c.site).collect::<Vec<_>>(), [2, 1, 3]);
        drop(chain);
        hirsel_core_sender_drop(second);
        hirsel_core_sender_drop(c.sender);
    }
    assert_eq!(context.current.attempts(), 0);
    assert_eq!(context.available(), before);
}
#[test]
fn zero_channel_capacity_is_bounds_and_huge_charge_refuses_before_publication() {
    for capacity in [0, u64::MAX] {
        let mut context = Context::new(1024);
        causes::task_start(&mut context).unwrap();
        let before = context.available();
        // SAFETY: live fixture context and compiler-produced scalar layout.
        let c = unsafe { hirsel_core_channel_new(&mut context, capacity, value::<u64>(8), 73) };
        assert!(c.sender.is_null() && c.receiver.is_null());
        assert_eq!(c.status, u8::from(capacity == 0));
        if capacity == 0 {
            assert_eq!(context.causes.iter().next().unwrap().kind(), causes::BOUNDS);
        } else {
            assert_eq!(c.available, before);
            assert_eq!(context.available(), before);
        }
    }
}
#[test]
#[ignore = "invoked in a subprocess by guarded_stacks_report_102"]
fn stack_stop_child() {
    #[inline(never)]
    fn recurse(n: u64) -> u64 {
        if n == u64::MAX {
            return n;
        }
        let mut bytes = [0u8; 8192];
        std::hint::black_box(&mut bytes);
        let r = recurse(std::hint::black_box(n + 1));
        std::hint::black_box(bytes[0] as u64 + r)
    }
    unsafe extern "C" fn overflow(_: *mut Context, _: *mut u8, _: *mut u8) -> u8 {
        std::hint::black_box(recurse(0));
        0
    }
    run(overflow, ());
}
#[test]
fn guarded_stacks_report_102() {
    let out = crate::test_process::command()
        .args([
            "--exact",
            "tasks_tests::stack_stop_child",
            "--ignored",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(102));
    assert_eq!(out.stderr, b"run stop: Stack\n");
}
#[test]
fn child_results_keep_their_origin_funding_until_joiner_drop() {
    unsafe extern "C" fn allocate(context: *mut Context, dest: *mut u8, _: *mut u8) -> u8 {
        let block = Block::new(unsafe { &*context }, Some(16), 16, 8).unwrap();
        // SAFETY: result owns the child-funded allocation after the child ends.
        unsafe {
            dest.cast::<*mut u8>().write(block.into_raw().as_ptr());
        }
        0
    }
    unsafe extern "C" fn root(context: *mut Context, dest: *mut u8, _: *mut u8) -> u8 {
        // SAFETY: result pointer becomes exclusively owned by this parent at join.
        unsafe {
            let mut unit = 0u8;
            let child =
                hirsel_core_spawn(context, 16, &mut unit, allocate, layout::<(), *mut u8>(8));
            let mut result = ptr::null_mut();
            assert_eq!(
                hirsel_core_join(context, child.task, (&mut result as *mut *mut u8).cast(), 1).kind,
                0
            );
            hirsel_core_free(std::ptr::NonNull::new(result).unwrap(), 16);
            dest.cast::<u64>().write(16);
        }
        0
    }
    assert_eq!(run(root, ()).0, 16);
}
unsafe extern "C" fn empty_recipe(
    _: *mut Context,
    _: *mut u8,
    _: *mut u64,
    _: *mut crate::counted::Worklist,
) -> bool {
    true
}
// CONC-11: both unwrap orders and the foreign last refund retain the origin payer.
#[test]
fn arc_unwrap_observes_release_order_and_foreign_last_refund() {
    unsafe extern "C" fn child(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: child input transfers one Arc holder with immutable u64 payload.
        unsafe {
            let node = std::ptr::NonNull::new(input.cast::<*mut u8>().read()).unwrap();
            hirsel_core_release(context, node, true, 16, empty_recipe);
        }
        0
    }
    unsafe extern "C" fn root(context: *mut Context, dest: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: two exact holders, transferred independently; unwrap success owns the node.
        unsafe {
            let release_first = input.cast::<bool>().read();
            let node = Block::graph(&*context, Some(16), 16, 8).unwrap().into_raw();
            hirsel_core_counted_init(node, true);
            node.as_ptr().add(8).cast::<u64>().write(41);
            hirsel_core_share(node, true);
            let mut pointer = node.as_ptr();
            let h = hirsel_core_spawn(
                context,
                0,
                (&mut pointer as *mut *mut u8).cast(),
                child,
                layout::<*mut u8, ()>(0),
            );
            if release_first {
                for _ in 0..POLLS_PER_TURN {
                    assert_eq!(hirsel_core_check_cancel(context, 1), 0);
                }
            }
            let unique = hirsel_core_try_unwrap(node, true);
            assert_eq!(unique, release_first);
            if unique {
                assert_eq!(node.as_ptr().add(8).cast::<u64>().read(), 41);
                hirsel_core_free(node, 16);
            } else {
                hirsel_core_release(context, node, true, 16, empty_recipe);
            }
            let mut unit = 0u8;
            assert_eq!(hirsel_core_join(context, h.task, &mut unit, 2).kind, 0);
            dest.cast::<u64>().write(u64::from(unique));
        }
        0
    }
    for release_first in [false, true] {
        let (result, events) = run(root, release_first);
        assert_eq!(result, u64::from(release_first));
        assert_eq!(events.iter().filter(|e| e.kind == ARC_NEW).count(), 1);
        assert_eq!(events.iter().filter(|e| e.kind == ARC_SHARE).count(), 1);
        assert_eq!(
            events.iter().find(|e| e.kind == ARC_UNWRAP).unwrap().value,
            u64::from(release_first)
        );
        if !release_first {
            assert!(
                events
                    .iter()
                    .any(|e| e.kind == REFUND && e.task != 0 && e.object == 0 && e.value == 16)
            );
        }
    }
}
#[test]
fn cancelled_joiner_retains_handle_for_shielded_abandonment() {
    unsafe extern "C" fn root(context: *mut Context, dest: *mut u8, _: *mut u8) -> u8 {
        // SAFETY: the join failure preserves the parent's sole task handle.
        unsafe {
            let mut input = 4u64;
            let h = hirsel_core_spawn(
                context,
                0,
                (&mut input as *mut u64).cast(),
                add,
                layout::<u64, u64>(8),
            );
            (*context)
                .cancel_requested
                .store(true, std::sync::atomic::Ordering::Release);
            assert_eq!(hirsel_core_join(context, h.task, dest, 51).status, 1);
            hirsel_core_task_drop(context, h.task);
            assert_eq!(
                (*context)
                    .causes
                    .iter()
                    .map(|c| (c.kind(), c.site))
                    .collect::<Vec<_>>(),
                [(causes::CANCELLED, 51)]
            );
        }
        1
    }
    let mut context = Context::new(1024);
    causes::task_start(&mut context).unwrap();
    let before = context.available();
    let mut unit = 0u8;
    // SAFETY: failure leaves its result uninitialized; only the chain is consumed.
    unsafe {
        assert_eq!(
            run_root(
                &mut context,
                root,
                &mut unit,
                &mut unit,
                0,
                0,
                SchedulerProvider::default()
            ),
            1
        );
        let chain = causes::Causes::from_raw(hirsel_core_causes_take(&mut context));
        assert_eq!(
            chain.iter().map(|c| (c.kind(), c.site)).collect::<Vec<_>>(),
            [(causes::CANCELLED, 51)]
        );
        drop(chain);
    }
    assert_eq!(context.available(), before + 32);
}
#[test]
fn abandoning_successful_resource_result_propagates_its_finalizer_failure() {
    unsafe extern "C" fn create(context: *mut Context, destination: *mut u8, _: *mut u8) -> u8 {
        // SAFETY: successful task result transfers its prepaid resource container.
        unsafe {
            let p = hirsel_core_resource_alloc(context, 1, 8, 0, 0).pointer;
            assert!(!p.is_null());
            destination.cast::<Resource>().write(Resource {
                pointer: p,
                site: 61,
            });
        }
        0
    }
    unsafe extern "C" fn root(context: *mut Context, _: *mut u8, _: *mut u8) -> u8 {
        // SAFETY: sole child handle is consumed by cleanup. Its R cleanup can fail
        // and must abandon this normal continuation, even though the child succeeded.
        unsafe {
            let mut unit = 0u8;
            let descriptor = TaskLayout {
                input: value::<()>(0),
                result: ValueLayout {
                    drop: drop_resource,
                    ..value::<Resource>(16)
                },
            };
            let child = hirsel_core_spawn(context, 32, &mut unit, create, descriptor);
            assert!(!child.task.is_null());
            hirsel_core_task_drop(context, child.task)
        }
    }
    let mut context = Context::new(2048);
    causes::task_start(&mut context).unwrap();
    let before = context.available();
    let mut unit = 0u8;
    // SAFETY: root result is uninitialized on its failure status and never read.
    unsafe {
        assert_eq!(
            run_root(
                &mut context,
                root,
                &mut unit,
                &mut unit,
                0,
                0,
                SchedulerProvider::default()
            ),
            1
        );
        let chain = causes::Causes::from_raw(hirsel_core_causes_take(&mut context));
        assert_eq!(
            chain.iter().map(|c| (c.kind(), c.site)).collect::<Vec<_>>(),
            [(causes::FAIL, 61)]
        );
        drop(chain);
    }
    assert_eq!(context.available(), before);
}

#[test]
fn foreign_growth_defers_parent_refunds_until_parent_events_and_received_grant_reuses_room() {
    #[derive(Clone, Copy)]
    struct Input {
        parent: *const Context,
        first: std::ptr::NonNull<u8>,
        second: std::ptr::NonNull<u8>,
        resource: std::ptr::NonNull<u8>,
    }
    unsafe extern "C" fn child(context: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: the child owns both transferred blocks and the resource, while
        // its blocked parent grants exclusive access to its original payer.
        unsafe {
            let input = input.cast::<Input>().read();
            let before = (*input.parent).available();
            drop(Block::from_raw(input.first, 8));
            let second = Block::from_raw(input.second, 8);
            let grown = second.replacement(&*context, Some(16), 16, 8).unwrap();
            // MEM-20: the parent's eight-byte refund is still pending: this
            // child event cannot make it visible to the parent's limit.
            let growth = (*input.parent).available() == before - 16;
            drop(second);
            drop(grown);
            hirsel_core_finalizer_begin(context, input.resource);
            let first = Block::new(&*context, Some(16), 16, 8).unwrap();
            drop(first);
            let next = Block::new(&*context, Some(16), 16, 8);
            let grant = next.is_ok();
            drop(next);
            hirsel_core_finalizer_end(context, input.resource);
            hirsel_core_resource_free(input.resource, 0);
            out.cast::<u64>()
                .write(u64::from(growth) | (u64::from(grant) << 1));
        }
        0
    }
    unsafe extern "C" fn root(context: *mut Context, out: *mut u8, _: *mut u8) -> u8 {
        // SAFETY: prepared owners move once to a child; join moves its plain result.
        unsafe {
            let mut input = Input {
                parent: context,
                first: Block::new(&*context, Some(8), 8, 8).unwrap().into_raw(),
                second: Block::new(&*context, Some(8), 8, 8).unwrap().into_raw(),
                resource: std::ptr::NonNull::new(
                    hirsel_core_resource_alloc(&*context, 1, 8, 0, 16).pointer,
                )
                .unwrap(),
            };
            let task = hirsel_core_spawn(
                context,
                0,
                (&mut input as *mut Input).cast(),
                child,
                layout::<Input, u64>(8),
            );
            let joined = hirsel_core_join(context, task.task, out, 1);
            assert_eq!((joined.kind, joined.status), (0, 0));
        }
        0
    }
    let (result, events) = run(root, ());
    assert_eq!(result, 3);
    let refund = events
        .iter()
        .position(|e| e.kind == REFUND && e.value == 8)
        .unwrap();
    let charge = events
        .iter()
        .position(|e| e.kind == CHARGE && e.value == 16)
        .unwrap();
    assert!(refund < charge);
    // SAFETY: native context binding has the same logical task ownership as Rust.
    unsafe {
        let context = hirsel_core_context_new(1 << 20);
        assert!(!hirsel_core_task_start(context).pointer.is_null());
        let before = (*context).available();
        let mut result = 0u64;
        assert_eq!(
            run_root(
                context,
                root,
                ptr::null_mut(),
                (&mut result as *mut u64).cast(),
                0,
                8,
                SchedulerProvider::default()
            ),
            0
        );
        assert_eq!(result, 3);
        assert_eq!((*context).available(), before);
        hirsel_core_context_free(context);
    }
}

#[test]
fn inline_arc_count_operations_publish_the_same_events_as_runtime_entries() {
    unsafe extern "C" fn root(context: *mut Context, out: *mut u8, _: *mut u8) -> u8 {
        use std::sync::atomic::{AtomicU64, Ordering, fence};
        // SAFETY: this fixture owns an aligned node and performs the emitted
        // sequence while its cooperative scheduler holds the event turn.
        unsafe {
            let node = Block::graph(&*context, Some(16), 16, 8).unwrap().into_raw();
            node.cast::<AtomicU64>().as_ptr().write(AtomicU64::new(1));
            if (*context).events != 0 {
                hirsel_core_arc_initialized(context);
            }
            let count = node.cast::<AtomicU64>().as_ref();
            let old = count
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
                .unwrap();
            assert_eq!(old, 1);
            if (*context).events != 0 {
                hirsel_core_arc_shared(context, node.as_ptr());
            }
            let last = count.fetch_sub(1, Ordering::Release) == 1;
            if (*context).events != 0 {
                hirsel_core_arc_released(context, last);
            }
            let unique = count.load(Ordering::Acquire) == 1;
            if (*context).events != 0 {
                hirsel_core_arc_unwrapped(context, unique);
            }
            let last = count.fetch_sub(1, Ordering::Release) == 1;
            if last {
                fence(Ordering::Acquire);
            }
            if (*context).events != 0 {
                hirsel_core_arc_released(context, last);
            }
            crate::block::free_graph(node, 16);
            out.cast::<u64>().write(u64::from(unique && last));
        }
        0
    }
    let (result, events) = run(root, ());
    assert_eq!(result, 1);
    let arc: Vec<_> = events
        .iter()
        .filter(|e| (ARC_NEW..=ARC_UNWRAP).contains(&e.kind))
        .map(|e| (e.kind, e.value))
        .collect();
    assert_eq!(
        arc,
        [
            (ARC_NEW, 1),
            (ARC_SHARE, 2),
            (ARC_RELEASE, 0),
            (ARC_UNWRAP, 1),
            (ARC_RELEASE, 1)
        ]
    );
}

#[test]
fn event_gate_enables_poll_hooks_without_a_sink_and_children_inherit_the_gate() {
    unsafe extern "C" fn child(context: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: parent transfers one expected recording bit and owns result.
        unsafe {
            out.cast::<u64>().write(u64::from(
                ((*context).events != 0) == input.cast::<bool>().read(),
            ));
        }
        0
    }
    unsafe extern "C" fn root(context: *mut Context, out: *mut u8, _: *mut u8) -> u8 {
        use std::sync::atomic::{AtomicU64, Ordering};
        let node = AtomicU64::new(1);
        // SAFETY: exact emitted count/gate sequence under the scheduler turn.
        unsafe {
            if (*context).events != 0 {
                hirsel_core_arc_initialized(context);
            }
            node.fetch_add(1, Ordering::Relaxed);
            if (*context).events != 0 {
                hirsel_core_arc_shared(context, (&node as *const AtomicU64).cast());
            }
            let last = node.fetch_sub(1, Ordering::Release) == 1;
            if (*context).events != 0 {
                hirsel_core_arc_released(context, last);
            }
            let unique = node.load(Ordering::Acquire) == 1;
            if (*context).events != 0 {
                hirsel_core_arc_unwrapped(context, unique);
            }
            let mut enabled = (*context).events != 0;
            let task = hirsel_core_spawn(
                context,
                0,
                (&mut enabled as *mut bool).cast(),
                child,
                layout::<bool, u64>(8),
            );
            assert!(!task.task.is_null());
            assert_eq!(hirsel_core_join(context, task.task, out, 1).status, 0);
        }
        0
    }
    ARC_HOOK_CALLS.set(0);
    let mut context = Context::new(1 << 20);
    causes::task_start(&mut context).unwrap();
    let mut result = 0u64;
    // SAFETY: root has unit input and an exclusive initialized u64 destination.
    unsafe {
        assert_eq!(
            run_root(
                &mut context,
                root,
                ptr::null_mut(),
                (&mut result as *mut u64).cast(),
                0,
                8,
                SchedulerProvider::default()
            ),
            0
        );
    }
    assert_eq!(result, 1);
    assert_eq!(context.events, 0);
    assert_eq!(
        ARC_HOOK_CALLS.get(),
        4,
        "a no-sink run must enter Arc poll hooks"
    );
    let (result, events) = run(root, ());
    assert_eq!(result, 1);
    assert_eq!(ARC_HOOK_CALLS.get(), 8);
    let arc: Vec<_> = events
        .iter()
        .filter(|e| (ARC_NEW..=ARC_UNWRAP).contains(&e.kind))
        .map(|e| (e.kind, e.value))
        .collect();
    assert_eq!(
        arc,
        [
            (ARC_NEW, 1),
            (ARC_SHARE, 2),
            (ARC_RELEASE, 0),
            (ARC_UNWRAP, 1)
        ]
    );
}

#[test]
fn unjoined_failed_child_frees_the_moved_string_backing() {
    unsafe extern "C" fn fail(context: *mut Context, _: *mut u8, _: *mut u8) -> u8 {
        // SAFETY: child owns this freshly admitted buffer and its unused primary cell.
        unsafe {
            let message = hirsel_core_alloc(context, ptr::null_mut(), 4, 1, 8, false, 0);
            assert!(!message.pointer.is_null());
            ptr::copy_nonoverlapping(b"boom".as_ptr(), message.pointer, 4);
            hirsel_core_raise_string(context, 55, message.pointer, 4, 4);
        }
        1
    }
    unsafe extern "C" fn abandon(context: *mut Context, result: *mut u8, _: *mut u8) -> u8 {
        // SAFETY: root transfers its only child handle to drop and never joins it.
        unsafe {
            let child = hirsel_core_spawn(context, 8, ptr::null_mut(), fail, layout::<(), ()>(0));
            assert!(!child.task.is_null());
            assert_eq!(hirsel_core_task_drop(context, child.task), 0);
            result.cast::<u64>().write(1);
        }
        0
    }
    assert_eq!(run(abandon, ()).0, 1);
}

#[path = "rule_tasks_tests.rs"]
mod rule_tasks_tests;
