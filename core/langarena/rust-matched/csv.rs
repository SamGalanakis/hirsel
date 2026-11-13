// Reviewed-by: langs-numeric-920 2026-11-13; valid generated CSV uses same quoted-byte scanner, inline3 numeric fields, owned Point vector, LCG/fixed10 generation and fixed7 reductions; overflow checks required.
// Matched twin of csv.hirsel. Reviewed below.
// kostya/LangArena3c9f84d benchmarks/csv_parse.rs:30-115.
// Same quoted field scanner, fixed10 generator and owned Point vector.
mod text_support;
struct Point {
    x: f64,
    y: f64,
    z: f64,
}
fn next(g: &mut i32) -> f64 {
    *g = g.wrapping_mul(3877).wrapping_add(29573) % 139968;
    f64::from(*g) / 139968.0
}
fn generate(rows: u64) -> String {
    let mut out = String::new();
    let mut g = 42;
    for i in 0..rows {
        let c = char::from_u32(65 + u32::try_from(i % 26).unwrap()).unwrap();
        let x = next(&mut g);
        let z = next(&mut g);
        let y = next(&mut g);
        out.push_str(&format!(
            "\"point {c}\\n, \"\"{}\"\"\",{:.10},,{:.10},\"[{}\\n, {}]\",{:.10}\n",
            i % 100,
            x,
            z,
            if i % 2 == 0 { "true" } else { "false" },
            i % 100,
            y
        ));
    }
    out
}
fn parse(source: &str) -> Vec<Point> {
    let b = source.as_bytes();
    let n = b.len();
    let mut points = Vec::new();
    let mut i = 0;
    let mut field = 0;
    let mut values = [0.0; 3];
    while i < n {
        let quoted = b[i] == b'"';
        if quoted {
            i += 1;
            loop {
                assert!(i < n);
                if b[i] == b'"' {
                    i += 1;
                    if i < n && b[i] == b'"' {
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else {
            let start = i;
            while i < n && b[i] != b',' && b[i] != b'\n' {
                i += 1;
            }
            if field == 1 || field == 3 || field == 5 {
                let number: f64 = source[start..i].parse().unwrap();
                let slot = if field == 1 {
                    0
                } else if field == 3 {
                    2
                } else {
                    1
                };
                values[slot] = number;
            }
        }
        if i == n || b[i] == b'\n' {
            assert_eq!(field, 5);
            points.push(Point {
                x: values[0],
                y: values[1],
                z: values[2],
            });
            field = 0;
        } else if b[i] == b',' {
            field += 1;
        } else {
            panic!("malformed CSV");
        }
        i += 1;
    }
    points
}
fn checksum(s: &str) -> u32 {
    s.bytes().fold(5381u32, |h, b| {
        (h << 5).wrapping_add(h).wrapping_add(u32::from(b))
    })
}
fn main() {
    let a = text_support::arguments(&["rows"], false);
    let rows: u64 = a[1].parse().unwrap();
    let warmup: u64 = a[2].parse().unwrap();
    let iterations: u64 = a[3].parse().unwrap();
    let source = generate(rows);
    let mut result = 0u32;
    for _ in 0..warmup.checked_add(iterations).unwrap() {
        let points = parse(&source);
        if points.is_empty() {
            continue;
        }
        let mut x = 0.0;
        let mut y = 0.0;
        let mut z = 0.0;
        for p in &points {
            x += p.x;
            y += p.y;
            z += p.z;
        }
        let count = points.len() as f64;
        result = result
            .wrapping_add(checksum(&format!("{:.7}", x / count)))
            .wrapping_add(checksum(&format!("{:.7}", y / count)))
            .wrapping_add(checksum(&format!("{:.7}", z / count)));
    }
    text_support::output(result.wrapping_add(checksum(&source)));
}
