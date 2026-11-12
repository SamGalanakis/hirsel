use crate::support::*;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
pub trait Batch: Send + 'static {
    fn new(id: usize, rows: Vec<String>, released: mpsc::Sender<usize>) -> Self;
    fn upper(&mut self);
    fn stats(&self) -> (usize, usize, u64);
}
fn publish<B>(tx: &SyncSender<B>, batch: B) -> bool {
    tx.send(batch).is_ok()
}
pub fn run<B: Batch>() {
    let (input, variant) = input();
    let mut lines = input.lines();
    let batch_size: usize = lines.next().unwrap().parse().unwrap();
    let rows: Vec<String> = lines.map(String::from).collect();
    let count = rows.len().div_ceil(batch_size);
    let failed = variant != "normal";
    let limit = if variant == "cancel-receive" {
        1
    } else if failed {
        2
    } else {
        count
    };
    let (released_tx, released_rx) = mpsc::channel();
    let (tx1, rx1) = mpsc::sync_channel::<B>(8);
    let (tx2, rx2) = mpsc::sync_channel::<B>(8);
    let (start_tx, start_rx) = mpsc::sync_channel(0);
    let (full_tx, full_rx) = mpsc::sync_channel(0);
    let (sink_tx, sink_rx) = mpsc::sync_channel(0);
    let (cancel_tx, cancel_rx) = mpsc::sync_channel(0);
    let source_variant = variant.clone();
    let transform_variant = variant.clone();
    let mut source = Some(thread::spawn(move || {
        let mut sent = 0;
        let mut full = false;
        for (id, chunk) in rows.chunks(batch_size).take(limit).enumerate() {
            let batch = B::new(id + 1, chunk.to_vec(), released_tx.clone());
            if source_variant == "cancel-send" && id == 1 {
                tx1.send(batch).ok();
                for id in 3..=8 {
                    tx1.send(B::new(id, vec!["cancel".into()], released_tx.clone()))
                        .ok();
                }
                let batch = B::new(9, vec!["cancel".into()], released_tx.clone());
                let pending = match tx1.try_send(batch) {
                    Err(TrySendError::Full(b)) => b,
                    _ => panic!("gate did not fill q1"),
                };
                full_tx.send(()).unwrap();
                cancel_rx.recv().unwrap();
                drop(pending);
                return (8, true);
            }
            if !failed && id == 8 {
                match tx1.try_send(batch) {
                    Err(TrySendError::Full(b)) => {
                        full = true;
                        full_tx.send(()).unwrap();
                        if !publish(&tx1, b) {
                            break;
                        }
                    }
                    _ => panic!("gate did not fill q1"),
                }
            } else if !publish(&tx1, batch) {
                break;
            }
            sent += 1;
        }
        if !failed && count <= 8 {
            full_tx.send(()).unwrap();
        }
        (sent, full)
    }));
    let transform = thread::spawn(move || {
        start_rx.recv().unwrap();
        if transform_variant == "cancel-send" {
            for batch in rx1.try_iter() {
                drop(batch);
            }
            drop(rx1);
            drop(tx2);
            return (0, Some("Cancelled@source.send".into()));
        }
        let mut seen = 0usize;
        while let Ok(mut batch) = rx1.recv() {
            seen += 1;
            if seen == 2 && transform_variant != "normal" {
                drop(batch);
                drop(rx1);
                drop(tx2);
                return (
                    seen,
                    Some(
                        match transform_variant.as_str() {
                            "capacity:batch2.output" => "Capacity@batch2.output",
                            "error" => "Transform@batch2",
                            "cancel-send" => "Cancelled@source.send",
                            _ => "Cancelled@transform.check",
                        }
                        .to_string(),
                    ),
                );
            }
            batch.upper();
            if !publish(&tx2, batch) {
                break;
            }
        }
        if transform_variant == "cancel-receive" {
            return (seen, Some("Cancelled@transform.receive".into()));
        }
        (seen, None)
    });
    let sink = thread::spawn(move || {
        sink_rx.recv().unwrap();
        consume(rx2)
    });
    if variant == "cancel-send" || variant == "normal" {
        full_rx.recv().unwrap();
    }
    // Failure fixtures publish two batches before the transform starts. The sink starts
    // after transform completion, giving an exact observable prefix and queue custody.
    let source_result = if failed && variant != "cancel-send" {
        Some(source.take().unwrap().join().unwrap())
    } else {
        None
    };
    start_tx.send(()).unwrap();
    let result = if failed {
        let result = transform.join().unwrap();
        sink_tx.send(()).unwrap();
        result
    } else {
        sink_tx.send(()).unwrap();
        transform.join().unwrap()
    };
    if variant == "cancel-send" {
        cancel_tx.send(()).unwrap();
    }
    let (sent, full) = if let Some(result) = source_result {
        result
    } else {
        source.take().unwrap().join().unwrap()
    };
    let (batches, rows, bytes, checksum) = sink.join().unwrap();
    let mut released: Vec<_> = released_rx.into_iter().collect();
    if failed {
        println!(
            "release-order={}",
            released
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    released.sort_unstable();
    println!(
        "sent={sent} transformed={} sink-batches={batches} rows={rows} bytes={bytes} sum={checksum}",
        if result.1.is_some() && variant != "cancel-receive" {
            result.0.saturating_sub(1)
        } else {
            result.0
        }
    );
    println!("backpressure={full}");
    println!(
        "released={}",
        released
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    finish(result.1.map_or(Ok(()), Err));
}
fn consume<B: Batch>(rx: Receiver<B>) -> (usize, usize, usize, u64) {
    let mut total = (0, 0, 0, 0);
    while let Ok(batch) = rx.recv() {
        let (rows, bytes, sum) = batch.stats();
        total.0 += 1;
        total.1 += rows;
        total.2 += bytes;
        total.3 += sum;
    }
    total
}
