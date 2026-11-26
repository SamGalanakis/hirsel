// Replay of items whose content lies past their flat bytes (docs/recording.md,
// "Replay of a run with helpers"). Included from recording_units_native.rs.

/// The root sends variant items whose cases hold a `String`, a `List[u64]`
/// or nothing to its own connection, and receives them in order.
const U5_SEQ_ITEMS: &str = r#"
type Msg: cross = Text(u64, String) | Nums(List[u64]) | Count(u64)

fn score(take m: Msg) -> u64 {
    match take m {
        Text(n, take s) => 1000,
        Nums(take l) => list.len(l) * 100,
        Count(k) => k
    }
}

fn text(take m: Msg) -> String {
    match take m {
        Text(n, take s) => s,
        Nums(take l) => string.from("nums"),
        Count(k) => string.from("count")
    }
}

pub fn main(out: Stdout) -> Result[(), Exit] waits {
    let (c, inbox): (Client[Msg], Inbox[Msg]) = result.unwrap(conn.new(3, bytes 256))
    let mut l: List[u64] = list.new()
    list.push(mut l, 1)
    list.push(mut l, 2)
    result.unwrap(conn.send(c, Count(7)))
    result.unwrap(conn.send(c, Text(5, string.from("text"))))
    result.unwrap(conn.send(c, Nums(take l)))
    let a = score(result.unwrap(conn.receive(inbox)))
    let s = text(result.unwrap(conn.receive(inbox)))
    let d = score(result.unwrap(conn.receive(inbox)))
    if a + d * 1000 != 200007 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, s))
    Ok(())
}
"#;

/// An item's `String` and list replay from its `TAKE`, whichever case holds
/// them: the replayed item's sequence words point at the elements the take
/// carries, and only the active case's sequences are copied and charged, as
/// a send's are. Before, each sequence word of a replayed item was null and
/// every case's rows were copied, so the replay aborted on its first item.
/// The oracle is the program's literals: the `Count` and list cases' scores
/// must sum to 200007, and the `String` the root writes is the one it sent,
/// so a take whose text changes ends the replay at that write's `OP`.
#[test]
#[ignore = "explicit native integration target"]
fn u5_items_with_sequences_replay_by_case() {
    crate::openat2_required::require();

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let mut request = source_request(U5_SEQ_ITEMS, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let mut options = native_options(backend, dir.path());
        options.runtime = env_record_runtime();
        options.recording = true;
        options.grant_mask = 1 << grant_kind("Stdout").unwrap();
        options.cas = dir.path().join(format!("u5q-store-{backend:?}"));
        options.output = dir.path().join(format!("u5q-{backend:?}"));
        let program = product.build(&entry, &options).unwrap().executable;
        let path = dir.path().join(format!("u5q-{backend:?}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!((code, out.as_str()), (Some(0), "text"), "{backend:?}: {err}");
        let frames = unit_streams(&path, &format!("{backend:?}"), &["root"]);
        let (code, out, err) = u4_replay(&program, &path, None);
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (Some(0), "", ""),
            "{backend:?}: {:#?}",
            frames[0].1
        );
        // The second item's take, by hand from recording.md: tag 0, the
        // connection's ordinal 2 (the grant of `Stdout` is 1), 32 plain bytes
        // with 5 first, then one part, the `String`'s list (0) of 4 bytes,
        // "text", which becomes "tcxt". The root's frames: its `GRANT` (1),
        // the three sends (2-4), the item takes (5-7) and the write (8).
        let tape = std::fs::read(&path).unwrap();
        let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
            std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
        })
        .unwrap()
        .1
        .tape_id;
        let wrong = u4_patched(&tape, &tape_id, 0, |payload| {
            let Some(at) = payload.windows(6).position(|w| w == [0, 4, b't', b'e', b'x', b't']) else {
                return false;
            };
            payload[at + 3] = b'c';
            true
        });
        let corrupt = dir.path().join(format!("u5q-{backend:?}-text.tape"));
        std::fs::write(&corrupt, wrong).unwrap();
        let (code, out, err) = u4_replay(&program, &corrupt, None);
        assert_eq!((code, out.as_str()), (Some(1), ""), "{backend:?}: {err}");
        let lines: Vec<_> = err.lines().collect();
        assert!(
            matches!(&lines[..], [l] if l.starts_with("replay: Diverged(Arg) at 8 in root ")),
            "{backend:?}: {err} {:#?}",
            frames[0].1
        );
    }
}

/// A granted capability replays where it arrives, in an item or a helper's
/// result: its tag 5 snapshot takes the receiving unit's next ordinal, as
/// the recorder numbered it, and a grant on that unit's own cell, so its
/// draw is checked against that unit's stream. Before, any item or result
/// holding a capability was refused. Three programs record and replay
/// `Sealed`: a `Random` the root sends itself (RL9_CAPABILITY_HOME), one the
/// root sends a helper (RL9_CAPABILITY_HELPER), replayed as that helper's
/// unit alone, since its end hands the root a survivor, and one a helper's
/// result returns (RL9_RESULT). The oracle is the snapshot's layout in
/// recording.md: handle part (1), tag 5, the ordinal, kind 3 (`Random`), no
/// stored bytes. Renaming the ordinal to one the unit never numbers ends
/// the replay at that `TAKE`, in the unit that took it, with one line.
#[test]
#[ignore = "explicit native integration target"]
fn u5_capabilities_replay_where_they_arrive() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Arrival, Frame};

    let dir = tempfile::tempdir().unwrap();
    let grants = ["Tasks", "Random", "Stdout"];
    let cases = [
        (RL9_CAPABILITY_HOME, "u5c-home", "capability", 0u32, "root", None),
        (RL9_CAPABILITY_HELPER, "u5c-helper", "capability", 1, "root.0", Some("root.0")),
        (RL9_RESULT, "u5c-result", "result", 0, "root", None),
    ];
    // A capability's part where it arrives: handle (1), tag 5, its ordinal
    // there (a send's canonical form has 0), `Random`, no stored bytes.
    let part = |w: &[u8]| matches!(w, [1, 5, o, 3, 0] if *o != 0);
    for backend in [Backend::Cranelift, Backend::Llvm] {
        for (source, name, printed, stream, unit, only) in cases {
            let path = rl9_record_with(source, dir.path(), name, backend, printed, &grants);
            let program = dir.path().join(format!("{name}-{backend:?}"));
            let at = format!("{backend:?} {name}");
            let (code, out, err) = u4_replay(&program, &path, only);
            assert_eq!((code, out.as_str(), err.as_str()), (Some(0), "", ""), "{at}");
            let names: &[&str] = if name == "u5c-home" {
                &["root"]
            } else {
                &["root", "root.0"]
            };
            let streams = unit_streams(&path, &at, names);
            let frames = &streams[stream as usize].1;
            let take = frames
                .iter()
                .position(|f| match f {
                    Frame::Take { body, .. } => {
                        matches!(body.arrival(), Arrival::Item(..) | Arrival::HelperEnd(..))
                            && body.bytes().windows(5).any(part)
                    }
                    _ => false,
                })
                .unwrap_or_else(|| panic!("{at}: no take holds a capability: {frames:#?}"));
            let tape = std::fs::read(&path).unwrap();
            let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
                std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
            })
            .unwrap()
            .1
            .tape_id;
            let wrong = u4_patched(&tape, &tape_id, stream, |payload| {
                let Some(i) = payload.windows(5).position(part) else {
                    return false;
                };
                payload[i + 2] = 99;
                true
            });
            let corrupt = dir.path().join(format!("{name}-{backend:?}-ordinal.tape"));
            std::fs::write(&corrupt, wrong).unwrap();
            let (code, out, err) = u4_replay(&program, &corrupt, only);
            assert_eq!((code, out.as_str()), (Some(1), ""), "{at}: {err}");
            let lines: Vec<_> = err.lines().collect();
            let prefix = format!("replay: Diverged(Take) at {} in {unit} ", take + 1);
            assert!(
                matches!(&lines[..], [l] if l.starts_with(&prefix)),
                "{at}: {err} {frames:#?}"
            );
        }
    }
}
