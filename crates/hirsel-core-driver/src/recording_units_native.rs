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
        options.cas = dir.path().join(format!("rl7-store-{backend:?}"));
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
    use hirsel_tape_format::{Args, Arrival, Frame, op};

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
        options.cas = dir.path().join(format!("rl9-store-{backend:?}"));
        options.output = dir.path().join(format!("rl9-{backend:?}"));
        let program = product.build(&entry, &options).unwrap().executable;
        let path = dir.path().join(format!("rl9-{backend:?}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!((code, out.as_str()), (Some(0), "conn"), "{backend:?}: {err}");
        assert!(!err.contains("truncated"), "{backend:?}: {err}");

        let streams = rl9_streams(&path, &format!("{backend:?}"));
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
/// The streams of a two-unit recording, root then `root.0`: each one's
/// entry and frames.
fn rl9_streams(
    path: &Path,
    at: &str,
) -> Vec<(Option<hirsel_tape_format::Entry>, Vec<hirsel_tape_format::Frame>)> {
    use hirsel_tape_format::Index;
    use std::os::unix::fs::FileExt;
    let file = std::fs::File::open(path).unwrap();
    let read = |at: u64, b: &mut [u8]| file.read_at(b, at);
    let (mut index, _) = Index::open(read).unwrap();
    let paths: Vec<_> = index.paths().collect();
    let names: Vec<_> = paths.iter().map(|(_, n)| n.as_str()).collect();
    assert_eq!(names, ["root", "root.0"], "{at}");
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
    streams
}

/// RL9's `Shared` program: the root moves its only holder of a node into a
/// helper's entry, and the helper drops it before it returns.
const RL9_SHARED: &str = r#"
fn reader(take s: Shared[u64]) -> u64 {
    7
}

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let s: Shared[u64] = shared.new(42)
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, take s, reader))
    if result.unwrap(task.join(take h)) != 7 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, "shared"))
    Ok(())
}
"#;

/// RL9 for a `Shared` node crossing into a helper (U3). Hand-derived from
/// docs/recording.md: the root numbers the node g+1 at `shared.new` and the
/// helper handle g+2 at the spawn. The helper's entry holds the node as a
/// value: 8 plain bytes (the holder word zeroed), one part, a handle (1), a
/// `Shared` snapshot (1), its ordinal 1 in the helper (which has no grants),
/// home stream 0 and node g+1, then the payload, 8 plain bytes holding 42 and
/// no parts. The spawn's arguments hold the same value in canonical form,
/// the ordinal left out. The helper's drop of the only holder is a foreign
/// last drop: it posts the shared release home, so the helper's stream takes
/// nothing for it and the root takes `shared release` (7, stream 0, node
/// g+1). The post precedes the helper's end in home's mailbox, so the root
/// takes the release before the helper end, whose survivor list is empty.
/// Both are taken in the root's `task.join`, parked or not as the helper's
/// timing has it, so their gap and point kind are not this law's.
#[test]
#[ignore = "explicit native integration target"]
fn rl9_shared_crosses_into_a_helper() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Args, Arrival, Frame, HelperOutcome, op};

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let mut request = source_request(RL9_SHARED, dir.path());
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
        options.grant_mask = ["Tasks", "Stdout"]
            .iter()
            .fold(0, |m, g| m | 1 << grant_kind(g).unwrap());
        options.output = dir.path().join(format!("shared-{backend:?}"));
        let program = product.build(&entry, &options).unwrap().executable;
        let path = dir.path().join(format!("shared-{backend:?}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!((code, out.as_str()), (Some(0), "shared"), "{backend:?}: {err}");
        assert!(!err.contains("truncated"), "{backend:?}: {err}");
        let streams = rl9_streams(&path, &format!("{backend:?}"));
        let g = streams[0]
            .1
            .iter()
            .filter(|f| matches!(f, Frame::Grant { .. }))
            .count() as u8;
        let value = |ordinal: Option<u8>| {
            let mut v = vec![8, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1];
            v.extend(ordinal);
            v.extend([0, 0, 0, 0, g + 1, 8, 42, 0, 0, 0, 0, 0, 0, 0, 0]);
            v
        };

        let (entry, helper) = &streams[1];
        let body = entry.as_ref().expect("a helper entry").body();
        assert_eq!(body.value.bytes(), value(Some(1)), "{backend:?}: entry");
        let spawned = [
            body.descriptor.to_vec(),
            65536u64.to_le_bytes().to_vec(),
            value(None),
        ]
        .concat();
        let spawns: Vec<_> = streams[0]
            .1
            .iter()
            .filter_map(|f| match f {
                Frame::Op { op: o, args, .. } if *o == op::UNIT_SPAWN => Some(args.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(spawns, [Args::of_parts(&[&spawned])], "{backend:?}: spawn");
        let takes = |frames: &[Frame]| -> Vec<Vec<u8>> {
            frames
                .iter()
                .filter_map(|f| match f {
                    Frame::Take { body, .. } => Some(body.bytes().to_vec()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(takes(helper), Vec::<Vec<u8>>::new(), "{backend:?}: helper takes");
        let home = takes(&streams[0].1);
        assert_eq!(home.len(), 2, "{backend:?}: root takes {home:?}");
        assert_eq!(home[0], [7, 0, 0, 0, 0, g + 1], "{backend:?}: release");
        let Frame::Take { body, .. } = streams[0]
            .1
            .iter()
            .filter(|f| matches!(f, Frame::Take { .. }))
            .nth(1)
            .unwrap()
        else {
            unreachable!()
        };
        match body.arrival() {
            Arrival::HelperEnd(ordinal, HelperOutcome::Returned(v), survivors) => {
                assert_eq!(ordinal, g as u64 + 2, "{backend:?}: helper ordinal");
                assert_eq!(v.bytes(), [8, 7, 0, 0, 0, 0, 0, 0, 0, 0], "{backend:?}");
                assert!(survivors.is_empty(), "{backend:?}: {survivors:?}");
            }
            a => panic!("{backend:?}: {a:?}"),
        }
    }
}

/// Build `source` with the recording runtime on `backend`, run it recorded,
/// and check it printed `printed` with no stream truncated: the tape's path.
fn rl9_record(source: &str, dir: &Path, name: &str, backend: Backend, printed: &str) -> PathBuf {
    rl9_record_with(source, dir, name, backend, printed, &["Tasks", "Stdout"])
}

/// `rl9_record` with the run granted `grants`.
fn rl9_record_with(
    source: &str,
    dir: &Path,
    name: &str,
    backend: Backend,
    printed: &str,
    grants: &[&str],
) -> PathBuf {
    rl9_record_net(source, dir, name, backend, printed, grants, &[])
}

/// `rl9_record_with`, the run's Net granted `net_rules`.
fn rl9_record_net(
    source: &str,
    dir: &Path,
    name: &str,
    backend: Backend,
    printed: &str,
    grants: &[&str],
    net_rules: &[&str],
) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut request = source_request(source, dir);
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{name}: {:?}", product.diagnostics);
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let mut options = native_options(backend, dir);
    options.runtime = env_record_runtime();
    options.recording = true;
    options.grant_mask = grants
        .iter()
        .fold(0, |m, g| m | 1 << grant_kind(g).unwrap());
    options.net_rules = net_rules
        .iter()
        .map(|r| hirsel_core_net::GrantRule::parse(r).unwrap())
        .collect();
    options.output = dir.join(format!("{name}-{backend:?}"));
    let program = product.build(&entry, &options).unwrap().executable;
    let path = dir.join(format!("{name}-{backend:?}.tape"));
    let (code, out, err) = tape_run(&program, &[], Some(&path));
    assert_eq!((code, out.as_str()), (Some(0), printed), "{name}/{backend:?}: {err}");
    assert!(!err.contains("truncated"), "{name}/{backend:?}: {err}");
    path
}

/// A helper makes a node and sends its only holder to the root, then ends
/// while the root holds it.
const RL9_SURVIVOR: &str = r#"
fn maker(take c: Client[Shared[u64]]) -> u64 {
    let s: Shared[u64] = shared.new(42)
    result.unwrap(conn.try_send(c, take s))
    7
}

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let (c, i): (Client[Shared[u64]], Inbox[Shared[u64]]) = result.unwrap(conn.new(4, bytes 0))
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, take c, maker))
    let t = result.unwrap(conn.receive(i))
    if result.unwrap(task.join(take h)) != 7 { return Error(Exit(status: 3)) }
    {
        let gone = take t
    }
    result.unwrap(stdout.write(out, "survivor"))
    Ok(())
}
"#;

/// A `Shared` node that survives its home helper (U3, CONC-15), hand-derived
/// from docs/recording.md before its first run. The root numbers the
/// connection g+1 and its client g+2 at `conn.new`, and the helper handle
/// g+3 at the spawn. The helper (stream 1, no grants) numbers the arriving
/// client 1 and its node 2 at `shared.new`. The root's takes before its
/// write, in its port's order:
/// - the item: inbox g+1; 8 plain bytes zeroed, one part, a handle (1), a
///   `Shared` snapshot (1) whose arriving holder the root numbers g+4, home
///   stream 1 and node 2, the payload 42 with no parts;
/// - the close of inbox g+1: the helper cleaned the last client;
/// - the helper end: ordinal g+3, the result 7 as 8 plain bytes and no
///   parts, and one survivor: kind 1, stream 1, ordinal 2, amount 48, the
///   node's charge round8(8 + 40) (budgets.md, `Shared` node);
/// - the root's drop of `t`, the last holder: home has ended and forwards to
///   the root, so the root takes `shared release` (7, stream 1, node 2), the
///   survivor it kept, at that drop (kind 0).
///
/// The inbox's cleaning after the write is not this law's. The first three
/// are taken in `conn.receive` and `task.join`, parked or
/// not as the helper's timing has it, so their gaps and point kinds are not
/// this law's. The helper's stream holds no take.
#[test]
#[ignore = "explicit native integration target"]
fn rl9_shared_survives_its_helper() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Frame, TakePoint, op};

    let dir = tempfile::tempdir().unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let path = rl9_record(RL9_SURVIVOR, dir.path(), "survivor", backend, "survivor");
        let streams = rl9_streams(&path, &format!("{backend:?}"));
        // The root's frames before its `stdout.write`, its first operation
        // that is no unit operation.
        let unit_ops = [op::UNIT_SPAWN, op::CONN_TRY_SEND, op::CONN_SEND, op::CLOSE];
        let write = streams[0]
            .1
            .iter()
            .position(|f| match f {
                Frame::Op { op: o, .. } | Frame::Issue { op: o, .. } => !unit_ops.contains(o),
                _ => false,
            })
            .expect("the stdout write");
        let root = &streams[0].1[..write];
        let g = root
            .iter()
            .filter(|f| matches!(f, Frame::Grant { .. }))
            .count() as u8;
        let takes = |frames: &[Frame]| -> Vec<(TakePoint, Vec<u8>)> {
            frames
                .iter()
                .filter_map(|f| match f {
                    Frame::Take { point, body, .. } => Some((*point, body.bytes().to_vec())),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(takes(&streams[1].1), [], "{backend:?}: helper takes");
        let item = [
            vec![0, g + 1, 8],
            vec![0; 8],
            vec![1, 1, 1, g + 4, 1, 0, 0, 0, 2, 8, 42],
            vec![0; 7],
            vec![0],
        ]
        .concat();
        let end = [
            vec![4, g + 3, 0, 8, 7],
            vec![0; 7],
            vec![0, 1, 1, 1, 0, 0, 0, 2, 48],
            vec![0; 7],
        ]
        .concat();
        let bodies: Vec<_> = takes(root).into_iter().map(|(_, b)| b).collect();
        assert_eq!(
            bodies,
            [item, vec![1, g + 1], end, vec![7, 1, 0, 0, 0, 2]],
            "{backend:?}: root takes"
        );
        assert_eq!(
            takes(root).last().map(|t| t.0),
            Some(TakePoint::Operation),
            "{backend:?}: the release is at the drop"
        );
    }
}

/// One unit's send that waits: the root fills its one-slot connection, a task
/// it spawns sends again and waits, and the root's receive frees the slot.
const RL9_ACCEPTED: &str = r#"
fn sender(take c: Client[u64]) -> u64 waits {
    result.unwrap(conn.send(c, 7))
    1
}

fn at_once(take n: u64) -> u64 { n }

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let (c, i): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.try_send(c, 5))
    let w = task.spawn(tasks, bytes 65536, take c, sender)
    let y = task.spawn(tasks, bytes 65536, 0, at_once)
    let z = result.unwrap(task.join(take y))
    let a = result.unwrap(conn.receive(i))
    let b = result.unwrap(conn.receive(i))
    let d = result.unwrap(task.join(take w))
    if z != 0 || a != 5 || b != 7 || d != 1 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, "accepted"))
    Ok(())
}
"#;

/// A waiting send's `Accepted` settlement (recording.md, "TAKE", amended by
/// unit-tapes-u3e-2692), hand-derived before its first run. The root numbers
/// the connection g+1 and its client g+2 at `conn.new`; `try_send` of 5
/// fills the one slot, so the task's `conn.send` of 7 on client g+2 waits:
/// an `ISSUE`, the stream's only `CONN_SEND`. The root's first receive takes
/// 5 and frees the slot, the send is accepted, and the root takes its
/// settlement: tag 3, the `ISSUE`'s position (its frame's place in the
/// stream, counted from 1), outcome 0 and nothing after it. The items taken
/// are 5 then 7, from inbox g+1.
#[test]
#[ignore = "explicit native integration target"]
fn rl9_waiting_send_is_accepted() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Arrival, Frame, op};

    let dir = tempfile::tempdir().unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let path = rl9_record(RL9_ACCEPTED, dir.path(), "accepted", backend, "accepted");
        let frames = rl8_root_frames(&path);
        let g = frames
            .iter()
            .filter(|f| matches!(f, Frame::Grant { .. }))
            .count() as u64;
        let issues: Vec<_> = frames
            .iter()
            .enumerate()
            .filter_map(|(at, f)| match f {
                Frame::Issue { op: o, object, .. } if *o == op::CONN_SEND => {
                    Some((at as u64 + 1, *object))
                }
                _ => None,
            })
            .collect();
        let [(position, client)] = issues[..] else {
            panic!("{backend:?}: {issues:?}")
        };
        assert_eq!(client, g + 2, "{backend:?}: the waiting send's client");
        assert!(position < 128, "{backend:?}: a one-byte varint");
        let mut settlements = Vec::new();
        let mut items = Vec::new();
        for f in &frames {
            if let Frame::Take { body, .. } = f {
                match body.arrival() {
                    Arrival::Settlement(..) => settlements.push(body.bytes().to_vec()),
                    Arrival::Item(inbox, v) => items.push((inbox, v.bytes().to_vec())),
                    _ => {}
                }
            }
        }
        assert_eq!(
            settlements,
            [vec![3, position as u8, 0]],
            "{backend:?}: settlement"
        );
        let word = |v: u8| [vec![8, v], vec![0; 7], vec![0]].concat();
        assert_eq!(
            items,
            [(g + 1, word(5)), (g + 1, word(7))],
            "{backend:?}: items"
        );
    }
}

/// The root sends its `Random` over its own connection and draws through
/// what it takes.
const RL9_CAPABILITY_HOME: &str = r#"
pub fn main(tasks: Tasks, r: Random, out: Stdout) -> Result[(), Exit] waits {
    let (c, i): (Client[Random], Inbox[Random]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.try_send(c, r))
    let s = result.unwrap(conn.try_receive(i))
    let mut b: Bytes = list.with_capacity(8)
    host.random(s, mut b)
    if list.len(b) != 8 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, "capability"))
    Ok(())
}
"#;

/// The root sends its `Random` to a helper over the helper's connection, and
/// the helper draws through it.
const RL9_CAPABILITY_HELPER: &str = r#"
fn drawer(take hello: Client[Client[Random]]) -> u64 waits {
    let (back, back_in): (Client[Random], Inbox[Random]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.send(hello, take back))
    let r = result.unwrap(conn.receive(back_in))
    let mut b: Bytes = list.with_capacity(8)
    host.random(r, mut b)
    list.len(b)
}

pub fn main(tasks: Tasks, r: Random, out: Stdout) -> Result[(), Exit] waits {
    let (hello, hello_in): (Client[Client[Random]], Inbox[Client[Random]]) = result.unwrap(conn.new(1, bytes 0))
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, take hello, drawer))
    let back = result.unwrap(conn.receive(hello_in))
    result.unwrap(conn.send(back, r))
    if result.unwrap(task.join(take h)) != 8 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, "capability"))
    Ok(())
}
"#;

/// A granted capability that crosses in an item (recording.md, "Handle
/// snapshots"; U3), hand-derived before the first run. An item holding a
/// `Random` is 8 plain bytes (the zeroed capability word), one part, a handle
/// (1) whose tag 5 snapshot names its new ordinal where it arrives, kind 3
/// and no stored bytes. Its draw is then an operation of the unit that took
/// it, on that ordinal:
/// - at home, the root numbers its connection g+1 and client g+2, and the
///   `Random` that arrives g+3: the take is `[0, g+1]` and that value, and
///   the root's `RANDOM` names g+3, not the grant's own ordinal;
/// - in a helper (stream 1, no grants), `hello` is 1 from its entry, `back` 2
///   and its client 3, and the arriving `Random` 4: the helper's take is
///   `[0, 2]` and the value, its `RANDOM` names 4, and the root draws
///   nothing.
///
/// RL9 pairs the sent item, canonical (no ordinal), with each take.
#[test]
#[ignore = "explicit native integration target"]
fn rl9_capability_crosses_in_an_item() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Args, Arrival, Frame, Value, op};

    let grants = ["Tasks", "Random", "Stdout"];
    let item = |ordinal: u8| [vec![8], vec![0; 8], vec![1, 1, 5, ordinal, 3, 0]].concat();
    let canonical = Value::decode(&item(1)).unwrap().canonical();
    let takes = |frames: &[Frame]| -> Vec<Vec<u8>> {
        frames
            .iter()
            .filter_map(|f| match f {
                Frame::Take { body, .. } if matches!(body.arrival(), Arrival::Item(..)) => {
                    Some(body.bytes().to_vec())
                }
                _ => None,
            })
            .collect()
    };
    let draws = |frames: &[Frame]| -> Vec<u64> {
        frames
            .iter()
            .filter_map(|f| match f {
                Frame::Op { op: o, object, .. } if *o == op::RANDOM => Some(*object),
                _ => None,
            })
            .collect()
    };
    let sent = |frames: &[Frame], o: u8| -> Vec<Args> {
        frames
            .iter()
            .filter_map(|f| match f {
                Frame::Op { op: x, args, .. } | Frame::Issue { op: x, args, .. } if *x == o => {
                    Some(args.clone())
                }
                _ => None,
            })
            .collect()
    };
    let dir = tempfile::tempdir().unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let path = rl9_record_with(RL9_CAPABILITY_HOME, dir.path(), "cap-home", backend, "capability", &grants);
        let frames = rl8_root_frames(&path);
        let g = frames
            .iter()
            .filter(|f| matches!(f, Frame::Grant { .. }))
            .count() as u8;
        assert_eq!(
            takes(&frames),
            [[vec![0, g + 1], item(g + 3)].concat()],
            "{backend:?}: home take"
        );
        assert_eq!(draws(&frames), [u64::from(g + 3)], "{backend:?}: home draw");
        assert_eq!(
            sent(&frames, op::CONN_TRY_SEND),
            [Args::of_parts(&[&canonical])],
            "{backend:?}: home send"
        );

        let path = rl9_record_with(
            RL9_CAPABILITY_HELPER,
            dir.path(),
            "cap-helper",
            backend,
            "capability",
            &grants,
        );
        let streams = rl9_streams(&path, &format!("{backend:?}"));
        let helper = &streams[1].1;
        assert_eq!(takes(helper), [[vec![0, 2], item(4)].concat()], "{backend:?}: helper take");
        assert_eq!(draws(helper), [4], "{backend:?}: helper draw");
        assert_eq!(draws(&streams[0].1), [] as [u64; 0], "{backend:?}: root draws");
        let root_sends = sent(&streams[0].1, op::CONN_SEND);
        assert_eq!(
            root_sends.last(),
            Some(&Args::of_parts(&[&canonical])),
            "{backend:?}: root send"
        );
    }
}

// RL8 natively, the ordering seam (feature `testing`).
include!("recording_rl8_native.rs");
// Handle snapshots that cross in items, entries and results.
include!("recording_handles_native.rs");
