// AWFY JavaScript mandelbrot.js and nbody.js.
namespace numeric;
import math;

fn mandelbrot(size: i64) -> bool {
    let expected = if size == 500 { 191 } else if size == 750 { 50 } else if size == 1 { 128 } else { -1 };
    if expected == -1 { return false; }
    var sum = 0;
    var byte = 0;
    var bits = 0;
    for y in size {
        let ci = 2.0 * y / size - 1.0;
        for x in size {
            let cr = 2.0 * x / size - 1.5;
            var zrzr = 0.0;
            var zi = 0.0;
            var zizi = 0.0;
            var step = 0;
            var escape = 0;
            while escape == 0 && step < 50 {
                let zr = zrzr - zizi + cr;
                zi = 2.0 * zr * zi + ci;
                zrzr = zr * zr;
                zizi = zi * zi;
                if zrzr + zizi > 4.0 { escape = 1; }
                step += 1;
            }
            byte = (byte << 1) + escape;
            bits += 1;
            if bits == 8 {
                sum ^= byte;
                byte = 0;
                bits = 0;
            } else if x == size - 1 {
                sum ^= byte << (8 - bits);
                byte = 0;
                bits = 0;
            }
        }
    }
    return sum == expected;
}

struct Body { x: f64, y: f64, z: f64, vx: f64, vy: f64, vz: f64, mass: f64 }
const PI = 3.141592653589793;
const SOLAR_MASS = 4.0 * PI * PI;

fn body(x: f64, y: f64, z: f64, vx: f64, vy: f64, vz: f64, mass: f64) -> Body {
    return Body { x: x, y: y, z: z, vx: vx * 365.24, vy: vy * 365.24,
                  vz: vz * 365.24, mass: mass * SOLAR_MASS };
}

fn create_bodies() -> Body[5] {
    var bodies: Body[5] = [
        body(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0),
        body(4.84143144246472090, -1.16032004402742839, -1.03622044471123109e-01,
             0.00166007664274403694, 0.00769901118419740425, -6.90460016972063023e-05, 0.000954791938424326609),
        body(8.34336671824457987, 4.12479856412430479, -0.403523417114321381,
             -0.00276742510726862411, 0.00499852801234917238, 0.0000230417297573763929, 0.000285885980666130812),
        body(12.8943695621391310, -15.1111514016986312, -0.223307578892655734,
             0.00296460137564761618, 0.00237847173959480950, -0.0000296589568540237556, 0.0000436624404335156298),
        body(15.3796971148509165, -25.9193146099879641, 0.179258772950371181,
             0.00268067772490389322, 0.00162824170038242295, -0.0000951592254519715807, 0.0000515138902046611451)
    ];
    var px = 0.0;
    var py = 0.0;
    var pz = 0.0;
    for i in bodies.len {
        let b = bodies[i];
        px += b.vx * b.mass;
        py += b.vy * b.mass;
        pz += b.vz * b.mass;
    }
    bodies[0].vx = 0.0 - px / SOLAR_MASS;
    bodies[0].vy = 0.0 - py / SOLAR_MASS;
    bodies[0].vz = 0.0 - pz / SOLAR_MASS;
    return bodies;
}

fn advance(bodies: Body[5]&, dt: f64) {
    for i in 5 {
        for j in i + 1..5 {
            let first .= bodies[i];
            let second .= bodies[j];
            let dx = first.x - second.x;
            let dy = first.y - second.y;
            let dz = first.z - second.z;
            let squared = dx * dx + dy * dy + dz * dz;
            let mag = dt / (squared * sqrt(squared));
            first.vx -= dx * second.mass * mag;
            first.vy -= dy * second.mass * mag;
            first.vz -= dz * second.mass * mag;
            second.vx += dx * first.mass * mag;
            second.vy += dy * first.mass * mag;
            second.vz += dz * first.mass * mag;
        }
    }
    for i in 5 {
        let b .= bodies[i];
        b.x += dt * b.vx;
        b.y += dt * b.vy;
        b.z += dt * b.vz;
    }
}

fn energy(bodies: Body[5]&) -> f64 {
    var total = 0.0;
    for i in 5 {
        let b .= bodies[i];
        total += 0.5 * b.mass * (b.vx * b.vx + b.vy * b.vy + b.vz * b.vz);
        for j in i + 1..5 {
            let other .= bodies[j];
            let dx = b.x - other.x;
            let dy = b.y - other.y;
            let dz = b.z - other.z;
            total -= b.mass * other.mass / sqrt(dx * dx + dy * dy + dz * dz);
        }
    }
    return total;
}

fn nbody(steps: i64) -> bool {
    if steps != 1 && steps != 250000 { return false; }
    let expected = if steps == 1 { -0.16907495402506745 } else { -0.1690859889909308 };
    var bodies = create_bodies();
    for _ in steps { advance(bodies, 0.01); }
    return energy(bodies) == expected;
}
