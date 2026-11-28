use super::*;
use hirsel_core_rt::tasks::ValueLayout;

#[repr(C)]
struct Admission {
    handle: usize,
    available: u64,
}

unsafe extern "C" {
    fn hirsel_core_held_new(
        context: *mut Context,
        value: *const u8,
        witness: *const [u8; 32],
        charge: u64,
        layout: &ValueLayout,
    ) -> Admission;
    fn hirsel_core_held_take(handle: usize, witness: *const [u8; 32], output: *mut u8) -> bool;
    fn hirsel_core_held_drop(context: *mut Context, handle: usize) -> u8;
}

// PACK-16's single-node graph for u64, with no outgoing edges.
fn witness() -> [u8; 32] {
    let name = b"u64".as_slice();
    let native = ContentHash::of_parts("hirsel.core-closure.v1", [name]);
    let mut graph = Vec::from(1u32.to_le_bytes());
    graph.extend_from_slice(&3u32.to_le_bytes());
    graph.extend_from_slice(name);
    graph.push(1);
    graph.extend_from_slice(&native.0);
    graph.extend_from_slice(&[0; 16]);
    ContentHash::of_parts("hirsel.held-type.v1", [name, &native.0, &graph]).0
}

#[test]
fn host_and_loaded_code_exchange_held_scalars() {
    let _alone = alone();
    let (loader, _) = primary_loader();
    let module = compile_pack_module(
        "pub fn make(v: u64) -> Held[send] { code.hold(v) }
         pub fn extract(take h: Held[send]) -> u64 {
             let r: Result[u64, (code.TakeError, Held[send])] = code.take_as(take h)
             match take r { Ok(v) => v, Error(_) => 0 }
         }",
        &["make", "extract"],
        Backend::Cranelift,
    );
    loader.admit_module(&module.bytes).unwrap();
    let make = module.load_raw(loader, "make");
    let extract = module.load_raw(loader, "extract");
    let hash = witness();
    let mut observed = [0u64; 2];
    let status = run_in_worker(1 << 20, |ctx| unsafe {
        let value = 201u64;
        let layout = ValueLayout {
            bytes: 8,
            align: 8,
            logical: 8,
            clean: None,
        };
        let mut held =
            hirsel_core_held_new(ctx, (&raw const value).cast(), &hash, 40, &layout).handle;
        let take_loaded: Take = std::mem::transmute(extract.entry());
        let status = take_loaded(ctx, &mut observed[0], &mut held);
        if status != 0 {
            return status;
        }
        let make_loaded: Make = std::mem::transmute(make.entry());
        let status = make_loaded(ctx, &mut held, 309);
        if status != 0 {
            return status;
        }
        if !hirsel_core_held_take(held, &hash, (&raw mut observed[1]).cast()) {
            return hirsel_core_held_drop(ctx, held);
        }
        0
    });
    assert_eq!(status, Ok(0));
    assert_eq!(observed, [201, 309]);
}

#[test]
fn prelinked_pools_keep_distinct_held_domains() {
    use hirsel_core_rt::run_stop::ExecutionScope;
    let _alone = alone();
    let pool = || {
        let loader = hirsel_pack::start(hirsel_pack::config(0, Mask::PURE, 0, 0)).unwrap();
        hirsel_pack::loader_handle(loader).unwrap()
    };
    let a = pool();
    let b = pool();
    let producer = Execution::in_pool(a).unwrap();
    let foreign = Execution::in_pool(b).unwrap();
    let consumer = Execution::in_pool(a).unwrap();
    let hash = witness();
    let value = 17u64;
    let layout = ValueLayout {
        bytes: 8,
        align: 8,
        logical: 8,
        clean: None,
    };
    let _producer = unsafe { ExecutionScope::enter(&producer) };
    let mut ctx = Context::new(4096);
    let held = unsafe {
        hirsel_core_held_new(&raw mut ctx, (&raw const value).cast(), &hash, 40, &layout).handle
    };
    let mut output = 0;
    let took = {
        let _foreign = unsafe { ExecutionScope::enter(&foreign) };
        unsafe { hirsel_core_held_take(held, &hash, (&raw mut output as *mut u64).cast()) }
    };
    assert!(!took);
    assert_eq!(output, 0);
    let _consumer = unsafe { ExecutionScope::enter(&consumer) };
    assert!(unsafe { hirsel_core_held_take(held, &hash, (&raw mut output as *mut u64).cast()) });
    assert_eq!(output, value);
}

#[test]
fn stop_returns_loaded_code_slot_and_literal_owners() {
    use hirsel_core_rt::{
        causes::CauseDataRef,
        code,
        run_stop::{self, Stop},
    };

    let _alone = alone();
    let (loader, pool) = primary_loader();
    let module = compile_pack_module(
        "pub fn text() -> String { \"stopped literal\" }",
        &["text"],
        Backend::Cranelift,
    );
    loader.admit_module(&module.bytes).unwrap();
    let raw = module.load_raw(loader, "text");
    let pc = raw.entry();
    let owner =
        unsafe { CauseDataRef::retain_raw(raw.cause_owner().unwrap() as *const _).unwrap() };
    let status = run_in_worker(1 << 20, |ctx| unsafe {
        let text: unsafe extern "C" fn(*mut Context, *mut [usize; 3]) -> u8 =
            std::mem::transmute(pc);
        let status = text(ctx, &mut [0; 3]);
        if status != 0 {
            return status;
        }
        let handle = code::hirsel_core_code_bind(raw.descriptor() as *const u8);
        let shared = code::hirsel_core_code_share(handle);
        let slot = code::hirsel_core_code_slot_new(pool);
        assert_eq!(code::hirsel_core_code_activate(slot, 0, shared, &mut 0), 0);
        drop(raw);
        run_stop::raise(Stop::from_static(b"CustodyStop"));
    });
    assert_eq!(
        status,
        Err(EndReason::RunStop(Stop::from_static(b"CustodyStop")))
    );
    loader.reclaim().unwrap();
    assert_eq!(
        (loader.resolve(pc).is_some(), owner.ref_count()),
        (false, 1)
    );
}
