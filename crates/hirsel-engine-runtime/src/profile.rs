//! Optional request-scoped JSONL capture. The scheduler writes only to a fixed
//! memory buffer; the CLI takes it after compilation and performs all file IO.
//! serde_json supplies the codec. A failed bounded write rolls back its line.
use super::{QueryId, RuntimeEvent, WaitReason, WakeReason};
use serde::Serialize;
use std::io::{self, Write};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
};

pub const CAPACITY: usize = 16 * 1024 * 1024;
static ENABLED: AtomicBool = AtomicBool::new(false);
static CAPTURE: Mutex<Option<Capture>> = Mutex::new(None);
static NEXT_RUNTIME: AtomicU64 = AtomicU64::new(0);

pub(super) fn runtime_id() -> u64 {
    NEXT_RUNTIME.fetch_add(1, Relaxed)
}

pub struct Capture {
    bytes: Vec<u8>,
    truncated: bool,
}

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > CAPACITY - self.bytes.len() {
            return Err(io::Error::other("query profile buffer full"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Capture {
    fn new() -> Self {
        Self {
            bytes: Vec::with_capacity(CAPACITY),
            truncated: false,
        }
    }
    fn push(&mut self, row: &impl Serialize) {
        if self.truncated {
            return;
        }
        let start = self.bytes.len();
        if serde_json::to_writer(&mut *self, row).is_err() || self.write_all(b"\n").is_err() {
            self.bytes.truncate(start);
            self.truncated = true;
        }
    }
    /// Called outside scheduler locks, after the command's work has drained.
    pub fn write_to(&self, mut output: impl Write) -> io::Result<()> {
        output.write_all(b"{\"schema\":1,\"event\":\"profile\",\"capacity_bytes\":16777216}\n")?;
        output.write_all(&self.bytes)?;
        serde_json::to_writer(
            &mut output,
            &serde_json::json!({
                "event": "end", "truncated": self.truncated, "buffer_bytes": self.bytes.len(),
            }),
        )?;
        output.write_all(b"\n")?;
        output.flush()
    }
}

pub fn start() -> Result<(), &'static str> {
    let mut capture = CAPTURE.lock().unwrap_or_else(|e| e.into_inner());
    if capture.is_some() {
        return Err("query profile already active");
    }
    *capture = Some(Capture::new());
    ENABLED.store(true, Relaxed);
    Ok(())
}
pub fn finish() -> Option<Capture> {
    ENABLED.store(false, Relaxed);
    CAPTURE.lock().unwrap_or_else(|e| e.into_inner()).take()
}
fn push(row: &impl Serialize) {
    if !ENABLED.load(Relaxed) {
        return;
    }
    if let Some(capture) = CAPTURE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        capture.push(row);
    }
}

#[derive(Serialize)]
struct Query {
    kind: u32,
    address: [u8; 32],
}
impl From<QueryId> for Query {
    fn from(id: QueryId) -> Self {
        Self {
            kind: id.kind,
            address: id.address.0.0,
        }
    }
}
/// Bind a logical query to its definition at the producer's existing identity
/// boundary. This diagnostic label never participates in query identity.
pub fn definition(id: QueryId, definition: &hirsel_syntax::DefPath) {
    if !ENABLED.load(Relaxed) {
        return;
    }
    #[derive(Serialize)]
    struct Label<'a> {
        event: &'static str,
        query: Query,
        definition: &'a str,
    }
    push(&Label {
        event: "definition",
        query: id.into(),
        definition: &definition.to_string(),
    });
}

struct Roots<'a>(&'a [QueryId]);
impl Serialize for Roots<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for query in self.0 {
            sequence.serialize_element(&Query::from(*query))?;
        }
        sequence.end()
    }
}

#[derive(Default, Serialize)]
struct Event<'a> {
    event: &'static str,
    runtime: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<Query>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dependency: Option<Query>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_ns: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cpu_ns: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    charged: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    roots: Option<Roots<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wait_reason: Option<WaitReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wake_reason: Option<WakeReason>,
}
pub(super) fn record(runtime: u64, event: &RuntimeEvent) {
    if !ENABLED.load(Relaxed) {
        return;
    }
    let mut row = Event {
        runtime,
        ..Event::default()
    };
    match event {
        RuntimeEvent::Claim(q) => {
            row.event = "claim";
            row.query = Some((*q).into());
        }
        RuntimeEvent::Read { query, dependency } => {
            row.event = "read";
            row.query = Some((*query).into());
            row.dependency = Some((*dependency).into());
        }
        RuntimeEvent::Admit {
            query,
            worker,
            elapsed_ns,
            charged,
        } => {
            row.event = "admit";
            row.query = Some((*query).into());
            row.worker = Some(*worker);
            row.elapsed_ns = Some(*elapsed_ns);
            row.charged = Some(*charged);
        }
        RuntimeEvent::Release {
            query,
            worker,
            elapsed_ns,
            cpu_ns,
            charged,
        } => {
            row.event = "release";
            row.query = Some((*query).into());
            row.worker = Some(*worker);
            row.elapsed_ns = Some(*elapsed_ns);
            row.cpu_ns = Some(*cpu_ns);
            row.charged = Some(*charged);
        }
        RuntimeEvent::Waiting {
            query,
            elapsed_ns,
            reason,
        } => {
            row.event = "waiting";
            row.query = Some((*query).into());
            row.elapsed_ns = Some(*elapsed_ns);
            row.wait_reason = Some(*reason);
        }
        RuntimeEvent::Wake {
            query,
            elapsed_ns,
            reason,
        } => {
            row.event = "wake";
            row.query = Some((*query).into());
            row.elapsed_ns = Some(*elapsed_ns);
            row.wake_reason = Some(*reason);
        }
        RuntimeEvent::Settled { query, result } => {
            row.event = "settled";
            row.query = Some((*query).into());
            row.success = Some(result.is_ok());
        }
        RuntimeEvent::Request { request, roots } => {
            row.event = "request";
            row.request = Some(request.0);
            row.roots = Some(Roots(roots));
        }
        RuntimeEvent::Completed(request) => {
            row.event = "completed";
            row.request = Some(request.0);
        }
        RuntimeEvent::Cancelled(request) => {
            row.event = "cancelled";
            row.request = Some(request.0);
        }
    }
    push(&row);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_buffer_marks_truncation_without_exporting_a_partial_record() {
        let mut capture = Capture::new();
        let row = serde_json::json!({"event": "test", "text": "x".repeat(1024)});
        while !capture.truncated {
            capture.push(&row);
        }
        assert!(capture.bytes.len() <= CAPACITY);
        assert_eq!(capture.bytes.capacity(), CAPACITY);
        let length = capture.bytes.len();
        capture.push(&row);
        assert_eq!(capture.bytes.len(), length);
        let mut output = Vec::new();
        capture.write_to(&mut output).unwrap();
        let rows = std::str::from_utf8(&output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(rows.len() > 2);
        assert_eq!(rows.last().unwrap()["truncated"], true);
        assert!(rows[1..rows.len() - 1].iter().all(|actual| actual == &row));
    }
}
