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
    return black_box(values).iter().sum();
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
    return black_box(values).iter().sum();
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
fn material_scope<const BOUND: bool, const ALLOCATE: bool>() -> u64 {
    let mut ctx = Context::new(if BOUND { 1024 } else { 64 });
    let mut sum = 0;
    for _ in 0..COUNT {
        let bytes = black_box(64);
        if BOUND {
            ctx.limit_enter(bytes).unwrap();
        }
        if ALLOCATE {
            scoped_allocation(black_box(&mut ctx));
        }
        sum += black_box(black_box(&mut ctx).available());
        if BOUND {
            ctx.limit_exit();
        }
    }
    sum
}

fn limits(mode: &str) -> u64 {
    if mode == "full_alloc" {
        material_scope::<true, true>()
    } else if mode == "alloc_control" {
        material_scope::<false, true>()
    } else if mode == "query" {
        material_scope::<false, false>()
    } else if mode == "full" {
        material_scope::<true, false>()
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
        "alloc16" => allocation::<16>(runtime),
        "alloc64" => allocation::<64>(runtime),
        "alloc256" => allocation::<256>(runtime),
        "alloc4096" => allocation::<4096>(runtime),
        "growth" => growth(&mode),
        "region" => region(runtime),
        "rc" => counted(runtime),
        "limits" => limits(&mode),
        _ => panic!("unknown workload"),
    };
    println!("{checksum}");
}
