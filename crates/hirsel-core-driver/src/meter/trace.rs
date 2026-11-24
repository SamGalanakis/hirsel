//! Wall-clock phase timestamps for a resident request. Set
//! `HIRSEL_TRACE_REQUEST_PHASES=1` in the server and client environment to
//! receive one `hirsel-request-phases` JSON line on the client's stderr.
use serde::Serialize;
use std::cell::RefCell;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

thread_local! {
    static ACTIVE: RefCell<Option<Trace>> = const { RefCell::new(None) };
}

#[derive(Serialize)]
struct Trace {
    pid: u32,
    request: u64,
    #[serde(skip)]
    start: Instant,
    events: Vec<Event>,
}

#[derive(Serialize)]
struct Event {
    phase: &'static str,
    boundary: Boundary,
    ns: u128,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum Boundary {
    Mark,
    Begin,
    End,
}

pub(crate) fn begin() {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let trace = Trace {
        pid: std::process::id(),
        request: NEXT.fetch_add(1, Ordering::Relaxed),
        start: Instant::now(),
        events: Vec::new(),
    };
    ACTIVE.with_borrow_mut(|active| *active = Some(trace));
    mark("request accepted");
}

pub(crate) fn finish() {
    mark("reply");
    if let Some(trace) = ACTIVE.with_borrow_mut(Option::take)
        && let Ok(line) = serde_json::to_string(&trace)
    {
        let _ = writeln!(io::stderr().lock(), "hirsel-request-phases {line}");
    }
}

fn record(phase: &'static str, boundary: Boundary) -> bool {
    ACTIVE.with_borrow_mut(|active| {
        if let Some(trace) = active {
            trace.events.push(Event {
                phase,
                boundary,
                ns: trace.start.elapsed().as_nanos(),
            });
            true
        } else {
            false
        }
    })
}

pub(crate) fn mark(phase: &'static str) {
    record(phase, Boundary::Mark);
}

pub(crate) struct Span(&'static str);

pub(crate) fn span(phase: &'static str) -> Option<Span> {
    record(phase, Boundary::Begin).then_some(Span(phase))
}

impl Drop for Span {
    fn drop(&mut self) {
        record(self.0, Boundary::End);
    }
}
