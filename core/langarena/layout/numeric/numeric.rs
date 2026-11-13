// Reviewed-by: langs-text-921 2026-11-13; valid-input Fixed/AoS/SoA arrays, strides, gather and checked reduction match pack.hirsel; wrapping cfg is a separate denominator, not checked parity.
// LAYOUT3 matches pack.hirsel: same fixed strides, AoS/SoA, gather and reduction.
#[path = "../../rust-matched/support.rs"]
#[allow(dead_code)]
mod support;
struct Cell {
    value: u64,
    index: u64,
}
fn add(a: u64, b: u64, wrapping: bool) -> u64 {
    if wrapping {
        a.wrapping_add(b)
    } else {
        a.checked_add(b).expect("Overflow")
    }
}
fn kernel(c: &support::Config, wrapping: bool) -> u64 {
    let n = c.number("n");
    let seed = u64::try_from(c.number("seed")).unwrap();
    assert!(n >= 2);
    if c.name.ends_with("Fixed") {
        assert_eq!(n, 64);
        let mut values = [0u64; 64];
        let mut indices = [0u64; 64];
        let mut stencil = [0u64; 64];
        for i in 0..64 {
            values[i] = (u64::try_from(i).unwrap() * 37 + seed) % 1000;
            indices[i] = u64::try_from((i * 17 + 3) % 64).unwrap();
        }
        for i in 0..64 {
            stencil[i] = add(
                add(values[(i + 63) % 64], values[i], wrapping),
                values[(i + 1) % 64],
                wrapping,
            );
        }
        let mut total = 0;
        for i in 0..64 {
            total = add(
                total,
                stencil[usize::try_from(indices[i]).unwrap()],
                wrapping,
            );
        }
        total
    } else if c.name.ends_with("AoS") {
        let mut values = Vec::with_capacity(n);
        for i in 0..n {
            values.push(Cell {
                value: (u64::try_from(i).unwrap() * 37 + seed) % 1000,
                index: u64::try_from((i * 17 + 3) % n).unwrap(),
            });
        }
        let mut stencil = vec![0u64; n];
        for i in 0..n {
            stencil[i] = add(
                add(values[(i + n - 1) % n].value, values[i].value, wrapping),
                values[(i + 1) % n].value,
                wrapping,
            );
        }
        let mut total = 0;
        for i in 0..n {
            total = add(
                total,
                stencil[usize::try_from(values[i].index).unwrap()],
                wrapping,
            );
        }
        total
    } else {
        assert!(c.name.ends_with("SoA"));
        let mut values = vec![0u64; n];
        let mut indices = vec![0u64; n];
        let mut stencil = vec![0u64; n];
        for i in 0..n {
            values[i] = (u64::try_from(i).unwrap() * 37 + seed) % 1000;
            indices[i] = u64::try_from((i * 17 + 3) % n).unwrap();
        }
        for i in 0..n {
            stencil[i] = add(
                add(values[(i + n - 1) % n], values[i], wrapping),
                values[(i + 1) % n],
                wrapping,
            );
        }
        let mut total = 0;
        for i in 0..n {
            total = add(
                total,
                stencil[usize::try_from(indices[i]).unwrap()],
                wrapping,
            );
        }
        total
    }
}
fn run(c: &support::Config) -> u32 {
    let wrapping = cfg!(wrapping);
    let mut total = 0u32;
    for _ in 0..c.warmup + c.iterations {
        total = total.wrapping_add(kernel(c, wrapping) as u32);
    }
    total
}
fn main() {
    support::main(run);
}
