// Reviewed-by: langs-text-921 2026-11-13; inline scene/LUT, Vec3 arithmetic, total-order branches, sphere order and pixel sum match valid positive dimensions; compile with overflow checks.
// Conversion parity delta reviewed by langs-text-921 2026-11-13: truncate then half-open float bounds, unsigned Fannkuch/matching checked index sites, u32 Mandelbrot counter; approval scoped to this delta.
// Matches text_raytracer.hirsel: inline three-sphere scene and f64 triple arithmetic.
#[allow(dead_code)]
mod support;
#[derive(Clone, Copy)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}
#[derive(Clone, Copy)]
struct Sphere {
    center: Vec3,
    radius: f64,
    color: Vec3,
}
fn vector(x: f64, y: f64, z: f64) -> Vec3 {
    Vec3 { x, y, z }
}
fn scale(v: Vec3, s: f64) -> Vec3 {
    vector(v.x * s, v.y * s, v.z * s)
}
fn add(a: Vec3, b: Vec3) -> Vec3 {
    vector(a.x + b.x, a.y + b.y, a.z + b.z)
}
fn sub(a: Vec3, b: Vec3) -> Vec3 {
    vector(a.x - b.x, a.y - b.y, a.z - b.z)
}
fn dot(a: Vec3, b: Vec3) -> f64 {
    a.x * b.x + a.y * b.y + a.z * b.z
}
fn normal(v: Vec3) -> Vec3 {
    scale(v, 1.0 / dot(v, v).sqrt())
}
fn below(a: f64, b: f64) -> bool {
    a.total_cmp(&b).is_lt()
}
fn above(a: f64, b: f64) -> bool {
    a.total_cmp(&b).is_gt()
}
fn intersect(dir: Vec3, obj: Sphere) -> Option<f64> {
    let tca = dot(obj.center, dir);
    if below(tca, 0.0) {
        return None;
    }
    let d2 = dot(obj.center, obj.center) - tca * tca;
    let r2 = obj.radius * obj.radius;
    if above(d2, r2) {
        return None;
    }
    let t0 = tca - (r2 - d2).sqrt();
    if above(t0, 10000.0) { None } else { Some(t0) }
}
fn shade(dir: Vec3, obj: Sphere, t: f64) -> usize {
    let pi = add(vector(0.0, 0.0, 0.0), scale(dir, t));
    let n = normal(sub(pi, obj.center));
    let light_dir = normal(sub(vector(0.7, -1.0, 1.7), pi));
    let lam1 = dot(light_dir, n);
    let lam2 = if below(lam1, 0.0) {
        0.0
    } else if above(lam1, 1.0) {
        1.0
    } else {
        lam1
    };
    let color = add(
        scale(vector(1.0, 1.0, 1.0), lam2 * 0.5),
        scale(obj.color, 0.3),
    );
    let col = (color.x + color.y + color.z) / 3.0;
    usize::try_from(support::float_u64((col * 6.0).floor())).unwrap()
}
fn run(c: &support::Config) -> u32 {
    assert_eq!(c.name, "Etc::TextRaytracer");
    let w = c.number("w");
    let h = c.number("h");
    assert!(w > 0 && h > 0);
    let fw = w as f64;
    let fh = h as f64;
    let scene = [
        Sphere {
            center: vector(-1.0, 0.0, 3.0),
            radius: 0.3,
            color: vector(1.0, 0.0, 0.0),
        },
        Sphere {
            center: vector(0.0, 0.0, 3.0),
            radius: 0.8,
            color: vector(0.0, 1.0, 0.0),
        },
        Sphere {
            center: vector(1.0, 0.0, 3.0),
            radius: 0.4,
            color: vector(0.0, 0.0, 1.0),
        },
    ];
    let lut = [46u32, 45, 43, 42, 88, 77];
    let mut total = 0u32;
    for _ in 0..c.warmup + c.iterations {
        for j in 0..h {
            let fj = j as f64;
            for i in 0..w {
                let fi = i as f64;
                let dir = normal(vector((fi - fw / 2.0) / fw, (fj - fh / 2.0) / fh, 1.0));
                let mut pixel = 32u32;
                for obj in scene {
                    if let Some(t) = intersect(dir, obj) {
                        let at = shade(dir, obj, t);
                        if at < 6 {
                            pixel = lut[at];
                        }
                        break;
                    }
                }
                total = total.wrapping_add(pixel);
            }
        }
    }
    total
}
fn main() {
    support::main(run);
}
