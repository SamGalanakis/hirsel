// Reviewed-by: langs-text-921 2026-11-13; Single/T4/T8/T16: nested f64 rows, transpose and triple-loop order, Arc inputs, all task row partitions retained, reverse joins and center-block checksum; overflow checks required.
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
struct Matrix {
    rows: Vec<Vec<f64>>,
}
struct Job {
    a: std::sync::Arc<Matrix>,
    transposed: std::sync::Arc<Matrix>,
    start: usize,
    end: usize,
}
struct Block {
    start: usize,
    rows: Vec<Vec<f64>>,
}
fn worker(job: Job) -> Block {
    let n = job.a.rows.len();
    let mut rows = Vec::with_capacity(job.end - job.start);
    for i in job.start..job.end {
        let mut row = vec![0.0; n];
        for j in 0..n {
            let mut sum = 0.0;
            for k in 0..n {
                sum += job.a.rows[i][k] * job.transposed.rows[j][k];
            }
            row[j] = sum;
        }
        rows.push(row);
    }
    Block {
        start: job.start,
        rows,
    }
}
fn parallel(a: &std::sync::Arc<Matrix>, b: &[Vec<f64>], threads: usize) -> f64 {
    let n = a.rows.len();
    let mut transposed = zeros(n);
    for i in 0..n {
        for j in 0..n {
            transposed[j][i] = b[i][j];
        }
    }
    let columns = std::sync::Arc::new(Matrix { rows: transposed });
    let mut jobs = Vec::with_capacity(threads);
    for i in 0..threads {
        let job = Job {
            a: std::sync::Arc::clone(a),
            transposed: std::sync::Arc::clone(&columns),
            start: i * n / threads,
            end: (i + 1) * n / threads,
        };
        jobs.push(std::thread::spawn(move || worker(job)));
    }
    let mut blocks = Vec::with_capacity(threads);
    while let Some(handle) = jobs.pop() {
        blocks.push(handle.join().unwrap());
    }
    let center = n / 2;
    for block in &blocks {
        if center >= block.start && center < block.start + block.rows.len() {
            return block.rows[center - block.start][center];
        }
    }
    panic!("missing center row")
}
fn run(c: &support::Config) -> u32 {
    if c.name != "Matmul::Single" {
        let threads = match c.name.as_str() {
            "Matmul::T4" => 4,
            "Matmul::T8" => 8,
            "Matmul::T16" => 16,
            _ => panic!("unknown task"),
        };
        let n = c.number("n");
        let a = std::sync::Arc::new(Matrix { rows: matrix(n) });
        let b = matrix(n);
        let mut total = 0u32;
        for _ in 0..c.warmup + c.iterations {
            total = total.wrapping_add(support::checksum(&format!(
                "{:.7}",
                parallel(&a, &b, threads)
            )));
        }
        return total;
    }
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
