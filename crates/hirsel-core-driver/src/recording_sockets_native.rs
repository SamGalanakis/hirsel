// TCP sockets that cross units (docs/recording.md, "Sockets that cross",
// unit tapes U3). Included from recording_handles_native.rs, whose helpers
// it uses.

/// The root accepts a TCP socket, sends it into a connection whose inbox it
/// has dropped, which gives it back, sends it to itself, and moves the
/// socket it takes into a helper's entry; the helper reads what the root's
/// connected socket wrote.
const RL9_SOCKET: &str = r#"
fn reader(take s: Socket) -> u64 waits {
    let (read, write) = net.split(take s)
    let mut r = take read
    let _ = take write
    let mut b: Bytes = list.with_capacity(4)
    result.unwrap(host.recv_bounded(mut r, mut b, 4, Never))
}

pub fn main(n: Net, tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let (listening, addr) = result.unwrap(host.listen(n, Addr(ip: V4(2130706433), port: 0), 1))
    let mut listener = take listening
    let client = result.unwrap(host.connect(n, addr, Never))
    let (server, _peer) = result.unwrap(host.accept(mut listener, Never))
    let (cr, cw) = net.split(take client)
    let mut w = take cw
    let _ = result.unwrap(host.send_pair(mut w, string.as_bytes("hi"), string.as_bytes(""), Never))
    let (gone, dropped): (Client[Socket], Inbox[Socket]) = result.unwrap(conn.new(1, bytes 0))
    let _ = take dropped
    let back = match conn.try_send(gone, take server) {
        Error(Closed(take s)) => take s,
        _ => return Error(Exit(status: 2)),
    }
    let (c, i): (Client[Socket], Inbox[Socket]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.try_send(c, take back))
    let taken = result.unwrap(conn.try_receive(i))
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, take taken, reader))
    if result.unwrap(task.join(take h)) != 2 { return Error(Exit(status: 3)) }
    let _ = take cr
    let _ = take w
    result.unwrap(stdout.write(out, "sock"))
    Ok(())
}
"#;

/// An accepted TCP socket crosses units (recording.md, "Sockets that
/// cross"), hand-derived before the first run. The run's Net is granted
/// `Listen 127.0.0.0/8 0-65535` and `Connect 127.0.0.0/8 1-65535`.
/// - A socket's snapshot is tag 2: direction (ACCEPT 1), the peer as the
///   ACCEPT delivered it (19 bytes: family 0 for IPv4, `hi` 0 and `lo`
///   127.0.0.1 as `u64`s, the port), TLS 0 and state 0 (no receive on it
///   yet). An item or entry holding one is 8 plain bytes (the zeroed word)
///   and one handle part.
/// - After g GRANT frames the root's LISTEN makes g+1, its CONNECT g+2 and
///   its ACCEPT g+3. The first connection takes g+4 and g+5; the socket it
///   refuses comes back as g+3, so the root's stream names it without a
///   cut. The second connection takes g+6 and g+7, and the socket arrives in
///   the take as g+8. The helper handle is g+9.
/// - The helper's entry holds the socket as its ordinal 1, and its receive
///   on 1 delivers "hi" from the peer the snapshot names: the reads a replay
///   of the helper's stream gives. Its result is 2 and its end take keeps no
///   survivor: a TCP socket holds no charge.
///
/// Red on main: a recording unit's socket stopped the run (`UnitRecording`).
#[test]
#[ignore = "explicit native integration target"]
fn rl9_accepted_socket_crosses() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Args, Arrival, Frame, Out, Value, op};

    // Each op's delivered outcome in a stream, in order: an OP's, or the
    // DONE of the ISSUE on its task.
    let outs = |frames: &[Frame], want: u8| -> Vec<(u64, Out)> {
        let mut issued = std::collections::HashMap::new();
        let mut found = Vec::new();
        for f in frames {
            match f {
                Frame::Op { op: o, object, out, .. } if *o == want => {
                    found.push((*object, out.clone()));
                }
                Frame::Issue { task, op: o, object, .. } => {
                    issued.insert(*task, (*o, *object));
                }
                Frame::Done { task, out, .. } => {
                    if let Some((o, object)) = issued.remove(task)
                        && o == want
                    {
                        found.push((object, out.clone()));
                    }
                }
                _ => {}
            }
        }
        found
    };
    let socket = |peer: &[u8], ordinal: u8| -> Vec<u8> {
        [vec![8], vec![0; 8], vec![1, 1, 2, ordinal, 1], peer.to_vec(), vec![0, 0]].concat()
    };
    let dir = tempfile::tempdir().unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let path = rl9_record_net(
            RL9_SOCKET,
            dir.path(),
            "socket",
            backend,
            "sock",
            &["Net", "Tasks", "Stdout"],
            &["Listen 127.0.0.0/8 0-65535", "Connect 127.0.0.0/8 1-65535"],
        );
        let streams = rl9_streams(&path, &format!("{backend:?}"));
        let root = &streams[0].1;
        let g = root
            .iter()
            .filter(|f| matches!(f, Frame::Grant { .. }))
            .count() as u8;
        let accepts = outs(root, op::ACCEPT);
        assert_eq!(accepts.len(), 1, "{backend:?}: one accept");
        let peer = accepts[0].1.payload.clone();
        let mut loopback = vec![0];
        loopback.extend_from_slice(&0u64.to_le_bytes());
        loopback.extend_from_slice(&0x7f00_0001u64.to_le_bytes());
        assert_eq!(peer[..17], loopback[..], "{backend:?}: the peer is 127.0.0.1");
        let canonical = Value::decode(&socket(&peer, 0)).unwrap().canonical();
        let sends: Vec<_> = root
            .iter()
            .filter_map(|f| match f {
                Frame::Op { op: o, object, args, out, .. } if *o == op::CONN_TRY_SEND => {
                    Some((*object, args.clone(), out.value))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            sends,
            [
                (u64::from(g + 5), Args::of_parts(&[&canonical]), 2),
                (u64::from(g + 7), Args::of_parts(&[&canonical]), 0),
            ],
            "{backend:?}: sends, the first refused Closed"
        );
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
        assert_eq!(
            takes,
            [
                [vec![0, g + 6], socket(&peer, g + 8)].concat(),
                [vec![4, g + 9, 0, 8, 2], vec![0; 7], vec![0, 0, 0]].concat(),
            ],
            "{backend:?}: the item take names g+8; the end take keeps no survivor"
        );
        let entry = streams[1].0.as_ref().expect("a helper entry");
        assert_eq!(
            entry.body().value.bytes(),
            socket(&peer, 1).as_slice(),
            "{backend:?}: entry"
        );
        let reads: Vec<_> = outs(&streams[1].1, op::RECV_BOUNDED)
            .into_iter()
            .map(|(object, out)| (object, out.payload))
            .collect();
        assert_eq!(reads, [(1, b"hi".to_vec())], "{backend:?}: the helper's read");
    }
}
