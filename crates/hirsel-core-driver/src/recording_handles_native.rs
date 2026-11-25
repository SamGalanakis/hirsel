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
