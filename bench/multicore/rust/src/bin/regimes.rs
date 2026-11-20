use std::{env, sync::Arc, time::Instant};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc::{Receiver, Sender, channel},
    task::{JoinHandle, spawn},
    time::{Duration, sleep_until},
};

const LANES: u64 = 16;

#[inline(never)]
fn kernel(mut x: u64, rounds: u64) -> u64 {
    for _ in 0..rounds {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
    }
    x
}

async fn sum(mut jobs: Vec<JoinHandle<u64>>) -> u64 {
    let mut sum = 0u64;
    while let Some(job) = jobs.pop() {
        sum = sum.wrapping_add(job.await.unwrap());
    }
    sum
}

async fn join(mut jobs: Vec<JoinHandle<()>>) {
    while let Some(job) = jobs.pop() {
        job.await.unwrap();
    }
}

async fn independent(lanes: u64, waves: u64, grain: u64) -> u64 {
    let mut checksum = 0u64;
    for wave in 0..waves {
        let jobs = (0..lanes)
            .map(|lane| spawn(async move { kernel(wave * lanes + lane + 1, grain) }))
            .collect();
        checksum = checksum.wrapping_add(sum(jobs).await);
    }
    checksum
}

async fn stage(mut input: Receiver<u64>, output: Sender<u64>, count: u64, grain: u64) {
    for _ in 0..count {
        output
            .send(kernel(input.recv().await.unwrap(), grain))
            .await
            .unwrap();
    }
}

async fn pair(start: u64, count: u64, grain: u64) -> u64 {
    let (tx, rx) = channel(1);
    let (out, mut reply) = channel(1);
    let job = spawn(stage(rx, out, count, grain));
    let mut sum = 0u64;
    for i in 0..count {
        tx.send(start + i).await.unwrap();
        sum = sum.wrapping_add(reply.recv().await.unwrap());
    }
    job.await.unwrap();
    sum
}

async fn chain(start: u64, count: u64, grain: u64) -> u64 {
    let mut sum = 0u64;
    for i in 0..count {
        sum = sum.wrapping_add(
            spawn(async move { kernel(start + i, grain) })
                .await
                .unwrap(),
        );
    }
    sum
}

async fn produce(output: Sender<u64>, start: u64, count: u64, grain: u64) {
    for i in 0..count {
        output.send(kernel(start + i, grain)).await.unwrap();
    }
}

async fn collect(mut input: Receiver<u64>, count: u64) -> u64 {
    let mut sum = 0u64;
    for _ in 0..count {
        sum = sum.wrapping_add(input.recv().await.unwrap());
    }
    sum
}

async fn fan_in(count: u64, grain: u64) -> u64 {
    let (out, input) = channel(64);
    let jobs = (0..LANES)
        .map(|lane| spawn(produce(out.clone(), lane * count + 1, count, grain)))
        .collect();
    let sum = collect(input, LANES * count).await;
    join(jobs).await;
    sum
}

async fn fan_out(count: u64, grain: u64) -> u64 {
    let (out, input) = channel(64);
    let mut jobs = Vec::new();
    let mut outputs = Vec::new();
    for _ in 0..LANES {
        let (tx, rx) = channel(64);
        outputs.push(tx);
        jobs.push(spawn(stage(rx, out.clone(), count, grain)));
    }
    jobs.push(spawn(async move {
        for i in 0..count {
            for (lane, output) in outputs.iter().enumerate() {
                output.send(i * LANES + lane as u64 + 1).await.unwrap();
            }
        }
    }));
    let sum = collect(input, LANES * count).await;
    join(jobs).await;
    sum
}

async fn pipeline(count: u64, grain: u64) -> u64 {
    let (sender, mut input) = channel(64);
    let mut jobs = Vec::new();
    for _ in 0..8 {
        let (out, next) = channel(64);
        jobs.push(spawn(stage(input, out, count, grain)));
        input = next;
    }
    jobs.push(spawn(produce(sender, 1, count, 0)));
    let sum = collect(input, count).await;
    join(jobs).await;
    sum
}

fn tree(start: u64, leaves: u64, grain: u64) -> impl Future<Output = u64> + Send {
    Box::pin(async move {
        if leaves == 1 {
            return kernel(start, grain);
        }
        let half = leaves / 2;
        let a = spawn(tree(start, half, grain));
        let b = spawn(tree(start + half, half, grain));
        a.await.unwrap().wrapping_add(b.await.unwrap())
    })
}

async fn forest(waves: u64, grain: u64) -> u64 {
    let mut sum = 0u64;
    for wave in 0..waves {
        sum = sum.wrapping_add(tree(wave * 64 + 1, 64, grain).await);
    }
    sum
}

async fn hub(count: u64, grain: u64) -> u64 {
    let (request, mut requests) = channel(1);
    let (release, mut releases) = channel(1);
    let mut replies = Vec::new();
    let mut jobs = Vec::new();
    for lane in 0..LANES {
        let (grant, mut permit) = channel(1);
        replies.push(grant);
        let (request, release) = (request.clone(), release.clone());
        jobs.push(spawn(async move {
            let mut sum = 0u64;
            for i in 0..count {
                let value = kernel(lane * count + i + 1, grain);
                request.send(lane).await.unwrap();
                permit.recv().await.unwrap();
                sum = sum.wrapping_add(value);
                release.send(()).await.unwrap();
            }
            sum
        }));
    }
    let broker = spawn(async move {
        for _ in 0..LANES * count {
            let lane = requests.recv().await.unwrap();
            replies[lane as usize].send(()).await.unwrap();
            releases.recv().await.unwrap();
        }
    });
    let checksum = sum(jobs).await;
    broker.await.unwrap();
    checksum
}

async fn arc_hub(count: u64, grain: u64) -> u64 {
    let shared = Arc::new(1u64);
    sum((0..LANES)
        .map(|lane| {
            let shared = Arc::clone(&shared);
            spawn(async move {
                let mut sum = 0u64;
                for i in 0..count {
                    let value = kernel(lane * count + i + 1, grain);
                    let reference = Arc::clone(&shared);
                    sum = sum.wrapping_add(value.wrapping_add(*reference));
                }
                sum
            })
        })
        .collect())
    .await
}

async fn timers(count: u64, grain: u64, hot: u64) -> u64 {
    let hot_task = (hot != 0).then(|| {
        spawn(async move {
            let started = Instant::now();
            let checksum = pair(count + 1, hot, grain).await;
            (checksum, started.elapsed())
        })
    });
    let mut jobs = Vec::with_capacity(count as usize);
    for i in 0..count {
        jobs.push(spawn(async move {
            let deadline = Instant::now() + Duration::from_micros(2000 + i);
            sleep_until(deadline.into()).await;
            let lag = deadline.elapsed().as_nanos();
            (kernel(i + 1, if hot == 0 { grain } else { 0 }), lag)
        }));
    }
    let mut sum = 0u64;
    let mut lag = 0;
    let mut worst = 0;
    while let Some(job) = jobs.pop() {
        let (value, ns) = job.await.unwrap();
        sum = sum.wrapping_add(value);
        lag += ns;
        worst = worst.max(ns);
    }
    if let Some(job) = hot_task {
        let (value, duration) = job.await.unwrap();
        sum = sum.wrapping_add(value);
        eprintln!("hot-ns={}", duration.as_nanos());
    }
    eprintln!("wake-mean-ns={}", lag / u128::from(count));
    eprintln!("wake-max-ns={worst}");
    sum
}

async fn run(name: &str, count: u64, grain: u64) -> (u64, u64) {
    match name {
        "independent16" => (16 * count, independent(16, count, grain).await),
        "independent256" => (256 * count, independent(256, count, grain).await),
        "pairs" => (
            LANES * count,
            sum((0..LANES)
                .map(|lane| spawn(pair(lane * count + 1, count, grain)))
                .collect())
            .await,
        ),
        "single_pair" => (count, pair(1, count, grain).await),
        "fanin" => (LANES * count, fan_in(count, grain).await),
        "fanout" => (LANES * count, fan_out(count, grain).await),
        "pipeline" => (count, pipeline(count, grain).await),
        "grain_spawn_join" => (count, chain(1, count, grain).await),
        "grain_spawn_chain" => (
            LANES * count,
            sum((0..LANES)
                .map(|lane| spawn(chain(lane * count + 1, count, grain)))
                .collect())
            .await,
        ),
        "spawn_tree" => (64 * count, forest(count, grain).await),
        "channel_hub" => (LANES * count, hub(count, grain).await),
        "arc_hub" => (LANES * count, arc_hub(count, grain).await),
        "skew" => (1000 + count, timers(1000, grain, count).await),
        "timers" => (count, timers(count, grain, 0).await),
        "calibrate" => {
            let started = Instant::now();
            let sum = (1..=count).fold(0u64, |sum, seed| sum.wrapping_add(kernel(seed, grain)));
            eprintln!("kernel-ns={}", started.elapsed().as_nanos());
            (count, sum)
        }
        _ => panic!("unknown workload {name}"),
    }
}

async fn socket_server(port: u16, connections: u64, count: u64, grain: u64) -> (u64, u64) {
    let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    eprintln!("listening={}", listener.local_addr().unwrap().port());
    let mut jobs = Vec::new();
    for _ in 0..connections {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.set_nodelay(true).unwrap();
        jobs.push(spawn(async move {
            let mut sum = 0u64;
            for _ in 0..count {
                let value = kernel(socket.read_u64_le().await.unwrap(), grain);
                socket.write_u64_le(value).await.unwrap();
                sum = sum.wrapping_add(value);
            }
            sum
        }));
    }
    (connections * count, sum(jobs).await)
}

async fn socket_load(port: u16, connections: u64, count: u64) -> (u64, u64) {
    let mut jobs = Vec::new();
    // Connect before releasing the request loops so each socket can contend.
    let barrier = Arc::new(tokio::sync::Barrier::new(connections as usize + 1));
    for lane in 0..connections {
        let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        socket.set_nodelay(true).unwrap();
        let barrier = Arc::clone(&barrier);
        jobs.push(spawn(async move {
            let mut sum = 0u64;
            let mut latencies = Vec::with_capacity(count as usize);
            barrier.wait().await;
            for i in 0..count {
                let start = Instant::now();
                socket.write_u64_le(lane * count + i + 1).await.unwrap();
                sum = sum.wrapping_add(socket.read_u64_le().await.unwrap());
                latencies.push(start.elapsed().as_nanos());
            }
            (sum, latencies)
        }));
    }
    let started = Instant::now();
    barrier.wait().await;
    let mut sum = 0u64;
    let mut all = Vec::new();
    for job in jobs {
        let (checksum, latencies) = job.await.unwrap();
        sum = sum.wrapping_add(checksum);
        all.extend(latencies);
    }
    eprintln!("socket-ns={}", started.elapsed().as_nanos());
    all.sort_unstable();
    for percentile in [50, 99] {
        let rank = (all.len() * percentile).div_ceil(100);
        eprintln!("rtt-p{percentile}-ns={}", all[rank - 1]);
    }
    (connections * count, sum)
}

fn main() {
    let args: Vec<_> = env::args().collect();
    let workers = args[2].parse().unwrap();
    let count = args[3].parse().unwrap();
    let grain = args[4].parse().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()
        .unwrap();
    let (completed, sum) = runtime.block_on(async {
        match args[1].as_str() {
            "socket_server" => {
                socket_server(
                    args[5].parse().unwrap(),
                    args[6].parse().unwrap(),
                    count,
                    grain,
                )
                .await
            }
            "socket_load" => {
                socket_load(args[5].parse().unwrap(), args[6].parse().unwrap(), count).await
            }
            name => run(name, count, grain).await,
        }
    });
    println!("{completed} {sum}");
}
