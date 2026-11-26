use super::*;

/// A task that takes one tape frame of its own: a clock read.
#[derive(Clone, Copy)]
struct Other {
    clock: (Provider, u64),
    seen: *mut Option<Ob>,
}
unsafe extern "C" fn other(context: *mut crate::Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the spawner pins `seen` until it joins this task.
    unsafe {
        let job = input.cast::<Other>().read();
        // This task does not suspend between taking the context and giving
        // the spawner's back.
        let spawner = TASK.replace(context);
        *job.seen = Some(plain(job.clock, WALL_MS, 0, &[]));
        release_parked_sync();
        TASK.set(spawner);
    }
    0
}

/// The sync `Syncs::Parked` holds, or `SYNC_RELEASED`: later ones ready at once.
static PARKED_SYNC: AtomicU64 = AtomicU64::new(0);
const SYNC_RELEASED: u64 = u64::MAX;
fn release_parked_sync() {
    let held = PARKED_SYNC.swap(SYNC_RELEASED, SeqCst);
    if held != 0 && held != SYNC_RELEASED {
        crate::tasks::hirsel_core_ready(held);
    }
}

/// How the host answers SYNC and SYNC_DIR.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Syncs {
    /// Parked until the other task has run, then synced on the OS at resume.
    Parked,
    /// The OS, finished inside issue: the tape records OP.
    Blocking,
    /// The file's SYNC parks and fails with EIO; no syscall.
    Failing,
}
struct SyncHost(Provider, Syncs);
impl SyncHost {
    fn provider(&self) -> Provider {
        unsafe extern "C" fn wait(s: *mut u8, r: *mut Request, t: u64) -> Wait {
            let p = unsafe { (*s.cast::<SyncHost>()).0 };
            unsafe { (p.wait)(p.state, r, t) }
        }
        unsafe extern "C" fn issue(s: *mut u8, r: *mut Request, t: u64) -> Step {
            let SyncHost(p, syncs) = unsafe { &*s.cast::<SyncHost>() };
            let operation = unsafe { (*r).operation };
            let parked = *syncs == Syncs::Parked && matches!(operation, SYNC | SYNC_DIR);
            if parked || (*syncs == Syncs::Failing && operation == SYNC) {
                if !parked || PARKED_SYNC.compare_exchange(0, t, SeqCst, SeqCst).is_err() {
                    crate::tasks::hirsel_core_ready(t);
                }
                return Step {
                    kind: STEP_PENDING,
                    ..Step::default()
                };
            }
            unsafe { (p.issue)(p.state, r, t) }
        }
        unsafe extern "C" fn resume(s: *mut u8, r: *mut Request, t: u64) -> Step {
            let SyncHost(p, syncs) = unsafe { &*s.cast::<SyncHost>() };
            if *syncs == Syncs::Failing && unsafe { (*r).operation } == SYNC {
                return Step {
                    kind: STEP_DONE,
                    outcome: Outcome {
                        error: IoError::from_errno(libc::EIO),
                        ..Outcome::default()
                    },
                    deadline_ns: 0,
                };
            }
            if *syncs == Syncs::Parked && matches!(unsafe { (*r).operation }, SYNC | SYNC_DIR) {
                return unsafe { (p.issue)(p.state, r, t) };
            }
            unsafe { (p.resume)(p.state, r, t) }
        }
        unsafe extern "C" fn abort(_: *mut u8, _: *mut Request, _: u64, _: u8) -> Outcome {
            unreachable!("an issued sync is never aborted")
        }
        unsafe extern "C" fn deliver(s: *mut u8, r: *mut Request, o: Outcome) -> Outcome {
            let p = unsafe { (*s.cast::<SyncHost>()).0 };
            unsafe { (p.deliver)(p.state, r, o) }
        }
        Provider {
            state: (self as *const Self).cast_mut().cast(),
            wait,
            issue,
            resume,
            abort,
            deliver,
        }
    }
}

/// The bypass law on the real host, every S1 operation kind, each issued by
/// a task through the scheduler. Wrong implementation caught: one op kind
/// whose Replay reaches the host, or whose recorded answer (value, error,
/// written, carrier bytes) is lost; a parked SYNC whose ISSUE and DONE do not
/// replay around another task's frame; a tape recorded with SYNC as one OP
/// (the OS host, which syncs inside issue) that no longer replays.
#[test]
fn every_s1_kind_recorded_on_the_os_replays_with_no_host() {
    crate::openat2_required::require();
    super::run::check_launch_replay();
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let (world_dir, store_dir) = (root.join("world"), root.join("store"));
    std::fs::create_dir_all(&world_dir).unwrap();
    std::fs::create_dir_all(&store_dir).unwrap();
    let stdout = std::fs::File::create(root.join("stdout")).unwrap();
    let stderr = std::fs::File::create(root.join("stderr")).unwrap();
    let mut os = Box::new(OsProvider::new(stdout.into(), stderr.into()));
    let (wd, sd) = (os.grant(&world_dir).unwrap(), os.grant(&store_dir).unwrap());
    let mut h = header(REVISION);
    h.kind = KIND_RUN;
    let (store, real) = (os.provider(), os.provider());

    let ops = |context: *mut crate::Context, tape: &Tape, h: Provider, wd: u64| {
        let dir = tape.on_tape(crate::entry::DIR, (h, wd)).unwrap();
        let clock = tape.on_tape(crate::entry::CLOCK, (h, 0)).unwrap();
        let out = tape.on_tape(crate::entry::STDOUT, (h, 0)).unwrap();
        let err = tape.on_tape(crate::entry::STDERR, (h, 0)).unwrap();
        let sub = |o: &Ob| (dir.0, handle_like(dir.1, o.value));
        let mut obs = vec![plain(dir, MKDIR, 0, b"a")];
        let f = plain(dir, OPEN, 3, b"a/f");
        let file = sub(&f);
        obs.push(f);
        let big: Vec<u8> = (0..100_000u32).map(|i| (i * 13) as u8).collect();
        obs.push(input(file, WRITE, &big));
        // Another task, runnable while this one's SYNC is parked.
        let mut seen = None;
        let mut job = Other {
            clock,
            seen: &mut seen,
        };
        // SAFETY: the job and `seen` outlive the join below.
        let task = unsafe {
            crate::tasks::hirsel_core_spawn(
                context,
                0,
                1 << 16,
                (&mut job as *mut Other).cast(),
                other,
                crate::tasks::TaskLayout {
                    input: crate::tasks::ValueLayout {
                        bytes: size_of::<Other>(),
                        align: align_of::<Other>(),
                        logical: 0,
                        clean: None,
                    },
                    result: crate::tasks::ValueLayout {
                        bytes: 0,
                        align: 1,
                        logical: 0,
                        clean: None,
                    },
                },
                0,
            )
        }
        .task;
        assert!(!task.is_null(), "spawn failed: {:?}", last_fault());
        obs.push(plain(file, SYNC, 0, &[]));
        // SAFETY: the task is this one's child.
        unsafe { crate::tasks::hirsel_core_join(context, task, ptr::null_mut(), 0) };
        obs.push(plain(file, TRY_LOCK, 0, &[]));
        obs.push(plain(file, UNLOCK, 0, &[]));
        obs.push(plain(file, CLOSE, 0, &[]));
        let g = plain(dir, OPEN, 0, b"a/f");
        let read = sub(&g);
        obs.push(g);
        obs.push(spare(read, READ, &[], 150_000));
        obs.push(spare(read, READ, &[], 16));
        obs.push(plain(read, CLOSE, 0, &[]));
        obs.push(plain(dir, OPEN, 0, b"absent"));
        obs.push(plain(dir, RENAME, 0, b"a/f\0a/g"));
        let t = spare(dir, MKDTEMP, b"t.", 64);
        let temp = sub(&t);
        obs.push(t);
        obs.push(plain(temp, SYNC_DIR, 0, &[]));
        obs.push(plain(dir, REMOVE, 0, b"a/g"));
        obs.push(plain(dir, REMOVE_TREE, 0, b"a"));
        obs.push(plain(dir, SYNC_DIR, 0, &[]));
        obs.push(plain(clock, WALL_MS, 0, &[]));
        let mono = plain(clock, MONO_NS, 0, &[]);
        let deadline = mono.value + 1_000;
        obs.push(mono);
        obs.push(plain(clock, MONO_NS, 0, &[]));
        obs.push(input(clock, SLEEP_UNTIL, &deadline.to_le_bytes()));
        obs.push(input(out, STDOUT, b"out"));
        obs.push(input(err, STDERR, b"err"));
        obs.push(seen.expect("the other task ran"));
        obs
    };
    let taped = |mode: Mode, host: Provider, wd: u64| -> Vec<Ob> {
        let mut obs = Vec::new();
        in_task(mode == Mode::Record, &mut |context| {
            TASK.set(context);
            // SAFETY: the tape's store is the OS provider's store grant.
            let (tape, _) =
                unsafe { Tape::open(REVISION, (store, sd), b"t", mode, &h, &expect()) }.unwrap();
            obs = ops(context, &tape, host, wd);
            tape.end(&ending()).unwrap();
            TASK.set(ptr::null_mut());
        });
        obs
    };

    for syncs in [Syncs::Parked, Syncs::Blocking, Syncs::Failing] {
        let host = Counted::new(Some(real));
        let answers = SyncHost(host.provider(), syncs);
        // A parked SYNC is held until the other task has taken its frame, so
        // that frame falls between the SYNC's ISSUE and DONE.
        let _ = std::fs::remove_file(store_dir.join("t"));
        PARKED_SYNC.store(0, SeqCst);
        let recorded = taped(Mode::Record, answers.provider(), wd);
        let kinds: std::collections::BTreeSet<u8> = recorded.iter().map(|o| o.op).collect();
        assert_eq!(kinds, (OPEN..=SYNC_DIR).collect(), "every S1 op kind");
        assert_eq!(
            recorded[11].error.tag, 1,
            "the absent open recorded its error"
        );
        assert!(recorded.iter().all(|o| o.status == 0));
        let failed: Vec<usize> = (0..recorded.len())
            .filter(|&i| recorded[i].error != IoError::OK)
            .collect();
        if syncs == Syncs::Failing {
            assert_eq!(failed, [3, 11], "the SYNC recorded its EIO");
            assert_eq!(recorded[3].error, IoError::from_errno(libc::EIO));
        } else {
            assert_eq!(failed, [11]);
        }
        // The recording's blocks are written by the writer thread.
        super::writer::flush();
        let frames = tape_frames(&std::fs::read(store_dir.join("t")).unwrap());
        let syncs_at: Vec<usize> = (0..frames.len())
            .filter(|&i| {
                matches!(
                    frames[i],
                    Frame::Issue {
                        op: SYNC | SYNC_DIR,
                        ..
                    } | Frame::Op {
                        op: SYNC | SYNC_DIR,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(syncs_at.len(), 3);
        if syncs == Syncs::Blocking {
            assert!(
                syncs_at
                    .iter()
                    .all(|&i| matches!(frames[i], Frame::Op { .. }))
            );
        } else {
            let Frame::Issue { task, .. } = frames[syncs_at[0]] else {
                panic!("the parked SYNC records an ISSUE");
            };
            let done = (syncs_at[0]..frames.len())
                .find(|&i| matches!(frames[i], Frame::Done { task: t, .. } if t == task))
                .expect("the SYNC's DONE");
            assert!(
                syncs == Syncs::Failing
                    || frames[syncs_at[0] + 1..done]
                        .iter()
                        .any(|f| matches!(f, Frame::Op { task: t, .. } if *t != task)),
                "another task's frame falls inside the parked SYNC"
            );
        }

        let refuse = Counted::new(None);
        let replayed = taped(Mode::Replay, refuse.provider(), 0);
        assert_eq!(
            refuse.calls.get(),
            0,
            "an operation reached the host under Replay"
        );
        assert_eq!(replayed, recorded);
    }

    // With no task (no scheduler) nothing but the tape can ready a replayed
    // parked SYNC: Replay must still finish, from the tape alone.
    let bare = |mode: Mode, host: Provider, wd: u64| -> Vec<Ob> {
        let mut context = crate::Context::new(1 << 20);
        let mut files = crate::host_file::Files::default();
        context.host_files = &mut files;
        crate::causes::task_start(&mut context).unwrap();
        TASK.set(&mut context);
        // SAFETY: the tape's store is the OS provider's store grant.
        let (tape, _) =
            unsafe { Tape::open(REVISION, (store, sd), b"t", mode, &h, &expect()) }.unwrap();
        let dir = tape.on_tape(crate::entry::DIR, (host, wd)).unwrap();
        let f = plain(dir, OPEN, 3, b"bare");
        let file = (dir.0, handle_like(dir.1, f.value));
        let obs = vec![
            f,
            input(file, WRITE, b"x"),
            plain(file, SYNC, 0, &[]),
            plain(file, CLOSE, 0, &[]),
            plain(dir, SYNC_DIR, 0, &[]),
            plain(dir, REMOVE, 0, b"bare"),
        ];
        tape.end(&ending()).unwrap();
        TASK.set(ptr::null_mut());
        obs
    };
    let _ = std::fs::remove_file(store_dir.join("t"));
    PARKED_SYNC.store(SYNC_RELEASED, SeqCst); // no other task: ready in issue
    let parked = SyncHost(real, Syncs::Parked);
    let recorded = bare(Mode::Record, parked.provider(), wd);
    assert!(
        recorded
            .iter()
            .all(|o| o.status == 0 && o.error == IoError::OK)
    );
    // The recording's blocks are written by the writer thread.
    super::writer::flush();
    let frames = tape_frames(&std::fs::read(store_dir.join("t")).unwrap());
    let issued = frames
        .iter()
        .filter(|f| {
            matches!(
                f,
                Frame::Issue {
                    op: SYNC | SYNC_DIR,
                    ..
                }
            )
        })
        .count();
    assert_eq!(issued, 2, "both syncs parked");
    let refuse = Counted::new(None);
    assert_eq!(bare(Mode::Replay, refuse.provider(), 0), recorded);
    assert_eq!(refuse.calls.get(), 0);

    drop(os);
}
