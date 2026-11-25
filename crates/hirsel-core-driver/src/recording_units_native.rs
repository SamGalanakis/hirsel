// RL7's recording half (docs/recording.md, slice U2): each helper of a
// recorded run writes its own stream.
/// RL7's program at U2: the root hands its `Random` to one helper and spawns a
/// second through a function parameter. Connections between helpers cross as
/// handle snapshots, which U3 writes.
const RL7_UNITS: &str = r#"
type Draw: cross {
    r: Random,
    n: u64
}

fn draw(take d: Draw) -> u64 {
    let Draw(r: r, n: n) = take d
    let mut b: Bytes = list.with_capacity(8)
    host.random(r, mut b)
    n + list.len(b)
}

fn twice(take n: u64) -> u64 { n * 2 }

fn launch(tasks: Tasks, f: fn(take u64) -> u64, n: u64) -> u64 waits {
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, n, f))
    result.unwrap(task.join(take h))
}

pub fn main(tasks: Tasks, r: Random, out: Stdout) -> Result[(), Exit] waits {
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, Draw(r: r, n: 3), draw))
    let a = launch(tasks, twice, 21)
    let b = result.unwrap(task.join(take h))
    if a != 42 || b != 11 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, "units"))
    Ok(())
}
"#;

/// RL7, recording half (U2): each helper writes its own stream of the one
/// file, named by spawn index, with its entry, its own host operations and
/// its `END`; the root's `END` is the file's last block. Every expected byte
/// is derived from docs/recording.md and PROG-9 by hand.
#[test]
#[ignore = "explicit native integration target"]
fn rl7_helpers_record_their_own_streams() {
    // The recorder opens its tape beneath the tape directory (openat2).
    crate::openat2_required::require();
    use hirsel_tape_format::{Frame, Index, op};
    use std::os::unix::fs::FileExt;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let mut request = source_request(RL7_UNITS, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    // A u64 value: 8 plain bytes, no parts.
    let word = |v: u8| [vec![8, v], vec![0; 7], vec![0]].concat();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let mut options = native_options(backend, dir.path());
        options.runtime = env_record_runtime();
        options.recording = true;
        options.grant_mask = ["Tasks", "Random", "Stdout"]
            .iter()
            .fold(0, |m, g| m | 1 << grant_kind(g).unwrap());
        options.store = dir.path().join(format!("rl7-store-{backend:?}"));
        options.output = dir.path().join(format!("rl7-{backend:?}"));
        let program = product.build(&entry, &options).unwrap().executable;
        let path = dir.path().join(format!("rl7-{backend:?}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!(
            (code, out.as_str()),
            (Some(0), "units"),
            "{backend:?}: {err}"
        );
        assert!(!err.contains("truncated"), "{backend:?}: {err}");

        // The index refuses any block after the root's END, so a clean open
        // also shows the root END is the last block.
        let file = std::fs::File::open(&path).unwrap();
        let read = |at: u64, b: &mut [u8]| file.read_at(b, at);
        let (mut index, _) = Index::open(read).unwrap();
        let paths: Vec<_> = index.paths().collect();
        let names: Vec<_> = paths.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, ["root", "root.0", "root.1"], "{backend:?}");
        let mut streams = Vec::new();
        for (id, _) in &paths {
            let mut blocks = index.take(*id).unwrap();
            let mut decoder = blocks.decoder(read).unwrap();
            let mut frames = Vec::new();
            let mut last = 0;
            while let Some((flags, payload)) = blocks.next(read).unwrap() {
                last = flags;
                frames.extend(
                    decoder
                        .block(payload, flags & hirsel_tape_format::END != 0)
                        .unwrap(),
                );
            }
            assert_eq!(
                last,
                hirsel_tape_format::END,
                "{backend:?} stream {id} ends"
            );
            streams.push((decoder.take_entry(), frames));
        }
        let randoms = |frames: &[Frame]| {
            frames
                .iter()
                .filter(|f| matches!(f, Frame::Op { op: o, .. } if *o == op::RANDOM))
                .count()
        };
        assert_eq!(
            randoms(&streams[0].1),
            0,
            "{backend:?}: the root draws nothing"
        );
        assert_eq!(randoms(&streams[1].1), 1, "{backend:?}: root.0 draws once");
        assert_eq!(randoms(&streams[2].1), 0, "{backend:?}");
        // Plain bytes: `r`'s zeroed capability word, then `n`; one part, a
        // tag 5 snapshot of `Random` (3) at the helper's ordinal 1, no rules.
        let draw_x = [
            vec![16],
            vec![0; 8],
            vec![3],
            vec![0; 7],
            vec![1, 1, 5, 1, 3, 0],
        ]
        .concat();
        let expected: [(&[u8], Vec<u8>, Vec<u8>); 2] = [
            (
                b"\x01app@1/main/draw(take app@1/main/Draw)->u64",
                draw_x,
                word(11),
            ),
            (b"\x01app@1/main/twice(take u64)->u64", word(21), word(42)),
        ];
        for (spawn, (descriptor, x, r)) in expected.iter().enumerate() {
            let (entry, frames) = &streams[spawn + 1];
            let entry = entry.as_ref().expect("a helper entry");
            assert_eq!(
                (entry.parent, entry.spawn),
                (0, spawn as u64),
                "{backend:?}"
            );
            let body = entry.body();
            assert_eq!(body.descriptor, *descriptor, "{backend:?} root.{spawn}");
            assert_eq!(
                (body.n, body.bound),
                (65536, None),
                "{backend:?} root.{spawn}"
            );
            assert_eq!(body.value.bytes(), x.as_slice(), "{backend:?} root.{spawn}");
            let Some(Frame::HelperEnd { ending, .. }) = frames.last() else {
                panic!("{backend:?} root.{spawn}: {frames:?}");
            };
            assert_eq!(
                (ending.kind(), ending.body()),
                (0, r.as_slice()),
                "{backend:?}"
            );
        }
        assert!(
            matches!(streams[0].1.last(), Some(Frame::End { .. })),
            "{backend:?}"
        );
    }
}

/// RL9's program at U3: the root hands the helper a client of its `hello`
/// connection in the entry; the helper makes `back`, sends its client over
/// `hello`, and receives two items the root sends over `back`.
const RL9_UNITS: &str = r#"
fn produce(take hello: Client[Client[u64]]) -> u64 waits {
    let (back, back_in): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(2, bytes 0))
    result.unwrap(conn.send(hello, take back))
    let a = result.unwrap(conn.receive(back_in))
    let b = result.unwrap(conn.receive(back_in))
    a * 10 + b
}

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let (hello, hello_in): (Client[Client[u64]], Inbox[Client[u64]]) = result.unwrap(conn.new(1, bytes 0))
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, take hello, produce))
    let back = result.unwrap(conn.receive(hello_in))
    result.unwrap(conn.send(back, 5))
    result.unwrap(conn.send(back, 7))
    if result.unwrap(task.join(take h)) != 57 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, "conn"))
    Ok(())
}
"#;

/// RL9, streams of one recording agree (U3): a test-only reader pairs each
/// `CONN_SEND` with the `TAKE` it reached and the `UNIT_SPAWN` with its
/// child's entry. The oracle is the hand-written list of sends below, its
/// snapshots and ordinals derived from docs/recording.md by hand: a stream's
/// object ordinals follow its grants, `conn.new` numbers the connection and
/// then its client, a spawn numbers the helper handle, and a handle that
/// arrives is numbered at its take; a canonical value leaves out each
/// snapshot's ordinal.
#[test]
#[ignore = "explicit native integration target"]
fn rl9_sent_items_match_their_takes() {
    // The recorder opens its tape beneath the tape directory (openat2).
    crate::openat2_required::require();
    use hirsel_tape_format::{Args, Arrival, Frame, Index, op};
    use std::os::unix::fs::FileExt;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let mut request = source_request(RL9_UNITS, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    // A u64 item: 8 plain bytes, no parts.
    let word = |v: u8| [vec![8, v], vec![0; 7], vec![0]].concat();
    // A client value: its zeroed handle word, one part, a tag 1 handle
    // snapshot of kind 0 (its ordinal left out), the inbox stream, the
    // connection ordinal there, `cap` and room `b`.
    let client = |stream: u32, conn: u8, cap: u8| {
        [
            vec![8],
            vec![0; 8],
            vec![1, 1, 0],
            stream.to_le_bytes().to_vec(),
            vec![conn, cap],
            vec![0; 7],
            vec![0; 8],
        ]
        .concat()
    };
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let mut options = native_options(backend, dir.path());
        options.runtime = env_record_runtime();
        options.recording = true;
        options.grant_mask = ["Tasks", "Stdout"]
            .iter()
            .fold(0, |m, g| m | 1 << grant_kind(g).unwrap());
        options.store = dir.path().join(format!("rl9-store-{backend:?}"));
        options.output = dir.path().join(format!("rl9-{backend:?}"));
        let program = product.build(&entry, &options).unwrap().executable;
        let path = dir.path().join(format!("rl9-{backend:?}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!((code, out.as_str()), (Some(0), "conn"), "{backend:?}: {err}");
        assert!(!err.contains("truncated"), "{backend:?}: {err}");

        let file = std::fs::File::open(&path).unwrap();
        let read = |at: u64, b: &mut [u8]| file.read_at(b, at);
        let (mut index, _) = Index::open(read).unwrap();
        let paths: Vec<_> = index.paths().collect();
        let names: Vec<_> = paths.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, ["root", "root.0"], "{backend:?}");
        let mut streams = Vec::new();
        for (id, _) in &paths {
            let mut blocks = index.take(*id).unwrap();
            let mut decoder = blocks.decoder(read).unwrap();
            let mut frames = Vec::new();
            while let Some((flags, payload)) = blocks.next(read).unwrap() {
                frames.extend(
                    decoder
                        .block(payload, flags & hirsel_tape_format::END != 0)
                        .unwrap(),
                );
            }
            streams.push((decoder.take_entry(), frames));
        }
        // Per stream: each send's (client ordinal, arguments) and each
        // item take's (inbox ordinal, canonical item).
        let sends = |frames: &[Frame]| -> Vec<(u64, Args)> {
            frames
                .iter()
                .filter_map(|f| match f {
                    Frame::Op { op: o, object, args, .. }
                    | Frame::Issue { op: o, object, args, .. }
                        if *o == op::CONN_SEND =>
                    {
                        Some((*object, args.clone()))
                    }
                    _ => None,
                })
                .collect()
        };
        let items = |frames: &[Frame]| -> Vec<(u64, Vec<u8>)> {
            frames
                .iter()
                .filter_map(|f| match f {
                    Frame::Take { body, .. } => match body.arrival() {
                        Arrival::Item(inbox, value) => Some((inbox, value.canonical())),
                        _ => None,
                    },
                    _ => None,
                })
                .collect()
        };
        let grants = streams[0]
            .1
            .iter()
            .filter(|f| matches!(f, Frame::Grant { .. }))
            .count() as u64;
        // The root: `hello` g+1 and its client g+2, the helper handle g+3,
        // and `back`'s client g+4 at its take. The helper (stream 1): `hello`
        // 1 from its entry, then `back` 2 and its client 3.
        let (hello, back_at_root) = (grants + 1, grants + 4);
        let hello_client = client(0, hello as u8, 1);
        let back_client = client(1, 2, 2);
        // The hand-written sends: (sending stream, client ordinal there,
        // item), then (receiving stream, inbox ordinal there).
        let expected: [((usize, u64, Vec<u8>), (usize, u64)); 3] = [
            ((1, 1, back_client.clone()), (0, hello)),
            ((0, back_at_root, word(5)), (1, 2)),
            ((0, back_at_root, word(7)), (1, 2)),
        ];
        for s in 0..2 {
            let sent: Vec<_> = expected
                .iter()
                .filter(|((from, ..), _)| *from == s)
                .map(|((_, c, item), _)| (*c, Args::of_parts(&[item])))
                .collect();
            assert_eq!(sends(&streams[s].1), sent, "{backend:?}: stream {s} sends");
            let taken: Vec<_> = expected
                .iter()
                .filter(|(_, (to, _))| *to == s)
                .map(|((.., item), (_, inbox))| (*inbox, item.clone()))
                .collect();
            assert_eq!(items(&streams[s].1), taken, "{backend:?}: stream {s} takes");
        }
        // The root numbers `back` at its take.
        let back_snapshot = streams[0].1.iter().find_map(|f| match f {
            Frame::Take { body, .. } => match body.arrival() {
                Arrival::Item(_, value) => Some(value.bytes().to_vec()),
                _ => None,
            },
            _ => None,
        });
        let mut numbered = back_client.clone();
        numbered.insert(12, back_at_root as u8);
        assert_eq!(back_snapshot, Some(numbered), "{backend:?}");

        // The spawn and its child's entry: `hello` numbered 1 in the helper.
        let (entry, _) = &streams[1];
        let entry = entry.as_ref().expect("a helper entry");
        let body = entry.body();
        let mut x = hello_client.clone();
        x.insert(12, 1);
        assert_eq!(body.value.bytes(), x.as_slice(), "{backend:?}");
        assert!(
            body.descriptor.starts_with(b"\x01app@1/main/produce("),
            "{backend:?}: {:?}",
            String::from_utf8_lossy(body.descriptor)
        );
        let spawned = [
            body.descriptor.to_vec(),
            65536u64.to_le_bytes().to_vec(),
            hello_client,
        ]
        .concat();
        let spawns: Vec<_> = streams[0]
            .1
            .iter()
            .filter_map(|f| match f {
                Frame::Op { op: o, object, args, out, .. } if *o == op::UNIT_SPAWN => {
                    Some((*object, args.clone(), out.value))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            spawns,
            [(0, Args::of_parts(&[&spawned]), 1)],
            "{backend:?}: the spawn names its child's entry and stream"
        );
    }
}
