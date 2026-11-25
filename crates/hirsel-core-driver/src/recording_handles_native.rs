// Handle snapshots that cross units in items, entries and results
// (docs/recording.md, "Handle snapshots", unit tapes U3). Included from
// recording_units_native.rs, whose helpers it uses.

/// The root narrows its Net, sends the granted Net and the narrowing to
/// itself, listens through the narrowing it takes, and moves the narrowing
/// into a helper's entry.
const RL9_NET: &str = r#"
fn holder(take n: Net) -> u64 {
    7
}

pub fn main(n: Net, tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let ip = V4(2130706433)
    let mut rules: List[Rule] = list.new()
    list.push(mut rules, Rule(dir: Listen, prefix: ip, bits: 32, ports: (0, 65535), reach: Any))
    let narrowed = result.unwrap(net.narrow(n, rules))
    let (c, i): (Client[Net], Inbox[Net]) = result.unwrap(conn.new(2, bytes 0))
    result.unwrap(conn.try_send(c, n))
    result.unwrap(conn.try_send(c, narrowed))
    let granted = result.unwrap(conn.try_receive(i))
    let taken = result.unwrap(conn.try_receive(i))
    let (listener, _) = result.unwrap(host.listen(taken, Addr(ip: ip, port: 0), 1))
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, narrowed, holder))
    if result.unwrap(task.join(take h)) != 7 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, "net"))
    Ok(())
}
"#;

/// A Net, granted or narrowed, crosses in an item and in an entry
/// (recording.md, "Snapshots", tag 5), hand-derived before the first run.
/// The run's Net is granted `Listen 127.0.0.0/8 0-65535`; the program
/// narrows it to `Listen 127.0.0.1/32 0-65535`, whose intersection with the
/// grant is that rule (NET-6).
/// - A Net snapshot's stored bytes are the rule count and the rules as the
///   header writes them after the Net kind byte. One address rule is 23
///   bytes: direction (Listen 1), family (IPv4 0), `hi` 0 as a `u64`, `lo`
///   the IPv4 address as a `u64`, the prefix bits, and the ports as two
///   `u16`s. So the stored bytes are 24: count 1 and the rule.
/// - An item holding a Net is 8 plain bytes (the zeroed capability word), one
///   part, a handle (1): tag 5, its ordinal, kind 1, then the stored bytes.
/// - The root's grants are g GRANT frames; the narrowing takes ordinal g+1
///   with no frame, the connection g+2 and its client g+3; the granted Net
///   arrives as g+4 and the narrowing as g+5. Both takes name inbox g+2.
/// - `host.listen` through the narrowing that arrived names g+5.
/// - The helper's entry holds the narrowing as its ordinal 1.
///
/// RL9 pairs each sent item, canonical (no ordinal), with its take.
#[test]
#[ignore = "explicit native integration target"]
fn rl9_net_crosses_in_an_item_and_an_entry() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Args, Arrival, Frame, Value, op};

    let rule = |lo: u8, bits: u8| -> Vec<u8> {
        [
            vec![1, 0],
            vec![0; 8],
            vec![lo, 0, 0, 0x7f, 0, 0, 0, 0],
            vec![bits, 0, 0, 0xff, 0xff],
        ]
        .concat()
    };
    let granted = rule(0, 8);
    let narrowed = rule(1, 32);
    let net = |ordinal: u8, rule: &[u8]| -> Vec<u8> {
        [vec![8], vec![0; 8], vec![1, 1, 5, ordinal, 1, 24, 1], rule.to_vec()].concat()
    };
    let canonical = |rule: &[u8]| Value::decode(&net(1, rule)).unwrap().canonical();
    let dir = tempfile::tempdir().unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let path = rl9_record_net(
            RL9_NET,
            dir.path(),
            "net",
            backend,
            "net",
            &["Net", "Tasks", "Stdout"],
            &["Listen 127.0.0.0/8 0-65535"],
        );
        let streams = rl9_streams(&path, &format!("{backend:?}"));
        let root = &streams[0].1;
        let g = root
            .iter()
            .filter(|f| matches!(f, Frame::Grant { .. }))
            .count() as u8;
        let takes: Vec<_> = root
            .iter()
            .filter_map(|f| match f {
                Frame::Take { body, .. } if matches!(body.arrival(), Arrival::Item(..)) => {
                    Some(body.bytes().to_vec())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            takes,
            [
                [vec![0, g + 2], net(g + 4, &granted)].concat(),
                [vec![0, g + 2], net(g + 5, &narrowed)].concat(),
            ],
            "{backend:?}: takes"
        );
        let sends: Vec<_> = root
            .iter()
            .filter_map(|f| match f {
                Frame::Op { op: o, args, .. } if *o == op::CONN_TRY_SEND => Some(args.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            sends,
            [
                Args::of_parts(&[&canonical(&granted)]),
                Args::of_parts(&[&canonical(&narrowed)]),
            ],
            "{backend:?}: sends"
        );
        let listens: Vec<_> = root
            .iter()
            .filter_map(|f| match f {
                Frame::Op { op: o, object, .. } | Frame::Issue { op: o, object, .. }
                    if *o == op::LISTEN =>
                {
                    Some(*object)
                }
                _ => None,
            })
            .collect();
        assert_eq!(listens, [u64::from(g + 5)], "{backend:?}: listen");
        let entry = streams[1].0.as_ref().expect("a helper entry");
        assert_eq!(
            entry.body().value.bytes(),
            net(1, &narrowed).as_slice(),
            "{backend:?}: entry"
        );
    }
}

/// The root sends its Env and its Cred to itself, reads a variable through
/// the Env it took, and moves the Cred it took into a helper's entry, which
/// fetches through it.
const RL9_HOST: &str = r#"
fn fetcher(take c: Cred) -> u64 waits {
    let mut key_id = string.with_capacity(64)
    let mut token = string.with_capacity(64)
    let (_key, _expires) = result.unwrap(cred.fetch(c, mut key_id, mut token, Never))
    7
}

pub fn main(e: Env, c: Cred, tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let (ec, ei): (Client[Env], Inbox[Env]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.try_send(ec, e))
    let read = result.unwrap(conn.try_receive(ei))
    let value = match env.get(read, "HIRSEL_U3F") {
        Ok(Some(s)) => s,
        _ => return Error(Exit(status: 4)),
    }
    let (cc, ci): (Client[Cred], Inbox[Cred]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.try_send(cc, c))
    let fetch = result.unwrap(conn.try_receive(ci))
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, fetch, fetcher))
    if result.unwrap(task.join(take h)) != 7 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, value))
    Ok(())
}
"#;

/// An Env and a Cred cross in an item and a Cred in an entry (recording.md,
/// "Snapshots", tag 5), hand-derived before the first run. The run's Env is
/// granted the pattern `HIRSEL_U3F`, set to `env`; its Cred `c` is
/// `imds:127.0.0.1:<port>` with policy `mac`.
/// - Their stored bytes are what the header writes after the kind byte. An
///   Env's: its patterns' text as bytes (10, `HIRSEL_U3F`), the entry count
///   1, then the entry's name and value as bytes (10, `HIRSEL_U3F`; 3,
///   `env`), 27 bytes. A Cred's: parameter, source and policy as bytes.
/// - An item holding one is 8 plain bytes (the zeroed word), one part, a
///   handle (1): tag 5, its ordinal, kind (Env 7, Cred 14), the stored
///   bytes' length and the stored bytes.
/// - After g GRANT frames, the first connection takes g+1 (inbox) and g+2,
///   the Env arrives as g+3; the second g+4 and g+5, the Cred g+6.
/// - The helper's entry holds the Cred as its ordinal 1, and its fetch is a
///   `CRED_FETCH` on that ordinal in the helper's stream, none in the root's.
#[test]
#[ignore = "explicit native integration target"]
fn rl9_env_and_cred_cross_in_an_item_and_an_entry() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Arrival, Frame, op};

    let field = |text: &str| [vec![text.len() as u8], text.as_bytes().to_vec()].concat();
    let env = [field("HIRSEL_U3F"), vec![1], field("HIRSEL_U3F"), field("env")].concat();
    assert_eq!(env.len(), 27);
    let value = |ordinal: u8, kind: u8, stored: &[u8]| -> Vec<u8> {
        [
            vec![8],
            vec![0; 8],
            vec![1, 1, 5, ordinal, kind, stored.len() as u8],
            stored.to_vec(),
        ]
        .concat()
    };
    let fetches = |frames: &[Frame]| -> Vec<u64> {
        frames
            .iter()
            .filter_map(|f| match f {
                Frame::Op { op: o, object, .. } | Frame::Issue { op: o, object, .. }
                    if *o == op::CRED_FETCH =>
                {
                    Some(*object)
                }
                _ => None,
            })
            .collect()
    };
    let dir = tempfile::tempdir().unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let (port, server) = fake_imds(b"wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY", b"T".to_vec(), 1);
        let source = format!("imds:127.0.0.1:{port}");
        let cred = [field("c"), field(&source), field("mac")].concat();
        let path = rl9_record_configured(
            RL9_HOST,
            dir.path(),
            "host",
            (backend, "env"),
            &["Env", "Tasks", "Stdout"],
            |options| {
                options.env_patterns = hirsel_core_net::EnvPatterns::parse("HIRSEL_U3F");
                options.cred_grants =
                    vec![hirsel_core_net::CredGrant::new("c", &source, "mac").unwrap()];
            },
            &[("HIRSEL_U3F", "env")],
        );
        server.join().unwrap();
        let streams = rl9_streams(&path, &format!("{backend:?}"));
        let root = &streams[0].1;
        let g = root
            .iter()
            .filter(|f| matches!(f, Frame::Grant { .. }))
            .count() as u8;
        let takes: Vec<_> = root
            .iter()
            .filter_map(|f| match f {
                Frame::Take { body, .. } if matches!(body.arrival(), Arrival::Item(..)) => {
                    Some(body.bytes().to_vec())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            takes,
            [
                [vec![0, g + 1], value(g + 3, 7, &env)].concat(),
                [vec![0, g + 4], value(g + 6, 14, &cred)].concat(),
            ],
            "{backend:?}: takes"
        );
        let entry = streams[1].0.as_ref().expect("a helper entry");
        assert_eq!(
            entry.body().value.bytes(),
            value(1, 14, &cred).as_slice(),
            "{backend:?}: entry"
        );
        assert_eq!(fetches(root), [] as [u64; 0], "{backend:?}: root fetches");
        assert_eq!(fetches(&streams[1].1), [1], "{backend:?}: helper fetch");
    }
}

/// A helper takes a `Random` and a client in its entry and returns both;
/// the root draws through the returned `Random` and sends through the
/// returned client.
const RL9_RESULT: &str = r#"
fn keeper(take p: (Random, Client[u64])) -> (Random, Client[u64]) {
    p
}

pub fn main(tasks: Tasks, r: Random, out: Stdout) -> Result[(), Exit] waits {
    let (c, i): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(2, bytes 0))
    let p: (Random, Client[u64]) = (r, take c)
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, take p, keeper))
    let (back, client) = result.unwrap(task.join(take h))
    let mut b: Bytes = list.with_capacity(8)
    host.random(back, mut b)
    result.unwrap(conn.try_send(client, 5))
    if result.unwrap(conn.try_receive(i)) != 5 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, "result"))
    Ok(())
}
"#;

/// A helper's result that holds a capability and a client (recording.md,
/// "Snapshots"), hand-derived before the first run. The root numbers its
/// connection g+1 and client g+2 at `conn.new` and the helper g+3 at the
/// spawn. The result `(Random, Client[u64])` is 16 plain bytes, both words
/// zeroed, then two parts in offset order: the `Random`'s tag 5 snapshot
/// (kind 3, no stored bytes) and the client's tag 0 snapshot (stream 0,
/// connection g+1, `cap` 2, room 0).
/// - The helper's `END` holds it in canonical form, no ordinals.
/// - The root's end take: helper g+3, kind 0, the result with the `Random`
///   numbered g+4 and the client g+5, no survivors.
/// - The root's draw is a `RANDOM` on g+4, and its send through the returned
///   client is a `CONN_TRY_SEND` on g+5 that reaches inbox g+1: the item take
///   of 5.
///
/// The takes of the run's end, inbox g+1's close (tag 1), its seal answer
/// (2) and the root's last release of connection g+1 (6), are not this
/// law's.
#[test]
#[ignore = "explicit native integration target"]
fn rl9_result_crosses_with_its_handles() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Arrival, Frame, op};

    // The result: 16 zeroed plain bytes, two parts in offset order: the
    // `Random` (handle 1, tag 5, its ordinal, kind 3, no stored bytes) and
    // the client (handle 1, tag 0, its ordinal, home stream 0, connection
    // `conn`, `cap` 2, room 0). `ordinals` is empty in canonical form.
    let result = |ordinals: &[u8], conn: u8| -> Vec<u8> {
        let at = |i: usize| ordinals.get(i).map(|o| vec![*o]).unwrap_or_default();
        [
            vec![16],
            vec![0; 16],
            vec![2, 1, 5],
            at(0),
            vec![3, 0, 1, 0],
            at(1),
            vec![0; 4],
            vec![conn, 2],
            vec![0; 7],
            vec![0; 8],
        ]
        .concat()
    };
    let dir = tempfile::tempdir().unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let path = rl9_record_with(
            RL9_RESULT,
            dir.path(),
            "result",
            backend,
            "result",
            &["Tasks", "Random", "Stdout"],
        );
        let streams = rl9_streams(&path, &format!("{backend:?}"));
        let root = &streams[0].1;
        let g = root
            .iter()
            .filter(|f| matches!(f, Frame::Grant { .. }))
            .count() as u8;
        let takes: Vec<_> = root
            .iter()
            .filter_map(|f| match f {
                Frame::Take { body, .. }
                    if matches!(body.arrival(), Arrival::Item(..) | Arrival::HelperEnd(..)) =>
                {
                    Some(body.bytes().to_vec())
                }
                _ => None,
            })
            .collect();
        let end = [
            vec![4, g + 3, 0],
            result(&[g + 4, g + 5], g + 1),
            vec![0],
        ]
        .concat();
        let item = [vec![0, g + 1, 8, 5], vec![0; 7], vec![0]].concat();
        assert_eq!(takes, [end, item], "{backend:?}: root takes");
        let ending = streams[1]
            .1
            .iter()
            .find_map(|f| match f {
                Frame::HelperEnd { ending, .. } => Some(ending.body().to_vec()),
                _ => None,
            })
            .expect("the helper's END");
        assert_eq!(ending, result(&[], g + 1), "{backend:?}: END");
        let draws: Vec<_> = root
            .iter()
            .filter_map(|f| match f {
                Frame::Op { op: o, object, .. } if *o == op::RANDOM => Some(*object),
                _ => None,
            })
            .collect();
        assert_eq!(draws, [u64::from(g + 4)], "{backend:?}: draw");
        let sends: Vec<_> = root
            .iter()
            .filter_map(|f| match f {
                Frame::Op { op: o, object, .. } | Frame::Issue { op: o, object, .. }
                    if *o == op::CONN_TRY_SEND =>
                {
                    Some(*object)
                }
                _ => None,
            })
            .collect();
        assert_eq!(sends, [u64::from(g + 5)], "{backend:?}: send");
    }
}

/// A `Shared` node whose payload is a `Shared` node crosses: the root sends
/// the outer node to itself.
const RL9_NESTED: &str = r#"
pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let inner: Shared[u64] = shared.new(42)
    let outer: Shared[Shared[u64]] = shared.new(take inner)
    let (s, si): (Client[Shared[Shared[u64]]], Inbox[Shared[Shared[u64]]]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.try_send(s, take outer))
    let kept = result.unwrap(conn.try_receive(si))
    result.unwrap(stdout.write(out, "nested"))
    Ok(())
}
"#;

/// A `Shared` node nested in a `Shared` payload crosses (recording.md,
/// "Values" and "Snapshots"), hand-derived before the first run. A holder's
/// inline layout is private (MEM-13), so the law reads the take's value after
/// its plain bytes: its part count and parts. `shared.new` numbers the inner
/// node g+1 and the outer g+2; the connection is g+3 and its client g+4. The
/// outer node arrives first, as g+5 (home stream 0, node g+2), its payload
/// the inner holder's word zeroed and one part, the inner node's own
/// snapshot, as g+6 (node g+1), whose payload is 42 with no parts.
#[test]
#[ignore = "explicit native integration target"]
fn rl9_nested_shared_crosses() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Arrival, Frame};

    let dir = tempfile::tempdir().unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let path = rl9_record_with(
            RL9_NESTED,
            dir.path(),
            "nested",
            backend,
            "nested",
            &["Tasks", "Stdout"],
        );
        let root = &rl8_root_frames(&path);
        let g = root
            .iter()
            .filter(|f| matches!(f, Frame::Grant { .. }))
            .count() as u8;
        let items: Vec<_> = root
            .iter()
            .filter_map(|f| match f {
                Frame::Take { body, .. } if matches!(body.arrival(), Arrival::Item(..)) => {
                    Some(body.bytes().to_vec())
                }
                _ => None,
            })
            .collect();
        assert_eq!(items.len(), 1, "{backend:?}: item takes {items:?}");
        let body = &items[0];
        assert_eq!(body[1], g + 3, "{backend:?}: the node's inbox");
        // Past the plain bytes (each length and ordinal here is below 128,
        // one varint byte).
        let parts = &body[3 + usize::from(body[2])..];
        let shared = [
            vec![1, 1, 1, g + 5, 0, 0, 0, 0, g + 2, 8],
            vec![0; 8],
            vec![1, 1, 1, g + 6, 0, 0, 0, 0, g + 1, 8, 42],
            vec![0; 7],
            vec![0],
        ]
        .concat();
        assert_eq!(parts, shared, "{backend:?}: node parts");
    }
}
