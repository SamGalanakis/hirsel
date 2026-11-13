// Reviewed-by: langs-text-921 2026-11-13; all four tasks match inline arrays/planets, PBM retained bytes, spectral vector phases and float/checksum order; compile with overflow checks.
// Conversion parity delta reviewed by langs-text-921 2026-11-13: truncate then half-open float bounds, unsigned Fannkuch/matching checked index sites, u32 Mandelbrot counter; approval scoped to this delta.
// Matches clbg.hirsel Fannkuchredux: inline arrays, identical permutation/flip order.
#[allow(dead_code)]
mod support;
fn fannkuchredux(n: usize) -> (i32, i32) {
    let mut perm1 = [0; 32];
    for i in 0..32 {
        perm1[i] = i32::try_from(i).unwrap();
    }

    let mut perm = [0; 32];
    let mut count = [0; 32];
    let mut max_flips_count = 0;
    let mut perm_count = 0;
    let mut checksum = 0;
    let mut r = n;

    loop {
        while r > 1 {
            count[r - 1] = i32::try_from(r).unwrap();
            r -= 1;
        }

        perm[..n].copy_from_slice(&perm1[..n]);

        let mut flips_count = 0;
        let mut k = perm[0];

        while k != 0 {
            let k2 = (k + 1) >> 1;
            for i in 0..usize::try_from(k2).unwrap() {
                let j = usize::try_from(k).unwrap() - i;
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
            if r == n {
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
                let mut i = 0u32;
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
const SOLAR_MASS: f64 = 4.0 * std::f64::consts::PI * std::f64::consts::PI;
const DAYS_PER_YEAR: f64 = 365.24;

struct Planet {
    x: f64,
    y: f64,
    z: f64,
    vx: f64,
    vy: f64,
    vz: f64,
    mass: f64,
}

impl Planet {
    fn new(x: f64, y: f64, z: f64, vx: f64, vy: f64, vz: f64, mass: f64) -> Self {
        Self {
            x,
            y,
            z,
            vx: vx * DAYS_PER_YEAR,
            vy: vy * DAYS_PER_YEAR,
            vz: vz * DAYS_PER_YEAR,
            mass: mass * SOLAR_MASS,
        }
    }

    fn advance(&mut self, following: &mut [Planet], dt: f64) {
        for b2 in following {
            let dx = self.x - b2.x;
            let dy = self.y - b2.y;
            let dz = self.z - b2.z;

            let distance = (dx * dx + dy * dy + dz * dz).sqrt();
            let mag = dt / (distance * distance * distance);
            let b_mass_mag = self.mass * mag;
            let b2_mass_mag = b2.mass * mag;

            self.vx -= dx * b2_mass_mag;
            self.vy -= dy * b2_mass_mag;
            self.vz -= dz * b2_mass_mag;
            b2.vx += dx * b_mass_mag;
            b2.vy += dy * b_mass_mag;
            b2.vz += dz * b_mass_mag;
        }

        self.x += dt * self.vx;
        self.y += dt * self.vy;
        self.z += dt * self.vz;
    }
}

fn nbody_energy(bodies: &[Planet]) -> f64 {
    let mut e = 0.0;
    let nbodies = bodies.len();

    for i in 0..nbodies {
        let b = &bodies[i];
        e += 0.5 * b.mass * (b.vx * b.vx + b.vy * b.vy + b.vz * b.vz);

        for j in i + 1..nbodies {
            let b2 = &bodies[j];
            let dx = b.x - b2.x;
            let dy = b.y - b2.y;
            let dz = b.z - b2.z;
            let distance = (dx * dx + dy * dy + dz * dz).sqrt();
            e -= (b.mass * b2.mass) / distance;
        }
    }
    e
}

fn offset_momentum(bodies: &mut [Planet]) {
    let mut px = 0.0;
    let mut py = 0.0;
    let mut pz = 0.0;

    for b in bodies.iter() {
        let m = b.mass;
        px += b.vx * m;
        py += b.vy * m;
        pz += b.vz * m;
    }

    if let Some(b) = bodies.first_mut() {
        b.vx = -px / SOLAR_MASS;
        b.vy = -py / SOLAR_MASS;
        b.vz = -pz / SOLAR_MASS;
    }
}

fn create_bodies() -> [Planet; 5] {
    [
        Planet::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0),
        Planet::new(
            4.84143144246472090e+00,
            -1.16032004402742839e+00,
            -1.03622044471123109e-01,
            1.66007664274403694e-03,
            7.69901118419740425e-03,
            -6.90460016972063023e-05,
            9.54791938424326609e-04,
        ),
        Planet::new(
            8.34336671824457987e+00,
            4.12479856412430479e+00,
            -4.03523417114321381e-01,
            -2.76742510726862411e-03,
            4.99852801234917238e-03,
            2.30417297573763929e-05,
            2.85885980666130812e-04,
        ),
        Planet::new(
            1.28943695621391310e+01,
            -1.51111514016986312e+01,
            -2.23307578892655734e-01,
            2.96460137564761618e-03,
            2.37847173959480950e-03,
            -2.96589568540237556e-05,
            4.36624404335156298e-05,
        ),
        Planet::new(
            1.53796971148509165e+01,
            -2.59193146099879641e+01,
            1.79258772950371181e-01,
            2.68067772490389322e-03,
            1.62824170038242295e-03,
            -9.51592254519715870e-05,
            5.15138902046611451e-05,
        ),
    ]
}

fn nbody(c: &support::Config) -> u32 {
    let mut bodies = create_bodies();
    offset_momentum(&mut bodies);
    let v1 = nbody_energy(&bodies);
    for _ in 0..c.warmup + c.iterations {
        for _ in 0..1000 {
            for i in 0..5 {
                let (before, following) = bodies.split_at_mut(i + 1);
                before[i].advance(following, 0.01);
            }
        }
    }
    (support::checksum(&format!("{:.7}", v1)) << 5)
        & support::checksum(&format!("{:.7}", nbody_energy(&bodies)))
}
fn eval_a(i: usize, j: usize) -> f64 {
    let ij = (i + j) as f64;
    1.0 / (ij * (ij + 1.0) / 2.0 + i as f64 + 1.0)
}
fn times(u: &[f64], transpose: bool) -> Vec<f64> {
    let n = u.len();
    let mut result = vec![0.0; n];
    for i in 0..n {
        let mut sum = 0.0;
        for j in 0..n {
            sum += (if transpose {
                eval_a(j, i)
            } else {
                eval_a(i, j)
            }) * u[j];
        }
        result[i] = sum;
    }
    result
}
fn ata(u: &[f64]) -> Vec<f64> {
    let temp = times(u, false);
    times(&temp, true)
}
fn spectral(c: &support::Config) -> u32 {
    let n = c.number("size");
    let mut u = vec![1.0; n];
    let mut v = vec![1.0; n];
    for _ in 0..c.warmup + c.iterations {
        v = ata(&u);
        u = ata(&v);
    }
    let mut vbv = 0.0;
    let mut vv = 0.0;
    for i in 0..n {
        vbv += u[i] * v[i];
        vv += v[i] * v[i];
    }
    support::checksum(&format!("{:.7}", (vbv / vv).sqrt()))
}
fn run(c: &support::Config) -> u32 {
    if c.name == "CLBG::Nbody" {
        return nbody(c);
    }
    if c.name == "CLBG::Spectralnorm" {
        return spectral(c);
    }
    if c.name == "CLBG::Mandelbrot" {
        return mandelbrot(c);
    }
    assert_eq!(c.name, "CLBG::Fannkuchredux");
    let n = c.number("n");
    assert!((1..=32).contains(&n));
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
