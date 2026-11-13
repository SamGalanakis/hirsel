// Review pending: langs-text-921. No matched ratio before independent review.
// Matches clbg.hirsel Fannkuchredux: inline arrays, identical permutation/flip order.
#[allow(dead_code)]
mod support;
fn fannkuchredux(n: i32) -> (i32, i32) {
    let mut perm1 = [0; 32];
    for i in 0..32 {
        perm1[i] = i as i32;
    }

    let mut perm = [0; 32];
    let mut count = [0; 32];
    let mut max_flips_count = 0;
    let mut perm_count = 0;
    let mut checksum = 0;
    let mut r = n as usize;

    loop {
        while r > 1 {
            count[r - 1] = r as i32;
            r -= 1;
        }

        perm[..n as usize].copy_from_slice(&perm1[..n as usize]);

        let mut flips_count = 0;
        let mut k = perm[0];

        while k != 0 {
            let k2 = (k + 1) >> 1;
            for i in 0..k2 as usize {
                let j = k as usize - i;
                perm.swap(i, j);
            }
            flips_count += 1;
            k = perm[0];
        }

        if flips_count > max_flips_count {
            max_flips_count = flips_count;
        }

        if perm_count % 2 == 0 {
            checksum += flips_count;
        } else {
            checksum -= flips_count;
        }

        loop {
            if r == n as usize {
                return (checksum, max_flips_count);
            }

            let perm0 = perm1[0];
            for i in 0..r {
                let j = i + 1;
                perm1.swap(i, j);
            }

            perm1[r] = perm0;
            count[r] -= 1;
            if count[r] > 0 {
                break;
            }
            r += 1;
        }

        perm_count += 1;
    }
}
fn mandelbrot(c: &support::Config) -> u32 {
    let w = c.number("w");
    let h = c.number("h");
    let wf = w as f64;
    let hf = h as f64;
    let mut result = Vec::new();
    for _ in 0..c.warmup + c.iterations {
        let mut header = String::from("P4\n");
        header.push_str(&w.to_string());
        header.push(' ');
        header.push_str(&h.to_string());
        header.push('\n');
        result.extend_from_slice(header.as_bytes());
        let mut bit = 0u8;
        let mut byte = 0u8;
        for y in 0..h {
            let ci = 2.0 * y as f64 / hf - 1.0;
            for x in 0..w {
                let cr = 2.0 * x as f64 / wf - 1.5;
                let mut zr = 0.0;
                let mut zi = 0.0;
                let mut tr = 0.0f64;
                let mut ti = 0.0f64;
                let mut i = 0;
                while i < 50 && (tr + ti).total_cmp(&4.0).is_le() {
                    zi = 2.0 * zr * zi + ci;
                    zr = tr - ti + cr;
                    tr = zr * zr;
                    ti = zi * zi;
                    i += 1;
                }
                byte <<= 1;
                if (tr + ti).total_cmp(&4.0).is_le() {
                    byte |= 1;
                }
                bit += 1;
                if bit == 8 || x == w - 1 {
                    if bit != 8 {
                        byte <<= u8::try_from(8 - w % 8).unwrap();
                    }
                    result.push(byte);
                    byte = 0;
                    bit = 0;
                }
            }
        }
    }
    result.iter().fold(5381u32, |h, b| {
        (h << 5).wrapping_add(h).wrapping_add(u32::from(*b))
    })
}
fn run(c: &support::Config) -> u32 {
    if c.name == "CLBG::Mandelbrot" {
        return mandelbrot(c);
    }
    assert_eq!(c.name, "CLBG::Fannkuchredux");
    let n = i32::try_from(c.number("n")).unwrap();
    let mut total = 0u32;
    for _ in 0..c.warmup + c.iterations {
        let (checksum, flips) = fannkuchredux(n);
        total = total.wrapping_add(
            (checksum as u32)
                .wrapping_mul(100)
                .wrapping_add(flips as u32),
        );
    }
    total
}
fn main() {
    support::main(run);
}
