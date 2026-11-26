//! The verdicts of a replay's units (docs/recording.md, "Replay of a run with
//! helpers"). Each stream ends with its own verdict, and a verdict other than
//! `Sealed` ends only that unit. When every started stream has ended, the
//! replay writes one line per stream whose verdict is not `Sealed`, ordered by
//! spawn-index path compared numerically, and exits 1 if it wrote any.
use crate::units::Reply;
use std::sync::{Condvar, Mutex};

struct Waiter(*const Reply);
// SAFETY: the root keeps its reply live until the last helper completes it.
unsafe impl Send for Waiter {}

struct Board {
    waiting: Option<Waiter>,
    /// Started helper streams whose verdict is still to come.
    running: usize,
    /// Each verdict line other than `Sealed`, with its unit's name.
    lines: Vec<(String, String)>,
}

static BOARD: Mutex<Board> = Mutex::new(Board {
    waiting: None,
    running: 0,
    lines: Vec::new(),
});
static ENDED: Condvar = Condvar::new();

/// A helper stream started replaying: the replay's end waits for its verdict.
pub(crate) fn started() {
    BOARD.lock().unwrap().running += 1;
}

/// A started helper stream's verdict: its line, or None for `Sealed`.
pub(crate) fn ended(unit: &str, line: Option<String>) {
    let mut board = BOARD.lock().unwrap();
    board.running = board.running.saturating_sub(1);
    board.lines.extend(line.map(|line| (unit.to_owned(), line)));
    if board.running == 0 {
        let waiter = board.waiting.take();
        drop(board);
        if let Some(Waiter(reply)) = waiter {
            // SAFETY: this is the sole completion of the root's pinned reply.
            unsafe { Reply::complete(reply) };
        }
        ENDED.notify_all();
    }
}

/// The verdict of a unit no stream replays: a missing stream's.
pub(crate) fn unstarted(unit: &str, line: String) {
    BOARD.lock().unwrap().lines.push((unit.to_owned(), line));
}

/// A helper's replay ended at its verdict. The pool can run another unit
/// while this one stays parked until the process exits.
pub(crate) fn park() -> ! {
    if crate::units::in_unit() {
        Reply::new().wait();
        unreachable!("a reply without a producer cannot complete");
    }
    loop {
        std::thread::park();
    }
}

/// Wait for each started stream, then print verdicts other than Sealed
/// in unit-name order. True when the replay must exit 1.
pub(crate) fn conclude(stderr: i32, root: Option<String>) -> bool {
    if crate::units::in_unit() {
        let reply = Reply::new();
        let needs_wait = {
            let mut board = BOARD.lock().unwrap();
            let running = board.running != 0;
            if running {
                assert!(board.waiting.replace(Waiter(&reply)).is_none());
            }
            running
        };
        if needs_wait {
            reply.wait();
        }
    } else {
        drop(
            ENDED
                .wait_while(BOARD.lock().unwrap(), |board| board.running != 0)
                .unwrap(),
        );
    }
    let mut board = BOARD.lock().unwrap();
    let mut lines = std::mem::take(&mut board.lines);
    lines.extend(root.map(|line| ("root".to_owned(), line)));
    lines.sort_by_cached_key(|(unit, _)| order(unit));
    for (_, line) in &lines {
        let line = format!("{line}\n");
        // SAFETY: `stderr` is the launcher's stderr fd.
        unsafe { crate::os::diagnostic(stderr, line.as_ptr(), line.len()) };
    }
    !lines.is_empty()
}

/// A unit name's place in verdict order: its spawn-index path, compared
/// numerically, with a stream whose parent is absent (`?.N`) last.
fn order(unit: &str) -> (bool, Vec<u64>) {
    let mut parts = unit.split('.');
    let orphan = parts.next() == Some("?");
    (
        orphan,
        parts.map(|p| p.parse().unwrap_or(u64::MAX)).collect(),
    )
}

#[cfg(test)]
mod tests {
    /// Verdict order compares spawn indices as numbers and puts orphans
    /// last (recording.md: `root.2` before `root.10`).
    #[test]
    fn unit_names_order_numerically() {
        let mut names = ["?.3", "root.10", "root.2.0", "root", "root.2"];
        names.sort_by_cached_key(|n| super::order(n));
        assert_eq!(names, ["root", "root.2", "root.2.0", "root.10", "?.3"]);
    }
}
