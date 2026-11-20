use std::env;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

const LANES: u64 = 16;

async fn spawn_chain(rounds: u64) -> u64 {
    let mut lanes = JoinSet::new();
    for lane in 0..LANES {
        lanes.spawn(async move {
            let mut sum = 0;
            for i in 0..rounds {
                sum += tokio::spawn(async move { lane * rounds + i + 1 })
                    .await
                    .unwrap();
            }
            sum
        });
    }
    let mut sum = 0;
    while let Some(result) = lanes.join_next().await {
        sum += result.unwrap();
    }
    sum
}

async fn spawn_join(rounds: u64) -> (u64, u64) {
    let mut sum = 0;
    let mut completed = 0;
    for i in 0..rounds {
        sum += tokio::spawn(async move { i + 1 }).await.unwrap();
        completed += 1;
    }
    (sum, completed)
}

async fn ping_pong(rounds: u64, batch: usize) -> (u64, u64) {
    let (send, mut requests) = mpsc::channel(batch);
    let (reply, mut responses) = mpsc::channel(batch);
    let worker = tokio::spawn(async move {
        for _ in 0..rounds {
            reply
                .send(requests.recv().await.unwrap() + 1)
                .await
                .unwrap();
        }
    });
    let mut sum = 0;
    let mut completed = 0;
    while completed < rounds {
        let count = (rounds - completed).min(batch as u64);
        for i in 0..count {
            send.send(completed + i).await.unwrap();
        }
        for _ in 0..count {
            sum += responses.recv().await.unwrap();
        }
        completed += count;
    }
    worker.await.unwrap();
    (sum, completed)
}

async fn yield_many(rounds: u64) -> u64 {
    let mut lanes = JoinSet::new();
    for lane in 0..LANES {
        lanes.spawn(async move {
            let mut sum = 0;
            for i in 0..rounds {
                tokio::task::yield_now().await;
                sum += lane * rounds + i + 1;
            }
            sum
        });
    }
    let mut sum = 0;
    while let Some(result) = lanes.join_next().await {
        sum += result.unwrap();
    }
    sum
}

async fn fan_in(rounds: u64, capacity: usize) -> u64 {
    let (send, mut receive) = mpsc::channel(capacity);
    let mut lanes = JoinSet::new();
    for lane in 0..LANES {
        let send = send.clone();
        lanes.spawn(async move {
            for i in 0..rounds {
                send.send(lane * rounds + i + 1).await.unwrap();
            }
        });
    }
    drop(send);
    let mut sum = 0;
    while let Some(value) = receive.recv().await {
        sum += value;
    }
    while let Some(result) = lanes.join_next().await {
        result.unwrap();
    }
    sum
}

fn main() {
    let args: Vec<_> = env::args().collect();
    let workers = args[2].parse().unwrap();
    let rounds = args[3].parse().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .build()
        .unwrap();
    // The driver runs on a runtime worker like the Hirsel and Go drivers;
    // main only waits, so no round crosses the injector to the main thread.
    let case = args[1].clone();
    let driver = runtime.spawn(async move {
        match case.as_str() {
            "spawn_join" => spawn_join(rounds).await,
            "spawn_chain" => (spawn_chain(rounds).await, 0),
            "ping_pong" => ping_pong(rounds, 1).await,
            "ping_pong_burst" => ping_pong(rounds, 64).await,
            "yield_many" => (yield_many(rounds).await, 0),
            "fan_in" => (fan_in(rounds, 64).await, 0),
            "fan_in_cap1" => (fan_in(rounds, 1).await, 0),
            case => panic!("unknown workload: {case}"),
        }
    });
    let (sum, completed) = runtime.block_on(driver).unwrap();
    if args.get(4).is_some_and(|mode| mode == "work-receipt") {
        println!("{completed} {sum}");
    } else {
        println!("{sum}");
    }
}
