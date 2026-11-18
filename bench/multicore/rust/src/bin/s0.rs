use std::{
    env,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Instant,
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
            let dispatch = env::var_os("HIRSEL_S8_STATS")
                .is_some_and(|value| value == "1")
                .then(|| {
                    Arc::new(
                        (0..workers)
                            .map(|_| (AtomicU64::new(0), AtomicU64::new(0)))
                            .collect::<Vec<_>>(),
                    )
                });
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
                    let sample = dispatch.clone();
                    let enqueued = sample.as_ref().map(|_| Instant::now());
                    pool.spawn(move || {
                        if let Some((sample, enqueued)) = sample.zip(enqueued) {
                            let (count, elapsed) = &sample[rayon::current_thread_index().unwrap()];
                            count.fetch_add(1, Ordering::Relaxed);
                            elapsed
                                .fetch_add(enqueued.elapsed().as_nanos() as u64, Ordering::Relaxed);
                        }
                        send.send(matmul::worker(job)).unwrap()
                    });
                    receive
                },
                |receive| receive.recv().unwrap(),
            );
            if let Some(dispatch) = dispatch {
                for (worker, (count, elapsed)) in dispatch.iter().enumerate() {
                    eprintln!(
                        "dispatch worker={worker} first_runs={} nanoseconds={}",
                        count.load(Ordering::Relaxed),
                        elapsed.load(Ordering::Relaxed)
                    );
                }
            }
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
