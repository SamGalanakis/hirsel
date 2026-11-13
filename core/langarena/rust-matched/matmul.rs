// Review pending: langs-text-921. No matched ratio before independent review.
// Matches matmul.hirsel Single: nested f64 rows, explicit transpose, checked indices.
#[allow(dead_code)]
mod support;
fn matrix(n: usize) -> Vec<Vec<f64>> {
    let scale = 1.0 / (n * n) as f64;
    let mut rows = Vec::with_capacity(n);
    for i in 0..n {
        let fi = i as f64;
        let mut row = vec![0.0; n];
        for j in 0..n {
            let fj = j as f64;
            row[j] = scale * (fi - fj) * (fi + fj);
        }
        rows.push(row);
    }
    rows
}
fn zeros(n: usize) -> Vec<Vec<f64>> {
    let mut rows = Vec::with_capacity(n);
    for _ in 0..n {
        rows.push(vec![0.0; n]);
    }
    rows
}
fn multiply(a: &[Vec<f64>], b: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let n = a.len();
    let mut transposed = zeros(n);
    for i in 0..n {
        for j in 0..n {
            transposed[j][i] = b[i][j];
        }
    }
    let mut c = zeros(n);
    for i in 0..n {
        for j in 0..n {
            let mut sum = 0.0;
            for k in 0..n {
                sum += a[i][k] * transposed[j][k];
            }
            c[i][j] = sum;
        }
    }
    c
}
fn run(c: &support::Config) -> u32 {
    assert_eq!(c.name, "Matmul::Single");
    let n = c.number("n");
    assert!(n > 0);
    let a = matrix(n);
    let b = matrix(n);
    let mut total = 0u32;
    for _ in 0..c.warmup + c.iterations {
        let result = multiply(&a, &b);
        total = total.wrapping_add(support::checksum(&format!("{:.7}", result[n / 2][n / 2])));
    }
    total
}
fn main() {
    support::main(run);
}
