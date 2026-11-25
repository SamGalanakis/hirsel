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
