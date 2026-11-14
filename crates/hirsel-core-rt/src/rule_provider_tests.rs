//! Provider-rule oracles: confinement of a derived Dir and effect-free delivery replay.
use super::*;

#[test]
fn cap7_temporary_dir_never_widens_its_parent_authority() {
    crate::openat2_required::require();
    let mut f = Fixture::new();
    std::fs::write(f.root.join("parent-only"), b"secret").unwrap();
    let mut name = [0; 40];
    name[..4].copy_from_slice(b"sub-");
    let (derived, len) = f.call(MKDTEMP, f.dir, b"", 0, &mut name, 4);
    assert_eq!(derived.error, IoError::OK);
    let child = derived.value;
    assert_ne!(child, f.dir);
    assert_eq!(f.simple(OPEN, child, b"parent-only", 0).error.tag, 1);
    assert_eq!(f.simple(OPEN, child, b"../parent-only", 0).error.tag, 2);
    use std::os::unix::fs::symlink;
    let basename = std::str::from_utf8(&name[..len]).unwrap();
    symlink("../parent-only", f.root.join(basename).join("escape")).unwrap();
    assert_eq!(f.simple(OPEN, child, b"escape", 0).error.tag, 2);
    let file = f.simple(OPEN, child, b"inside", 3);
    assert_eq!(file.error, IoError::OK);
    assert_eq!(f.simple(CLOSE, file.value, b"", 0).error, IoError::OK);
    assert!(f.root.join(basename).join("inside").is_file());
    assert!(!f.root.join("inside").exists());
    assert_eq!(
        f.simple(REMOVE_TREE, f.dir, &name[..len], 0).error,
        IoError::OK
    );
    assert_eq!(
        std::fs::read(f.root.join("parent-only")).unwrap(),
        b"secret"
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Recorded {
    value: u64,
    error: IoError,
    written: u64,
    status: u8,
}
impl From<Outcome> for Recorded {
    fn from(o: Outcome) -> Self {
        Self {
            value: o.value,
            error: o.error,
            written: o.written,
            status: o.status,
        }
    }
}
struct Tape {
    context: *mut crate::Context,
    replay: bool,
    at: usize,
    values: Vec<Recorded>,
    points: Vec<(u8, u8, u64)>,
    effects: Vec<u8>,
}
unsafe extern "C" fn issue(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: the run pins the tape and request; no nested operation borrows them.
    unsafe {
        let tape = &mut *state.cast::<Tape>();
        let request = &mut *request;
        tape.points.push((0, request.operation, request.site));
        if tape.replay {
            let recorded = tape.values[tape.at];
            Outcome {
                value: recorded.value,
                error: recorded.error,
                written: recorded.written,
                status: recorded.status,
            }
        } else if request.operation == WRITE {
            tape.effects
                .extend_from_slice(std::slice::from_raw_parts(request.buffer.data, 2));
            Outcome {
                written: 2,
                error: IoError::from_errno(libc::ENOSPC),
                ..Outcome::default()
            }
        } else {
            Outcome {
                value: 887,
                ..Outcome::default()
            }
        }
    }
}
unsafe extern "C" fn delivery(state: *mut u8, request: *mut Request, result: Outcome) -> Outcome {
    // SAFETY: issue's exclusive tape/request borrow has ended before delivery.
    unsafe {
        let tape = &mut *state.cast::<Tape>();
        tape.points.push((1, (*request).operation, (*request).site));
        if !tape.replay {
            tape.values.push(result.into());
        }
        tape.at += 1;
        if (*request).operation == WRITE {
            (*tape.context)
                .cancel_requested
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }
    result
}
fn recorded_run(replay: Option<Vec<Recorded>>) -> (Tape, Vec<Recorded>, Vec<(u8, u64)>) {
    let mut context = crate::Context::new(4096);
    causes::task_start(&mut context).unwrap();
    let mut tape = Tape {
        context: &mut context,
        replay: replay.is_some(),
        at: 0,
        values: replay.unwrap_or_default(),
        points: vec![],
        effects: vec![],
    };
    let provider = Provider {
        state: (&mut tape as *mut Tape).cast(),
        issue,
        deliver: delivery,
    };
    let mut data = *b"abcdef";
    let mut request = Request {
        operation: WRITE,
        mode: 0,
        handle: 7,
        path: b"".as_ptr(),
        path_len: 0,
        buffer: Buffer {
            data: data.as_mut_ptr(),
            len: 6,
            capacity: 6,
        },
        site: 10,
    };
    let mut results = vec![];
    // SAFETY: pinned context/provider/request; each call ends before their assertions.
    unsafe {
        results.push(hirsel_core_host(&mut context, &provider, &mut request).into());
        request.operation = READ;
        request.site = 11;
        results.push(hirsel_core_host(&mut context, &provider, &mut request).into());
        request.operation = WALL_MS;
        request.site = 12;
        results.push(hirsel_core_host(&mut context, &provider, &mut request).into());
    }
    let chain = context
        .causes
        .iter()
        .map(|cause| (cause.kind(), cause.site))
        .collect();
    (tape, results, chain)
}
#[test]
fn cap8_conc7_replay_preserves_issue_delivery_and_cancellation_without_repeating_effects() {
    let (recorded, results, chain) = recorded_run(None);
    assert_eq!(recorded.effects, b"ab");
    assert_eq!(
        recorded.points,
        [
            (0, WRITE, 10),
            (1, WRITE, 10),
            (0, WALL_MS, 12),
            (1, WALL_MS, 12)
        ]
    );
    assert_eq!(recorded.at, 2);
    assert_eq!(
        results[0],
        Recorded {
            value: 0,
            error: IoError::from_errno(libc::ENOSPC),
            written: 2,
            status: 0
        }
    );
    assert_eq!(results[1].status, 1);
    assert_eq!((results[2].value, results[2].status), (887, 0));
    assert_eq!(chain, [(causes::CANCELLED, 11)]);
    let (replayed, replay_results, replay_chain) = recorded_run(Some(recorded.values));
    assert!(replayed.effects.is_empty());
    assert_eq!(replayed.at, 2);
    assert_eq!(replayed.points, recorded.points);
    assert_eq!(replay_results, results);
    assert_eq!(replay_chain, chain);
}

// CONC-7 shares the trace oracle above: the issued write delivers despite the
// cancellation requested at delivery; the next waiting READ observes it at entry,
// before issue, while a clock call never observes it. No extra test wrapper.
