use hirsel_core_layout::charge;
use hirsel_core_rt::{
    Context,
    block::Block,
    counted::{self, Worklist},
    region::Region,
};
use std::{hint::black_box, ptr, rc::Rc};

const COUNT: u64 = 1_000_000;
unsafe extern "C" fn empty(_: *mut Context, _: *mut u8, _: *mut u64, _: *mut Worklist) -> bool {
    true
}

fn allocation<const N: usize>(runtime: bool) -> u64 {
    let ctx = Context::new(u64::MAX);
    let mut sum = 0;
    for i in 0..COUNT {
        if runtime {
            // SAFETY: this is the emitter's inline instruction sequence. The
            // native size/alignment are constants, not fields in a Rust owner.
            let block = unsafe {
                hirsel_core_rt::abi::inline_own_alloc(&ctx, Some(N as u64), N, 1).unwrap()
            };
            // SAFETY: this fresh block has N bytes, all initialized before reading.
            unsafe {
                block.as_ptr().write_bytes(0, N);
                block.as_ptr().write(i as u8);
                sum += black_box(block.as_ptr()).read() as u64;
            }
            // SAFETY: all bytes are plain; pass the same original native operands.
            unsafe {
                hirsel_core_rt::abi::inline_own_free(&ctx, black_box(block), N as u64, N, 1);
            }
        } else {
            let mut block = Box::new([0u8; N]);
            block[0] = i as u8;
            sum += black_box(&block)[0] as u64;
            drop(black_box(block));
        }
    }
    sum
}

// Both branches make exactly malloc(N+8)/free calls, align to 8 and zero N
// payload bytes at base+8. Only the runtime branch admits, prefixes and refunds.
#[inline(never)]
fn matched_allocation<const N: usize, const RUNTIME: bool, const CURRENT: bool>() -> u64 {
    let ctx = Context::new(u64::MAX);
    let mut sum = 0;
    for i in 0..COUNT {
        // SAFETY: each branch owns the same N+8 native bytes, payload at offset8.
        unsafe {
            let block = if RUNTIME {
                hirsel_core_rt::abi::inline_own_alloc(&ctx, Some(N as u64), N, 8)
                    .unwrap()
                    .as_ptr()
            } else {
                let base = super::native_allocate(N + 8);
                if base.is_null() {
                    std::alloc::handle_alloc_error(
                        std::alloc::Layout::from_size_align(N + 8, 8).unwrap(),
                    );
                }
                base.add(8)
            };
            block.write_bytes(0, N);
            block.write(i as u8);
            sum += black_box(block).read() as u64;
            let block = black_box(block);
            if RUNTIME && CURRENT {
                hirsel_core_rt::abi::inline_own_free_current(
                    &ctx,
                    std::ptr::NonNull::new_unchecked(block),
                    N as u64,
                    N,
                    8,
                );
            } else if RUNTIME {
                hirsel_core_rt::abi::inline_own_free(
                    &ctx,
                    std::ptr::NonNull::new_unchecked(block),
                    N as u64,
                    N,
                    8,
                );
            } else {
                super::native_free(block.sub(8));
            }
        }
    }
    black_box(ctx.available());
    sum
}

// Access fields only through the offsets an emitter includes from abi_layout.
#[inline(always)]
unsafe fn field<T>(base: *mut u8, offset: usize) -> *mut T {
    // SAFETY: caller selects an aligned frozen field of a live task-owned object.
    unsafe { base.add(offset).cast() }
}
#[inline(never)]
fn emitted_region() -> u64 {
    use hirsel_core_rt::abi_layout::*;
    let ctx = Context::new(u64::MAX);
    let mut region = Region::new(65536);
    let mut sum = 0;
    for i in 0..COUNT {
        // SAFETY: this is the emitter's constant-size/stride u64 bump sequence.
        // The task exclusively owns ctx/region; allocation never moves a chunk.
        unsafe {
            let limit = field::<*mut u8>(
                (&ctx as *const Context).cast_mut().cast(),
                CONTEXT_CURRENT_LIMIT,
            )
            .read();
            let raw = (&mut region as *mut Region).cast::<u8>();
            let p = if field::<*mut u8>(raw, REGION_PAYER).read() != limit
                || field::<u64>(raw, REGION_CHARGE).read() == 0
            {
                let result = hirsel_core_rt::abi::hirsel_core_region_alloc(
                    &ctx,
                    &mut region,
                    8,
                    8,
                    8,
                    false,
                );
                assert!(!result.pointer.is_null());
                result.pointer
            } else {
                let available = field::<u64>(limit, LIMIT_AVAILABLE);
                assert!(available.read() >= 8);
                available.write(available.read().wrapping_sub(8));
                let outstanding = field::<u64>(limit, LIMIT_OUTSTANDING);
                outstanding.write(outstanding.read().wrapping_add(8));
                let charge = field::<u64>(raw, REGION_CHARGE);
                charge.write(charge.read().wrapping_add(8));
                let cursor = field::<*mut u8>(raw, REGION_CURSOR);
                let next = cursor.read().wrapping_add(8);
                if next.addr() <= field::<*mut u8>(raw, REGION_END).read().addr() {
                    let p = cursor.read();
                    cursor.write(next);
                    p
                } else {
                    hirsel_core_rt::abi::hirsel_core_region_refill(&mut region, 8, 8)
                }
            };
            p.cast::<u64>().write(i);
            sum += black_box(p).cast::<u64>().read();
        }
    }
    eprintln!(
        "logical_region_bytes=8000000 physical_chunk_bytes={}",
        region.physical_bytes()
    );
    drop(region);
    assert_eq!(ctx.available(), u64::MAX);
    sum
}

fn allocation_mode<const N: usize>(mode: &str) -> u64 {
    match mode {
        "matched" => matched_allocation::<N, false, false>(),
        "inline" => matched_allocation::<N, true, false>(),
        "current" => matched_allocation::<N, true, true>(),
        _ => allocation::<N>(mode == "runtime"),
    }
}

#[inline(never)]
fn emitted_growth() -> u64 {
    let ctx = Context::new(u64::MAX);
    let mut data: *mut u64 = ptr::null_mut();
    let mut len = 0usize;
    let mut capacity = 0usize;
    for i in 0..COUNT {
        if len == capacity {
            let next_capacity =
                charge::grown_capacity(capacity as u64, len as u64 + 1).unwrap() as usize;
            // SAFETY: this is the emitted slow grow path. Each old carrier owns
            // its prefix; admission precedes copy and the old block's refund.
            unsafe {
                let next = hirsel_core_rt::abi::hirsel_core_alloc(
                    &ctx,
                    data.cast(),
                    next_capacity * 8,
                    8,
                    charge::seq(next_capacity as u64, 8).unwrap(),
                    false,
                    hirsel_core_rt::block::PLAIN,
                )
                .pointer
                .cast::<u64>();
                assert!(!next.is_null());
                if capacity != 0 {
                    ptr::copy_nonoverlapping(data, next, len);
                    hirsel_core_rt::abi::hirsel_core_free(
                        std::ptr::NonNull::new_unchecked(data.cast()),
                        charge::seq(capacity as u64, 8).unwrap(),
                    );
                }
                data = next;
                capacity = next_capacity;
            }
        }
        // SAFETY: len < capacity; this slot is uninitialized and exclusively owned.
        unsafe {
            data.add(len).write(black_box(i));
        }
        len = len.wrapping_add(1);
    }
    // SAFETY: all len elements are initialized; the final carrier owns the block.
    unsafe {
        let sum = black_box(std::slice::from_raw_parts(data, len))
            .iter()
            .sum();
        hirsel_core_rt::abi::hirsel_core_free(
            std::ptr::NonNull::new_unchecked(data.cast()),
            charge::seq(capacity as u64, 8).unwrap(),
        );
        sum
    }
}

fn growth(mode: &str) -> u64 {
    match mode {
        "rust" => rust_growth(),
        "matched" => matched_growth(),
        "runtime" => emitted_growth(),
        _ => wrapped_growth(),
    }
}

#[inline(never)]
fn rust_growth() -> u64 {
    let mut values = Vec::new();
    for i in 0..COUNT {
        values.push(black_box(i));
    }
    black_box(values).iter().sum()
}

#[inline(never)]
fn matched_growth() -> u64 {
    let mut values = Vec::new();
    for i in 0..COUNT {
        if values.len() == values.capacity() {
            let capacity = charge::grown_capacity(values.capacity() as u64, values.len() as u64 + 1)
                .unwrap() as usize;
            let mut next = Vec::with_capacity(capacity);
            next.extend_from_slice(&values);
            values = next;
        }
        values.push(black_box(i));
    }
    black_box(values).iter().sum()
}

#[inline(never)]
fn wrapped_growth() -> u64 {
    let ctx = Context::new(u64::MAX);
    let mut storage: Option<Block> = None;
    let mut capacity = 0;
    for i in 0..COUNT {
        if i == capacity {
            let next_capacity = charge::grown_capacity(capacity, i + 1).unwrap();
            let bytes = next_capacity as usize * 8;
            let next = match &storage {
                Some(old) => old
                    .replacement(&ctx, charge::seq(next_capacity, 8), bytes, 8)
                    .unwrap(),
                None => Block::new(&ctx, charge::seq(next_capacity, 8), bytes, 8).unwrap(),
            };
            if let Some(old) = &storage {
                // SAFETY: copy the initialized, nonoverlapping old prefix.
                unsafe {
                    ptr::copy_nonoverlapping(
                        old.pointer().as_ptr(),
                        next.pointer().as_ptr(),
                        i as usize * 8,
                    );
                }
            }
            storage = Some(next);
            capacity = next_capacity;
        }
        // SAFETY: capacity exceeds i and this slot has not previously been written.
        unsafe {
            storage
                .as_ref()
                .unwrap()
                .pointer()
                .as_ptr()
                .cast::<u64>()
                .add(i as usize)
                .write(black_box(i));
        }
    }
    // SAFETY: every one of COUNT elements was initialized and remains owned here.
    unsafe {
        black_box(std::slice::from_raw_parts(
            storage.as_ref().unwrap().pointer().as_ptr().cast::<u64>(),
            COUNT as usize,
        ))
        .iter()
        .sum()
    }
}

fn region(runtime: bool) -> u64 {
    let mut sum = 0;
    if runtime {
        let ctx = Context::new(u64::MAX);
        let mut region = Region::new(65536);
        for i in 0..COUNT {
            let p = region.allocate(&ctx, 8, 8, Some(8)).unwrap();
            // SAFETY: the newly allocated u64 is aligned and live until region close.
            unsafe {
                p.cast::<u64>().as_ptr().write(i);
                sum += black_box(p).cast::<u64>().as_ptr().read();
            }
        }
        eprintln!(
            "logical_region_bytes=8000000 physical_chunk_bytes={}",
            region.physical_bytes()
        );
        drop(region);
    } else {
        let bump = bumpalo::Bump::new();
        for i in 0..COUNT {
            sum += *black_box(bump.alloc(i));
        }
        eprintln!(
            "payload_bytes=8000000 physical_chunk_bytes={}",
            bump.allocated_bytes()
        );
        drop(bump);
    }
    sum
}

fn counted(runtime: bool) -> u64 {
    if runtime {
        let mut ctx = Context::new(64);
        let node = Block::graph(&ctx, Some(16), 16, 8).unwrap().into_raw();
        // SAFETY: the fixture owns an initialized immutable u64 and exact holders.
        unsafe {
            counted::initialize(node, false);
            node.as_ptr().add(8).cast::<u64>().write(7);
            let mut sum = 0;
            for _ in 0..COUNT {
                counted::share(black_box(node), false);
                sum += black_box(node).as_ptr().add(8).cast::<u64>().read();
                counted::release(&mut ctx, black_box(node), false, 16, empty);
            }
            counted::release(&mut ctx, node, false, 16, empty);
            sum
        }
    } else {
        let value = Rc::new(7u64);
        let mut sum = 0;
        for _ in 0..COUNT {
            let holder = Rc::clone(black_box(&value));
            sum += **black_box(&holder);
            drop(black_box(holder));
        }
        sum
    }
}

// Emitted sequence for a statically empty bound. No values can refer to this
// scope, so no table identity is materialized. Admission/refund remain visible.
#[inline(always)]
fn empty_bound(available: &mut u64, reservation: u64) -> u64 {
    if reservation > *available {
        panic!("capacity");
    }
    *available -= reservation;
    let child_available = black_box(reservation);
    *available = available.wrapping_add(child_available);
    black_box(*available);
    child_available
}

#[inline(never)]
fn scoped_allocation(ctx: &mut Context) {
    // SAFETY: this callback owns its exact plain allocation and frees it once.
    unsafe {
        let p = hirsel_core_rt::abi::inline_own_alloc(ctx, Some(8), 8, 8).unwrap();
        p.as_ptr().write(7);
        black_box(p).as_ptr().read();
        hirsel_core_rt::abi::inline_own_free(ctx, p, 8, 8, 8);
    }
}

#[inline(never)]
fn material_scope<const BOUND: bool, const ALLOCATE: bool, const C_ABI: bool>() -> u64 {
    let mut ctx = Context::new(if BOUND { 1024 } else { 64 });
    let mut sum = 0;
    for _ in 0..COUNT {
        let bytes = black_box(64);
        if BOUND {
            if C_ABI {
                // SAFETY: the benchmark owns its live native context and matching exit.
                let admitted =
                    unsafe { hirsel_core_rt::abi::hirsel_core_limit_enter(&mut ctx, bytes) };
                assert!(!admitted.pointer.is_null());
            } else {
                ctx.limit_enter(bytes).unwrap();
            }
        }
        if ALLOCATE {
            scoped_allocation(black_box(&mut ctx));
        }
        sum += black_box(black_box(&mut ctx).available());
        if BOUND {
            if C_ABI {
                // SAFETY: exactly the successful entry above, with no surviving owners.
                unsafe {
                    hirsel_core_rt::abi::hirsel_core_limit_exit(&mut ctx);
                }
            } else {
                ctx.limit_exit();
            }
        }
    }
    sum
}

fn limits(mode: &str) -> u64 {
    if mode == "c_full_alloc" {
        material_scope::<true, true, true>()
    } else if mode == "c_full" {
        material_scope::<true, false, true>()
    } else if mode == "full_alloc" {
        material_scope::<true, true, false>()
    } else if mode == "alloc_control" {
        material_scope::<false, true, false>()
    } else if mode == "query" {
        material_scope::<false, false, false>()
    } else if mode == "full" {
        material_scope::<true, false, false>()
    } else if mode == "runtime" {
        let ctx = Context::new(1024);
        let mut sum = 0;
        for _ in 0..COUNT {
            // SAFETY: no operation in this statically empty body can retain a
            // payer or access the context before its reservation is refunded.
            sum += unsafe { empty_bound(&mut (*ctx.limit_record()).available, black_box(64)) };
        }
        sum
    } else {
        let mut available = 1024u64;
        let mut sum = 0;
        for _ in 0..COUNT {
            let reservation = black_box(64);
            if reservation > available {
                panic!("capacity");
            }
            available -= reservation;
            let child_available = reservation;
            sum += black_box(child_available);
            available += child_available;
            black_box(available);
        }
        sum
    }
}

pub fn main() {
    let mut args = std::env::args().skip(1);
    let workload = args.next().unwrap();
    let mode = args.next().unwrap();
    let runtime = mode == "runtime";
    let checksum = match workload.as_str() {
        "alloc16" => allocation_mode::<16>(&mode),
        "alloc64" => allocation_mode::<64>(&mode),
        "alloc256" => allocation_mode::<256>(&mode),
        "alloc4096" => allocation_mode::<4096>(&mode),
        "growth" => growth(&mode),
        "region" if mode == "inline" => emitted_region(),
        "region" => region(runtime),
        "rc" => counted(runtime),
        "limits" => limits(&mode),
        _ => panic!("unknown workload"),
    };
    println!("{checksum}");
}
