// Replay of clients that cross between units (docs/recording.md, "Replay of
// a run with helpers"): an arriving client replays as a stand-in with its
// connection's recorded layout. Included from recording_units_native.rs.

/// The root hands two clients of its connection, and a client of a
/// connection of clients, to a helper. The helper sends through the first
/// and returns it, drops the second, and sends the third back through the
/// fourth's connection; the root sends through the returned one, receives
/// the sent one, and drops both.
const U5_CLIENTS: &str = r#"
fn relay(take cs: (Client[u64], Client[u64], Client[u64], Client[Client[u64]])) -> Client[u64] waits {
    let (take kept, take dropped, take passed, take hello) = take cs
    result.unwrap(conn.send(kept, 7))
    drop(take dropped)
    result.unwrap(conn.send(hello, take passed))
    drop(take hello)
    kept
}

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let (c, inbox): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(2, bytes 0))
    let other = conn.client(c)
    let third = conn.client(c)
    let (hello, hello_in): (Client[Client[u64]], Inbox[Client[u64]]) = result.unwrap(conn.new(1, bytes 0))
    let x = (take c, take other, take third, take hello)
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, take x, relay))
    let back = result.unwrap(task.join(take h))
    result.unwrap(conn.send(back, 5))
    drop(take back)
    let passed = result.unwrap(conn.receive(hello_in))
    drop(take passed)
    let a = result.unwrap(conn.receive(inbox))
    let b = result.unwrap(conn.receive(inbox))
    if a * 10 + b != 75 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, "clients"))
    Ok(())
}
"#;

/// A client recorded in one unit and dropped in another replays `Sealed`:
/// each arriving client is a stand-in with the layout its snapshot carries
/// (recording.md, "Snapshots", tag 0), the helper's sends and drop through
/// it are checked against its stream, and the root's close comes from its
/// own `TAKE`. One corrupted frame gives exactly one line:
/// - the logical size in the helper's entry snapshot of the client it
///   returns: its `END`, whose result snapshot the stand-in rebuilds;
/// - the ordinal of the helper's drop `CLOSE`;
/// - the ordinal of the root's close `TAKE`.
///
/// A `--diagnose` replay by a build of another revision refuses the
/// arriving client (exit 2): the recorded layout need not be its own.
#[test]
#[ignore = "explicit native integration target"]
fn u5_clients_cross_and_drop_in_another_unit() {
    crate::openat2_required::require();

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let build = |source: &str, name: &str, backend: Backend| {
        let mut request = source_request(source, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        assert!(product.accepted(), "{:?}", product.diagnostics);
        let mut options = native_options(backend, dir.path());
        options.runtime = env_record_runtime();
        options.recording = true;
        options.grant_mask = ["Tasks", "Stdout"]
            .iter()
            .fold(0, |m, g| m | 1 << grant_kind(g).unwrap());
        options.cas = dir.path().join(format!("{name}-store-{backend:?}"));
        options.output = dir.path().join(format!("{name}-{backend:?}"));
        product.build(&entry, &options).unwrap().executable
    };
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let program = build(U5_CLIENTS, "u5c", backend);
        let path = dir.path().join(format!("u5c-{backend:?}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!((code, out.as_str()), (Some(0), "clients"), "{backend:?}: {err}");
        let frames = unit_streams(&path, &format!("{backend:?}"), &["root", "root.0"]);
        for unit in [None, Some("root"), Some("root.0")] {
            let (code, out, err) = u4_replay(&program, &path, unit);
            assert_eq!(
                (code, out.as_str(), err.as_str()),
                (Some(0), "", ""),
                "{backend:?} {unit:?}: {frames:#?}"
            );
        }
        let tape = std::fs::read(&path).unwrap();
        let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
            std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
        })
        .unwrap()
        .1
        .tape_id;
        // Patch byte `at` of the first `window` in `stream` that follows
        // `after`, to 99.
        let one_line = |name: &str, stream: u32, after: &[u8], window: &[u8], at: usize, expected: &str| {
            let wrong = u4_patched(&tape, &tape_id, stream, |payload| {
                let from = match after {
                    [] => 0,
                    _ => match payload.windows(after.len()).position(|w| w == after) {
                        Some(i) => i + after.len(),
                        None => return false,
                    },
                };
                let Some(i) = payload[from..].windows(window.len()).position(|w| w == window) else {
                    return false;
                };
                payload[from + i + at] = 99;
                true
            });
            let corrupt = dir.path().join(format!("u5c-{backend:?}-{name}.tape"));
            std::fs::write(&corrupt, wrong).unwrap();
            let (code, out, err) = u4_replay(&program, &corrupt, None);
            assert_eq!((code, out.as_str()), (Some(1), ""), "{backend:?} {name}: {err}");
            let lines: Vec<_> = err.lines().collect();
            assert!(
                matches!(&lines[..], [l] if l.starts_with(expected)),
                "{backend:?} {name}: {err} {frames:#?}"
            );
        };
        // By hand from recording.md. The root's grants are `Tasks` (1) and
        // `Stdout` (2); `conn.new` numbers connection 3 and client 4, and
        // `conn.client` client 5. The helper's entry numbers the clients 1
        // and 2: each a handle part (1), tag 0, its ordinal, home stream 0,
        // connection 3, `cap` 2, room 0, then logical size 8, whose low byte
        // moves to 99 in the first.
        let mut snapshot = vec![1, 0, 1, 0, 0, 0, 0, 3, 2, 0, 0, 0, 0, 0, 0, 0];
        snapshot.extend([0; 8]);
        snapshot.push(8);
        // Its frames: the send through client 1 (1), the drop's `CLOSE` of
        // client 2 (2), the send of client 3 through client 4 (3), the drop's
        // `CLOSE` of client 4 (4) and its `END` (5), whose result snapshot is
        // the stand-in's, now of logical size 99.
        one_line("layout", 1, &[], &snapshot, 24, "replay: Diverged(Outcome) at 5 in root.0 ");
        // The drop's `CLOSE` (op 3) of client 2, after the send (165)
        // through client 1 of 7. A `CLOSE` is matched by its object
        // wherever it falls, so the drop that finds none diverges at the
        // frame after it.
        let send = [165, 1, 10, 8, 7];
        one_line("close", 1, &send, &[3, 2, 0, 0, 0], 1, "replay: Diverged(Object) at 3 in root.0 ");
        // The root's close take: tag 1, connection 3.
        one_line("take", 0, &[], &[1, 3], 1, "replay: Diverged(Take) at 12 in root ");

        // A build of another revision: the same units with another exit
        // status on the failure path.
        let other = build(&U5_CLIENTS.replace("status: 3", "status: 4"), "u5c-other", backend);
        let argv = ["--hirsel-replay-recording", "diagnose", path.to_str().unwrap()];
        let (code, out, err) = tape_run(&other, &argv, None);
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (
                Some(2),
                "",
                "replay: refused: a client arriving in a replay of another revision\n"
            ),
            "{backend:?} across"
        );
    }
}
