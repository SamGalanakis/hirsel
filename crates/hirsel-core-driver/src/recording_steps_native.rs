// MEM-30 step accounts under recording (docs/recording.md, "Step accounts"):
// a same-unit spawn shares its spawner's account, so the order of the
// participants' turns decides who spends its last steps, and a helper's
// unused grant goes back to its donor at the join. Included from
// recording_units_native.rs.

/// Two children and their parent share one account and each spins across
/// slices, so their turns interleave. `BUDGET` either fits all 340,000
/// iterations or runs out in whichever participant the turn order reaches.
const STEPS_RACE: &str = r#"
fn spin_to(n: u64) -> u64 {
    let mut i = 0u64
    while i < n { i += 1 }
    i
}
fn crunch(take n: u64) -> u64 { spin_to(n) }

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let done = bound steps BUDGET {
        let a = task.spawn(tasks, bytes 4096, 150000u64, crunch)
        let b = task.spawn(tasks, bytes 4096, 90000u64, crunch)
        let mine = spin_to(100000)
        match (task.join(take a), task.join(take b)) {
            (Ok(x), Ok(y)) => x + y + mine,
            (Error(_), _) => 1,
            _ => 2,
        }
    }
    let said = match done {
        Ok(v) => if v == 340000 { "fits" } else if v == 1 { "a short" } else if v == 2 { "b short" } else { "wrong" },
        Error(_) => "parent short",
    }
    result.unwrap(stdout.write(out, said))
    Ok(())
}
"#;

/// Two children share the parent's account and each first writes to a served
/// `Stdout`, so each parks on the port with no event of its own: the recorded
/// `TURN` of the second child falls where the first parked, at the first
/// child's provider entry. The parent answers the oldest call first, so a
/// replay that parked the children in the other order wakes the wrong one.
const STEPS_PARK: &str = r#"
fn say(take o: Stdout) -> u64 waits {
    result.unwrap(stdout.write(o, "w"))
    1
}

fn serve(mut s: Served[StreamOp]) waits {
    let Some(take call) = provide.next(mut s) else { fail("no call") }
    let _ = provide.reply(mut s, take call, provide.stream_reply(Done))
}

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let done = bound steps 2000000 {
        let (o, port) = provide.stdout(bytes 64, None)
        let mut s = take port
        let a = task.spawn(tasks, bytes 4096, o, say)
        let b = task.spawn(tasks, bytes 4096, o, say)
        serve(mut s)
        serve(mut s)
        match (task.join(take a), task.join(take b)) {
            (Ok(x), Ok(y)) => x + y,
            _ => 0,
        }
    }
    let said = match done {
        Ok(2) => "served",
        _ => "wrong",
    }
    result.unwrap(stdout.write(out, said))
    Ok(())
}
"#;

/// A helper takes half of its donor's 400,000 and spends about 1,000; the
/// owner's continuation of 300,000 iterations fits only with the helper's
/// residual back.
const STEPS_REFUND: &str = r#"
fn spin_to(n: u64) -> u64 {
    let mut i = 0u64
    while i < n { i += 1 }
    i
}
fn light(take n: u64) -> u64 { spin_to(n) }

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let done = bound steps 400000 {
        let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, 1000u64, light))
        let got = result.unwrap(task.join(take h))
        got + spin_to(300000)
    }
    let said = match done {
        Ok(v) => if v == 301000 { "refunded" } else { "wrong" },
        Error(_) => "burned",
    }
    result.unwrap(stdout.write(out, said))
    Ok(())
}
"#;

fn steps_build(dir: &Path, source: &str, name: &str, backend: Backend) -> std::path::PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut request = source_request(source, dir);
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let mut options = native_options(backend, dir);
    options.runtime = env_record_runtime();
    options.recording = true;
    options.grant_mask = ["Tasks", "Stdout"]
        .iter()
        .fold(0, |m, g| m | 1 << grant_kind(g).unwrap());
    options.cas = dir.join(format!("{name}-store-{backend:?}"));
    options.output = dir.join(format!("{name}-{backend:?}"));
    product.build(&entry, &options).unwrap().executable
}

/// A spawn-then-continue run whose aggregate work fits completes; one whose
/// budget runs short fails in the participant the turn order reaches. With
/// recording off and on the outcome is the same, and the recording replays
/// `Sealed`: the replay starts each participant's turn at its recorded
/// `TURN`, so the same participant spends the last step and is refused at
/// the same step. A `TURN` naming a task the unit does not have diverges.
#[test]
#[ignore = "explicit native integration target"]
fn steps_shared_turns_replay() {
    crate::openat2_required::require();
    use hirsel_tape_format::Frame;

    let dir = tempfile::tempdir().unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        for (budget, fits) in [("2000000", true), ("300000", false)] {
            let name = format!("race{budget}");
            let source = STEPS_RACE.replace("BUDGET", budget);
            let program = steps_build(dir.path(), &source, &name, backend);
            let (code, plain, err) = tape_run(&program, &[], None);
            assert_eq!(code, Some(0), "{backend:?} {budget}: {err}");
            assert_eq!(plain == "fits", fits, "{backend:?} {budget}: {plain}");
            assert_ne!(plain, "wrong", "{backend:?} {budget}");
            let path = dir.path().join(format!("{name}-{backend:?}.tape"));
            let (code, out, err) = tape_run(&program, &[], Some(&path));
            assert_eq!((code, out.as_str()), (Some(0), plain.as_str()), "{backend:?} {budget}: {err}");
            let streams = unit_streams(&path, &format!("{backend:?}"), &["root"]);
            let turns: Vec<_> = streams[0]
                .1
                .iter()
                .filter_map(|f| match f {
                    Frame::Turn { gap, task } => Some((*gap, *task)),
                    _ => None,
                })
                .collect();
            assert!(!turns.is_empty(), "{backend:?} {budget}: {:#?}", streams[0].1);
            let (code, out, err) = u4_replay(&program, &path, None);
            assert_eq!((code, out.as_str(), err.as_str()), (Some(0), "", ""), "{backend:?} {budget}");

            let tape = std::fs::read(&path).unwrap();
            let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
                std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
            })
            .unwrap()
            .1
            .tape_id;
            // A TURN with its task bit (0x10 | kind 15), its gap and task as
            // one-byte varints, renamed to task 99.
            let wrong = u4_patched(&tape, &tape_id, 0, |payload| {
                turns.iter().any(|&(gap, task)| {
                    if gap >= 128 || task >= 128 {
                        return false;
                    }
                    let frame = [0x1f, gap as u8, task as u8];
                    let Some(at) = payload.windows(3).position(|w| w == frame) else {
                        return false;
                    };
                    payload[at + 2] = 99;
                    true
                })
            });
            let corrupt = dir.path().join(format!("{name}-{backend:?}-turn.tape"));
            std::fs::write(&corrupt, wrong).unwrap();
            let (code, out, err) = u4_replay(&program, &corrupt, None);
            assert_eq!((code, out.as_str()), (Some(1), ""), "{backend:?} {budget}: {err}");
            assert!(
                err.starts_with("replay: Diverged(Schedule)"),
                "{backend:?} {budget}: {err}"
            );
        }
        // The second child's TURN falls at the first child's provider entry,
        // where the recording dispatched it only after the first child parked.
        let program = steps_build(dir.path(), STEPS_PARK, "park", backend);
        let path = dir.path().join(format!("park-{backend:?}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!((code, out.as_str()), (Some(0), "served"), "{backend:?} park: {err}");
        let (code, out, err) = u4_replay(&program, &path, None);
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (Some(0), "", ""),
            "{backend:?} park: {:#?}",
            unit_streams(&path, &format!("{backend:?}"), &["root"])[0].1
        );
    }
}

/// The owner's continuation needs the helper's unused grant: the run says
/// "refunded" with recording off and on, and replays `Sealed` whole and per
/// unit, the owner taking the residual from its own `TAKE`. A `TAKE` whose
/// residual exceeds the grant the owner debited diverges.
#[test]
#[ignore = "explicit native integration target"]
fn steps_helper_residual_replays() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Arrival, Frame};

    let dir = tempfile::tempdir().unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let program = steps_build(dir.path(), STEPS_REFUND, "refund", backend);
        let (code, out, err) = tape_run(&program, &[], None);
        assert_eq!((code, out.as_str()), (Some(0), "refunded"), "{backend:?}: {err}");
        let path = dir.path().join(format!("refund-{backend:?}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!((code, out.as_str()), (Some(0), "refunded"), "{backend:?}: {err}");
        let streams = unit_streams(&path, &format!("{backend:?}"), &["root", "root.0"]);
        // The helper's entry bound is its grant; its END and the owner's
        // TAKE carry the same residual, below the grant.
        let grant = streams[1].0.as_ref().unwrap().body().bound.unwrap();
        let ended = streams[1].1.iter().find_map(|f| match f {
            Frame::HelperEnd { ending, .. } => Some(ending.residual()),
            _ => None,
        });
        let taken = streams[0].1.iter().find_map(|f| match f {
            Frame::Take { body, .. } => match body.arrival() {
                Arrival::HelperEnd(_, _, residual, _) => Some(residual),
                _ => None,
            },
            _ => None,
        });
        let residual = taken.unwrap();
        assert_eq!(ended, Some(residual), "{backend:?}");
        assert!(residual > 0 && residual < grant, "{backend:?}: {residual} of {grant}");
        for unit in [None, Some("root"), Some("root.0")] {
            let (code, out, err) = u4_replay(&program, &path, unit);
            assert_eq!((code, out.as_str(), err.as_str()), (Some(0), "", ""), "{backend:?} {unit:?}");
        }

        // The owner's TAKE: tag 4, ordinal, Returned (0), the value's 8
        // plain bytes of 1000 and no parts, then the residual varint; it
        // becomes one more than the grant, as long a varint as the residual
        // when both take three bytes.
        let tape = std::fs::read(&path).unwrap();
        let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
            std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
        })
        .unwrap()
        .1
        .tape_id;
        let varint = |mut n: u64| {
            let mut out = Vec::new();
            while n >= 0x80 {
                out.push(n as u8 | 0x80);
                n >>= 7;
            }
            out.push(n as u8);
            out
        };
        let (from, to) = (varint(residual), varint(grant + 1));
        assert_eq!(from.len(), to.len(), "{backend:?}: {residual} {grant}");
        let mut head = vec![0, 8];
        head.extend(1000u64.to_le_bytes());
        head.push(0);
        head.extend(&from);
        let wrong = u4_patched(&tape, &tape_id, 0, |payload| {
            let Some(at) = payload.windows(head.len()).position(|w| w == head) else {
                return false;
            };
            let at = at + head.len() - from.len();
            payload[at..at + to.len()].copy_from_slice(&to);
            true
        });
        let corrupt = dir.path().join(format!("refund-{backend:?}-residual.tape"));
        std::fs::write(&corrupt, wrong).unwrap();
        let (code, out, err) = u4_replay(&program, &corrupt, Some("root"));
        assert_eq!((code, out.as_str()), (Some(1), ""), "{backend:?}: {err}");
        assert!(err.starts_with("replay: Diverged(Take)"), "{backend:?}: {err}");
    }
}
