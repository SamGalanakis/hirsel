// Matched to pipeline/matched.hirsel: count then stream batches, exact buffers and three workers.
// Standard Rust channels/threads model the task provider's cancellation outcomes and custody.
// Independent review pending.
mod matched_sort;
#[path = "../matched_support.rs"]
mod support;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use support::*;
struct Batch {
    id: u64,
    data: String,
    offsets: Vec<u32>,
    released: SyncSender<u64>,
}
impl Drop for Batch {
    fn drop(&mut self) {
        let _ = self.released.try_send(self.id);
    }
}
fn batch(id: u64, rows: Vec<String>, released: &SyncSender<u64>) -> Batch {
    let mut size = 0usize;
    for row in &rows {
        size += row.len();
    }
    let mut data = String::with_capacity(size);
    let mut offsets = Vec::with_capacity(rows.len() + 1);
    offsets.push(0);
    for row in &rows {
        data.push_str(row);
        offsets.push(narrow(data.len()));
    }
    Batch {
        id,
        data,
        offsets,
        released: released.clone(),
    }
}
fn upper(b: &mut Batch) {
    let mut output = b.data.clone();
    output.make_ascii_uppercase();
    b.data = output;
}
fn wait_cancel_gate(tx: &SyncSender<()>) {
    loop {
        match tx.try_send(()) {
            Ok(()) => break,
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => panic!("cancel gate closed"),
        }
    }
}
struct Source {
    input: String,
    variant: String,
    output: SyncSender<Batch>,
    released: SyncSender<u64>,
    ready: SyncSender<()>,
    cancel_gate: SyncSender<()>,
}
fn source(s: Source) -> Result<(u64, bool)> {
    let mut at = 0;
    let batch_size = index(number(line(&s.input, &mut at))?);
    let failed = s.variant != "normal";
    let limit = if s.variant == "cancel-receive" {
        1
    } else if failed {
        2
    } else {
        u64::MAX
    };
    let (mut sent, mut full) = (0u64, false);
    while at < s.input.len() && sent < limit {
        let mut rows = Vec::with_capacity(batch_size);
        while at < s.input.len() && rows.len() < batch_size {
            rows.push(line(&s.input, &mut at).to_owned());
        }
        let b = batch(sent + 1, rows, &s.released);
        if s.variant == "normal" && sent == 8 {
            let pending = match s.output.try_send(b) {
                Err(TrySendError::Full(b)) => b,
                _ => panic!("q1 fullness witness"),
            };
            full = true;
            let _ = s.ready.try_send(());
            if s.output.send(pending).is_err() {
                break;
            }
        } else if s.output.send(b).is_err() {
            break;
        }
        sent += 1;
    }
    if s.variant == "cancel-send" {
        for id in 3..=8 {
            let mut rows = Vec::new();
            rows.push(String::literal("cancel"));
            let _ = s.output.send(batch(id, rows, &s.released));
        }
        let mut rows = Vec::new();
        rows.push(String::literal("cancel"));
        let ninth = batch(9, rows, &s.released);
        let pending = match s.output.try_send(ninth) {
            Err(TrySendError::Full(b)) => b,
            _ => panic!("q1 fullness witness"),
        };
        let _ = s.ready.try_send(());
        wait_cancel_gate(&s.cancel_gate);
        // The provider's pending cancellation rejects the next send and returns its payload.
        drop(pending);
        return Ok((8, true));
    }
    if !full {
        let _ = s.ready.try_send(());
    }
    Ok((sent, full))
}
struct Transform {
    variant: String,
    input: Receiver<Batch>,
    output: SyncSender<Batch>,
    start: Receiver<()>,
    ready: SyncSender<()>,
    forwarded: SyncSender<()>,
    cancel_gate: SyncSender<()>,
}
struct TransformResult {
    completed: u64,
    error: Option<Error>,
    retained: Option<Receiver<Batch>>,
}
fn transform(t: Transform) -> TransformResult {
    let _ = t.start.recv();
    if t.variant == "cancel-send" {
        let mut count = 0u64;
        while count < 8 {
            let _ = t.input.recv();
            count += 1;
        }
        return TransformResult {
            completed: 0,
            error: Some("Cancelled@source.send".into()),
            retained: Some(t.input),
        };
    }
    let mut completed = 0u64;
    while let Ok(mut current) = t.input.recv() {
        if current.id == 2 && t.variant != "normal" {
            if t.variant == "cancel" {
                let _ = t.ready.try_send(());
                wait_cancel_gate(&t.cancel_gate);
                return TransformResult {
                    completed,
                    error: Some("Cancelled@transform.check".into()),
                    retained: None,
                };
            }
            let size = (((current.data.len() as u64) + 7) / 8) * 8;
            let error = match admission(&t.variant, "batch2.output", size) {
                Err(e) => e,
                Ok(()) => Error::from("Transform@batch2"),
            };
            return TransformResult {
                completed,
                error: Some(error),
                retained: None,
            };
        }
        upper(&mut current);
        if t.output.send(current).is_err() {
            break;
        }
        completed += 1;
        let _ = t.forwarded.try_send(());
        if t.variant == "cancel-receive" {
            let _ = t.ready.try_send(());
            wait_cancel_gate(&t.cancel_gate);
            return TransformResult {
                completed,
                error: Some("Cancelled@transform.receive".into()),
                retained: None,
            };
        }
    }
    TransformResult {
        completed,
        error: None,
        retained: None,
    }
}
#[derive(Default)]
struct Stats {
    batches: u64,
    rows: u64,
    size: u64,
    checksum: u64,
}
fn sink(input: Receiver<Batch>, start: Receiver<()>) -> Stats {
    let _ = start.recv();
    let mut stats = Stats::default();
    while let Ok(b) = input.recv() {
        stats.batches += 1;
        stats.rows += (b.offsets.len() - 1) as u64;
        stats.size += b.data.len() as u64;
        stats.checksum += sum(b.data.as_bytes());
    }
    stats
}
fn ids(out: &mut String, xs: &[u64]) {
    let mut first = true;
    for id in xs {
        if !first {
            out.push(',');
        }
        first = false;
        num(out, *id);
    }
    out.push('\n');
}
fn run(input: String, variant: &str) -> Result<()> {
    let failed = variant != "normal";
    let mut at = 0;
    let batch_size = index(number(line(&input, &mut at))?);
    let mut rows = 0usize;
    while at < input.len() {
        line(&input, &mut at);
        rows += 1;
    }
    let batches = if variant == "cancel-send" {
        9
    } else {
        (rows + batch_size - 1) / batch_size
    };
    let (released, releases) = mpsc::sync_channel(batches);
    let (q1, r1) = mpsc::sync_channel(8);
    let (q2, r2) = mpsc::sync_channel(8);
    let (start, started) = mpsc::sync_channel(1);
    let (sink_start, sink_started) = mpsc::sync_channel(1);
    let (ready, ready_rx) = mpsc::sync_channel(1);
    let (transform_ready, transform_ready_rx) = mpsc::sync_channel(1);
    let (source_cancel, source_cancel_rx) = mpsc::sync_channel(1);
    let (transform_cancel, transform_cancel_rx) = mpsc::sync_channel(1);
    let _ = source_cancel.try_send(());
    let _ = transform_cancel.try_send(());
    let s = Source {
        input,
        variant: String::copy(variant),
        output: q1,
        released,
        ready,
        cancel_gate: source_cancel,
    };
    let mut src = Some(
        thread::Builder::new()
            .stack_size(4 * 1024 * 1024)
            .spawn(move || source(s))
            .unwrap(),
    );
    let (forwarded, progress) = mpsc::sync_channel(batches);
    let t = Transform {
        variant: String::copy(variant),
        input: r1,
        output: q2,
        start: started,
        ready: transform_ready,
        forwarded,
        cancel_gate: transform_cancel,
    };
    let trans = thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .spawn(move || transform(t))
        .unwrap();
    let snk = thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .spawn(move || sink(r2, sink_started))
        .unwrap();
    let _ = ready_rx.recv();
    let (mut sent, mut full) = (0u64, false);
    if failed && variant != "cancel-send" {
        (sent, full) = src.take().unwrap().join().unwrap()?;
    }
    let _ = start.try_send(());
    if !failed {
        let _ = sink_start.try_send(());
    }
    if variant == "cancel" || variant == "cancel-receive" {
        let _ = transform_ready_rx.recv();
        let _ = transform_cancel_rx.recv();
    }
    let transformed = trans.join().unwrap();
    let mut completed = 0u64;
    while progress.recv().is_ok() {
        completed += 1;
    }
    if variant == "cancel-send" {
        let _ = source_cancel_rx.recv();
        let _ = src.take().unwrap().join().unwrap();
        sent = 8;
        full = true;
    }
    drop(transformed.retained);
    if failed {
        let _ = sink_start.try_send(());
    }
    if let Some(h) = src.take() {
        (sent, full) = h.join().unwrap()?;
    }
    let stats = snk.join().unwrap();
    let mut released_ids = Vec::new();
    while let Ok(id) = releases.recv() {
        released_ids.push(id);
    }
    let mut rendered = String::new();
    if failed {
        rendered.push_str("release-order=");
        ids(&mut rendered, &released_ids);
    }
    rendered.push_str("sent=");
    num(&mut rendered, sent);
    rendered.push_str(" transformed=");
    num(&mut rendered, completed);
    rendered.push_str(" sink-batches=");
    num(&mut rendered, stats.batches);
    rendered.push_str(" rows=");
    num(&mut rendered, stats.rows);
    rendered.push_str(" bytes=");
    num(&mut rendered, stats.size);
    rendered.push_str(" sum=");
    num(&mut rendered, stats.checksum);
    rendered.push_str(if full {
        "\nbackpressure=true\nreleased="
    } else {
        "\nbackpressure=false\nreleased="
    });
    matched_sort::library_sort(
        &mut released_ids,
        matched_sort::Compare {
            call: |_, a, b| a.cmp(&b),
            environment: &(),
        },
    );
    ids(&mut rendered, &released_ids);
    put(&rendered)?;
    transformed.error.map_or(Ok(()), Err)
}
fn main() {
    let (input, variant) = match input() {
        Ok(x) => x,
        Err(_) => std::process::exit(2),
    };
    let result = run(input, &variant);
    Trace::new(1).finish(result);
}
