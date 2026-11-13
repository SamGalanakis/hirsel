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
            let block = Block::new(&ctx, Some(N as u64), N, 1).unwrap();
            // SAFETY: this fresh block has N bytes, all initialized before reading.
            unsafe {
                block.pointer().as_ptr().write_bytes(0, N);
                block.pointer().as_ptr().write(i as u8);
                sum += black_box(block.pointer().as_ptr()).read() as u64;
            }
            drop(black_box(block));
        } else {
            let mut block = Box::new([0u8; N]);
            block[0] = i as u8;
            sum += black_box(&block)[0] as u64;
            drop(black_box(block));
        }
    }
    sum
}

fn growth(mode: &str) -> u64 {
    if mode == "rust" {
        let mut values = Vec::new();
        for i in 0..COUNT {
            values.push(black_box(i));
        }
        return black_box(values).iter().sum();
    }
    if mode == "matched" {
        let mut values = Vec::new();
        for i in 0..COUNT {
            if values.len() == values.capacity() {
                let capacity =
                    charge::grown_capacity(values.capacity() as u64, values.len() as u64 + 1)
                        .unwrap() as usize;
                let mut next = Vec::with_capacity(capacity);
                next.extend_from_slice(&values);
                values = next;
            }
            values.push(black_box(i));
        }
        return black_box(values).iter().sum();
    }
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
        let node = Block::new(&ctx, Some(16), 16, 8).unwrap().into_raw();
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

fn limits(runtime: bool) -> u64 {
    if runtime {
        let mut ctx = Context::new(1024);
        let mut sum = 0;
        for _ in 0..COUNT {
            ctx.limit_enter(black_box(64)).unwrap();
            sum += black_box(ctx.available());
            ctx.limit_exit();
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
        "limits" => limits(runtime),
        _ => panic!("unknown workload"),
    };
    println!("{checksum}");
}
