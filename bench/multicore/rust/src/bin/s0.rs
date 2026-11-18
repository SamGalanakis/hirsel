use std::{
    env,
    sync::{Arc, mpsc},
};

#[allow(dead_code)]
#[expect(
    clippy::manual_range_contains,
    clippy::needless_range_loop,
    reason = "Keep the reviewed LangArena indexing and half-open conversion checks"
)]
#[path = "../../../../../core/langarena/rust-matched/matmul.rs"]
mod matmul;

fn main() {
    let args: Vec<_> = env::args().collect();
    let workers = args[2].parse().unwrap();
    let rounds: u64 = args[3].parse().unwrap();
    let (completed, checksum) = match args[1].as_str() {
        "matmul_t16" => {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(workers)
                .build()
                .unwrap();
            let (checksum, completed) = matmul::parallel_work(
                256,
                16,
                rounds,
                |job| {
                    let (send, receive) = mpsc::sync_channel(1);
                    pool.spawn(move || send.send(matmul::worker(job)).unwrap());
                    receive
                },
                |receive| receive.recv().unwrap(),
            );
            (completed, u64::from(checksum))
        }
        "arc_pair" => {
            let original = Arc::new(1);
            let mut checksum = 0;
            let mut completed = 0;
            for i in 0..rounds {
                let shared = Arc::clone(&original);
                checksum += i + *shared;
                drop(shared);
                completed += 1;
            }
            (completed, checksum)
        }
        case => panic!("unknown workload: {case}"),
    };
    println!("{completed} {checksum}");
}
