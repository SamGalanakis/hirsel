//! Optional test-supervisor progress, independent of captured program output.
//! The supervisor owns the directory and lifetime. Records contain operation
//! names and source identities, never launch arguments or capability inputs.
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{SystemTime, UNIX_EPOCH};

static DIRECTORY: OnceLock<Option<PathBuf>> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(0);

/// A coarse operation whose end is recorded even on early return or unwind.
pub struct Guard(Option<(PathBuf, u64)>);

fn record(path: &std::path::Path, id: u64, event: &str, phase: &str, detail: &str) {
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let line = serde_json::json!({
        "timestamp": now, "id": id,
        "thread": format!("{:?}", std::thread::current().id()),
        "event": event, "phase": phase, "detail": detail,
    })
    .to_string()
        + "\n";
    let _ = file.write_all(line.as_bytes());
}

/// Record only when a test supervisor explicitly supplies its private sink.
/// The detail closure is not evaluated in ordinary compiler invocations.
pub fn enter(phase: &'static str, detail: impl FnOnce() -> String) -> Guard {
    let Some(directory) =
        DIRECTORY.get_or_init(|| std::env::var_os("HIRSEL_TEST_PHASE_DIR").map(PathBuf::from))
    else {
        return Guard(None);
    };
    let path = directory.join(format!("{}.jsonl", std::process::id()));
    let id = NEXT.fetch_add(1, Relaxed);
    record(&path, id, "start", phase, &detail());
    Guard(Some((path, id)))
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some((path, id)) = &self.0 {
            record(path, *id, "end", "", "");
        }
    }
}
