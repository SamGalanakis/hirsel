// Capabilities inside plain variants that cross units (docs/recording.md,
// "Values"), the survivors a helper's key objects leave (unit tapes U3), and
// `cross` variants that move by case (CONC-2, CONC-15).
// Included from recording_handles_native.rs, whose helpers it uses.

/// Capabilities inside plain variants cross: the root sends itself an
/// `Option[Random]` (`Some`, then `None`), a `List[Option[Random]]` of one,
/// and a niche-laid `Pick`, and draws through the first two.
const RL9_VARIANT: &str = r#"
type Pick = Off | On(Random, u32, char)

pub fn main(r: Random, out: Stdout) -> Result[(), Exit] waits {
    let (c, i): (Client[Option[Random]], Inbox[Option[Random]]) = result.unwrap(conn.new(1, bytes 0))
    let o: Option[Random] = Some(r)
    result.unwrap(conn.try_send(c, o))
    let got = option.unwrap(result.unwrap(conn.try_receive(i)))
    let mut b: Bytes = list.with_capacity(8)
    host.random(got, mut b)
    let none: Option[Random] = None
    result.unwrap(conn.try_send(c, none))
    let _ = result.unwrap(conn.try_receive(i))
    let (l, li): (Client[List[Option[Random]]], Inbox[List[Option[Random]]]) = result.unwrap(conn.new(1, bytes 64))
    let mut os: List[Option[Random]] = list.with_capacity(1)
    list.push(mut os, Some(r))
    result.unwrap(conn.try_send(l, take os))
    let mut back = result.unwrap(conn.try_receive(li))
    let held = option.unwrap(option.unwrap(list.pop(mut back)))
    host.random(held, mut b)
    let (p, pi): (Client[Pick], Inbox[Pick]) = result.unwrap(conn.new(1, bytes 0))
    let on: Pick = On(r, 7, 'a')
    result.unwrap(conn.try_send(p, on))
    let _ = result.unwrap(conn.try_receive(pi))
    result.unwrap(stdout.write(out, "variant"))
    Ok(())
}
"#;

/// A capability in a plain variant's active case crosses as its snapshot,
/// and padding crosses as zero (recording.md, "Values"),
/// hand-derived before the first run.
/// - `Option[Random]` has a direct 1-byte tag at 0 (`Some` is case 1) and the
///   word at 8: 16 bytes. Connection g+1 (client g+2) takes `Some` as 16
///   bytes with the word zeroed and one part, the tag 5 snapshot (kind 3, no
///   stored bytes) arriving as g+3; then `None`, tag 0 and no parts.
/// - Connection g+4 (client g+5) takes the list: a 24-byte carrier, all
///   zeroed, then the sequence part (one 16-byte element, `Some` with its
///   word zeroed) and the element's snapshot, g+6.
/// - `Pick` is laid out as `On`'s payload, `Random` at 0, `u32` at 8 and
///   `char` at 12, `Off` in the char's niche: 16 bytes with no padding.
///   Connection g+7 (client g+8) takes `On(r, 7, 'a')` as the zeroed word,
///   7 and 0x61, then one part, the snapshot g+9 (the `OTHER` guard: the tag
///   word is outside the niche).
/// - The draws are `RANDOM`s on g+3 and g+6.
///
/// Bytes 1..8 after each `Option`'s tag and the unused payload of `None`
/// must be zero. The list element's padding previously carried stack bytes.
#[test]
#[ignore = "explicit native integration target"]
fn rl9_variant_capabilities_cross() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Arrival, Frame, op};

    let dir = tempfile::tempdir().unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let path = rl9_record_with(
            RL9_VARIANT,
            dir.path(),
            "variant",
            backend,
            "variant",
            &["Random", "Stdout"],
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
        let some = [vec![1], vec![0; 15]].concat();
        let expected = [
            [vec![0, g + 1, 16], some.clone(), vec![1, 1, 5, g + 3, 3, 0]].concat(),
            [vec![0, g + 1, 16, 0], vec![0; 15], vec![0]].concat(),
            [
                vec![0, g + 4, 24],
                vec![0; 24],
                vec![2, 0, 16],
                some,
                vec![1, 5, g + 6, 3, 0],
            ]
            .concat(),
            [
                vec![0, g + 7, 16],
                vec![0; 8],
                vec![7, 0, 0, 0, 0x61, 0, 0, 0],
                vec![1, 1, 5, g + 9, 3, 0],
            ]
            .concat(),
        ];
        assert_eq!(items, expected, "{backend:?}: the items");
        let draws: Vec<_> = root
            .iter()
            .filter_map(|f| match f {
                Frame::Op { op: o, object, .. } if *o == op::RANDOM => Some(*object),
                _ => None,
            })
            .collect();
        assert_eq!(
            draws,
            [u64::from(g + 3), u64::from(g + 6)],
            "{backend:?}: draws"
        );
    }
}

/// A helper draws a key share from the run's host `Random` and returns it.
const RL9_KEY_SURVIVOR: &str = r#"
fn sharer(take r: Random) -> secret.KeyShare {
    let mut point: Bytes = list.with_capacity(32)
    result.unwrap(secret.key_share(r, X25519, mut point))
}

pub fn main(tasks: Tasks, r: Random, out: Stdout) -> Result[(), Exit] waits {
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, r, sharer))
    let kept = result.unwrap(task.join(take h))
    drop(take kept)
    result.unwrap(stdout.write(out, "kept"))
    Ok(())
}
"#;

/// A key object a helper paid for and its owner keeps is a survivor of the
/// owner's end take, named by the stream that made it and its number among
/// that unit's tracked key object and registry entry charges (recording.md,
/// "Unit stream format": kind 3), hand-derived before the first run. The
/// helper, stream 1, tracks one charge, the key share's 64 bytes (TLS-8:
/// `KeyShare` 64), so the root's end take lists kind 3, stream 1, ordinal
/// 0, amount 64, with no origin kind. On main a kept key object cuts the
/// root's end take ("a helper end keeps a key object ..."), so there is no
/// `HelperEnd` take.
#[test]
#[ignore = "explicit native integration target"]
fn rl9_helper_key_object_survives_by_name() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Arrival, Frame, Survivor};

    let dir = tempfile::tempdir().unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let path = rl9_record_with(
            RL9_KEY_SURVIVOR,
            dir.path(),
            "keykept",
            backend,
            "kept",
            &["Tasks", "Random", "Stdout"],
        );
        let streams = rl9_streams(&path, &format!("{backend:?}"));
        let ends: Vec<_> = streams[0]
            .1
            .iter()
            .filter_map(|f| match f {
                Frame::Take { body, .. } => match body.arrival() {
                    Arrival::HelperEnd(_, _, _, survivors) => Some(survivors),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        let share = Survivor {
            name: hirsel_tape_format::SurvivorName {
                kind: hirsel_tape_format::SurvivorKind::KeyObject,
                stream: 1,
                ordinal: 0,
            },
            amount: 64,
        };
        assert_eq!(
            ends,
            [vec![share]],
            "{backend:?}: the helper end's survivors"
        );
    }
}

const RL9_MEDIA: &str = r#"
fn send_keys(take input: (secret.KeyShare, Client[(schedule.Schedule, packet.Sealer, packet.Opener, packet.HeaderMask)])) -> secret.KeyShare {
    let (take host_share, take send) = take input
    let mut point: Bytes = list.with_capacity(32)
    let virtual_share = result.unwrap(secret.key_share(provide.random(7), X25519, mut point))
    let profile: schedule.Profile13 = Quic(Aes128GcmSha256)
    let hash = string.as_bytes("0123456789abcdef0123456789abcdef")
    let mut keys = result.unwrap(schedule.tls13(profile, Client, take host_share, hash, hash))
    let mut sealer = result.unwrap(schedule.sealer(mut keys, Handshake, 0, 7))
    let opener = result.unwrap(schedule.opener(keys, Handshake))
    let mask = result.unwrap(schedule.header_mask(keys, Handshake, true))
    let mut data: Bytes = list.with_capacity(16)
    result.unwrap(packet.seal(mut sealer, 7, mut data, 0, 0, string.as_bytes("")))
    result.unwrap(conn.try_send(send, (take keys, take sealer, take opener, take mask)))
    take virtual_share
}

pub fn main(tasks: Tasks, random: Random, out: Stdout) -> Result[(), Exit] waits {
    let mut point: Bytes = list.with_capacity(32)
    let older = result.unwrap(secret.key_share(provide.random(11), X25519, mut point))
    let mut public: Bytes = list.with_capacity(32)
    let host_share = result.unwrap(secret.key_share(random, X25519, mut public))
    let (c, i): (Client[(schedule.Schedule, packet.Sealer, packet.Opener, packet.HeaderMask)], Inbox[(schedule.Schedule, packet.Sealer, packet.Opener, packet.HeaderMask)]) = result.unwrap(conn.new(1, bytes 0))
    let worker = result.unwrap(task.spawn_unit(tasks, bytes 65536, (take host_share, take c), send_keys))
    let returned = result.unwrap(task.join(take worker))
    let (take arrived_keys, take arrived_sealer, take opener, take mask) = result.unwrap(conn.try_receive(i))
    let mut keys = take arrived_keys
    let mut sealer = take arrived_sealer
    let info = result.unwrap(schedule.inspect(keys, Handshake))
    if !info.send || !info.receive { return Error(Exit(status: 3)) }
    if let Error(Issued) = schedule.sealer(mut keys, Handshake, 0, 8) {} else {
        return Error(Exit(status: 4))
    }
    let mut data: Bytes = list.with_capacity(16)
    if let Error(Reused) = packet.seal(mut sealer, 7, mut data, 0, 0, string.as_bytes("")) {} else {
        return Error(Exit(status: 5))
    }
    result.unwrap(packet.seal(mut sealer, 8, mut data, 0, 0, string.as_bytes("")))
    let mut empty: Bytes = list.with_capacity(16)
    if let Error(Tampered) = packet.open(opener, 0, 8, mut empty, 0, 0, string.as_bytes("")) {} else {
        return Error(Exit(status: 6))
    }
    let _ = packet.mask(mask, string.as_bytes("0123456789abcdef"), 0)
    drop(take older)
    drop(take returned)
    result.unwrap(stdout.write(out, "media"))
    Ok(())
}
"#;

/// Host KeyShare entry, Schedule and packet objects in a sent item, and a
/// virtual KeyShare result. The older root share keeps the helper's agreement
/// entry live through its end. TLS-8 charges the helper in this order: share
/// 64, registry entry 48, schedule 512, domain 16, sealer 1024, opener 1024,
/// mask 512. All seven survive and keep their own kind and name.
#[test]
#[ignore = "explicit native integration target"]
fn rl9_key_objects_cross_as_snapshots() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Args, Arrival, Cursor, Frame, HelperOutcome, Survivor, op};

    let dir = tempfile::tempdir().unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let path = rl9_record_with(
            RL9_MEDIA,
            dir.path(),
            "media_cross",
            backend,
            "media",
            &["Tasks", "Random", "Stdout"],
        );
        let streams = rl9_streams(&path, &format!("{backend:?}"));
        let program = path.with_extension("");
        for unit in [None, Some("root"), Some("root.0")] {
            let (code, out, err) = u4_replay(&program, &path, unit);
            assert_eq!(
                (code, out.as_str(), err.as_str()),
                (Some(0), "", ""),
                "{backend:?} {unit:?}"
            );
        }
        let root = &streams[0].1;
        let helper = &streams[1].1;
        let grant_count = root
            .iter()
            .filter(|f| matches!(f, Frame::Grant { .. }))
            .count() as u64;
        let [key_origin] = root
            .iter()
            .filter_map(|f| match f {
                Frame::Op {
                    op: op::MEDIA_KEY_SHARE,
                    out,
                    ..
                } => Some(out.value),
                _ => None,
            })
            .collect::<Vec<_>>()[..]
        else {
            panic!("{backend:?}: expected one host key share delivery");
        };
        let stream = root
            .iter()
            .find_map(|f| match f {
                Frame::Op {
                    op: op::UNIT_SPAWN,
                    out,
                    ..
                } => Some(out.value as u32),
                _ => None,
            })
            .unwrap();
        assert_eq!(stream, 1, "{backend:?}: the first helper");

        let entry = streams[1].0.as_ref().unwrap().body();
        let mut input = Cursor::new(entry.value.bytes());
        assert_eq!(input.bytes().unwrap(), [0; 16], "{backend:?}: entry words");
        assert_eq!(input.varint().unwrap(), 2);
        assert_eq!(input.take(3).unwrap(), [1, 8, 1]);
        assert_eq!(input.u32().unwrap(), 0);
        assert_eq!(input.varint().unwrap(), key_origin);
        assert_eq!(input.u8().unwrap(), 0); // X25519

        let [sealer_at, opener_at, mask_at] = helper
            .iter()
            .filter_map(|f| match f {
                Frame::Op {
                    op: op::MEDIA_CAPTURE,
                    out,
                    ..
                } if out.payload.is_empty() => Some(out.value),
                _ => None,
            })
            .collect::<Vec<_>>()[..]
        else {
            panic!("{backend:?}: expected three packet handle deliveries");
        };
        let mut items = root.iter().enumerate().filter_map(|(pos, f)| match f {
            Frame::Take { body, .. } => match body.arrival() {
                Arrival::Item(_, value) => Some((pos + 1, value)),
                _ => None,
            },
            _ => None,
        });
        let (take_at, item) = items.next().unwrap();
        assert!(items.next().is_none());
        let mut bytes = Cursor::new(item.bytes());
        let tape = std::fs::read(&path).unwrap();
        let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
            std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
        })
        .unwrap()
        .1
        .tape_id;
        assert_eq!(bytes.bytes().unwrap(), [0; 32], "{backend:?}: item words");
        assert_eq!(bytes.varint().unwrap(), 4);
        for (index, tag, at) in [
            (0, 9, None),
            (1, 10, Some(sealer_at)),
            (2, 11, Some(opener_at)),
            (3, 12, Some(mask_at)),
        ] {
            assert_eq!(bytes.take(2).unwrap(), [1, tag]);
            let ordinal_at = bytes.at;
            assert_eq!(bytes.varint().unwrap(), grant_count + 4 + index);
            assert_eq!(bytes.u32().unwrap(), stream);
            let position = bytes.varint().unwrap();
            match at {
                Some(at) => assert_eq!(position, at),
                None => assert!((1..sealer_at).contains(&position)),
            }
            assert_eq!(bytes.take(2).unwrap(), [0, 0]); // QUIC, AES-128-GCM
            match tag {
                9 => {
                    assert_eq!(bytes.take(20).unwrap(), [0; 20]); // Client, handshake, no updates/retire/export
                    assert_eq!(bytes.varint().unwrap(), 1); // the sealer's domain
                    assert_eq!(bytes.u8().unwrap(), 1); // Handshake
                    assert_eq!(bytes.u64().unwrap(), 0);
                }
                10 => {
                    assert_eq!(bytes.u8().unwrap(), 1);
                    assert_eq!(bytes.u64().unwrap(), 0);
                    assert_eq!(bytes.u64().unwrap(), 7);
                    assert_eq!(bytes.u8().unwrap(), 1);
                    assert_eq!(bytes.u64().unwrap(), 7);
                }
                11 => assert_eq!(bytes.u8().unwrap(), 1),
                12 => assert_eq!(bytes.take(2).unwrap(), [1, 1]), // Handshake, local
                _ => unreachable!(),
            }
            // Keep the block valid and corrupt this snapshot's receiving ordinal.
            let wrong = u4_patched(&tape, &tape_id, 0, |payload| {
                let value = item.bytes();
                let Some(at) = payload.windows(value.len()).position(|w| w == value) else {
                    return false;
                };
                payload[at + ordinal_at] = 99;
                true
            });
            let corrupt = dir.path().join(format!("media-{backend:?}-tag-{tag}.tape"));
            std::fs::write(&corrupt, wrong).unwrap();
            let (code, out, err) = u4_replay(&program, &corrupt, Some("root"));
            assert_eq!((code, out.as_str()), (Some(1), ""), "{backend:?} tag {tag}: {err}");
            let expected = format!("replay: Diverged(Take) at {take_at} in root ");
            assert!(
                matches!(err.lines().collect::<Vec<_>>().as_slice(), [line] if line.starts_with(&expected)),
                "{backend:?} tag {tag}: {err}"
            );
        }
        assert!(bytes.done());
        let args = Args::of_parts(&[&item.canonical()]);
        assert!(
            helper.iter().any(|f| matches!(
                f, Frame::Op { op: op::CONN_TRY_SEND, args: got, .. } if *got == args
            )),
            "{backend:?}: sender and receiver agree"
        );
        // MASK: opcode, zero requested spare, and lengths of the reference,
        // offset and sample. The reference is the mask's root ordinal and 3.
        let mask_operand = [(grant_count + 7).to_le_bytes().as_slice(), &[3]].concat();
        let mask_args = Args::of_parts(&[
            &[10, 0, 9, 8, 16],
            &mask_operand,
            &[0; 8],
            b"0123456789abcdef",
        ])
        .buffered([5, 0]);
        assert!(
            root.iter().any(|f| matches!(
                f, Frame::Op { op: op::MEDIA_CAPTURE, object: 0, args, out, .. }
                    if *args == mask_args && out.payload.len() == 5
            )),
            "{backend:?}: the mask operates under its arriving ordinal"
        );

        let (value, survivors) = root
            .iter()
            .find_map(|f| match f {
                Frame::Take { body, .. } => match body.arrival() {
                    Arrival::HelperEnd(_, HelperOutcome::Returned(value), _, survivors) => {
                        Some((value, survivors))
                    }
                    _ => None,
                },
                _ => None,
            })
            .unwrap();
        let mut expected = [vec![8], vec![0; 8], vec![1, 1, 8]].concat();
        hirsel_tape_format::put_varint(&mut expected, grant_count + 8);
        expected.extend_from_slice(&stream.to_le_bytes());
        expected.extend([0, 0, 0]); // no delivery, kind 3 charge ordinal 0, X25519
        assert_eq!(value.bytes(), expected, "{backend:?}: virtual key");
        let survivor = |kind, ordinal, amount| Survivor {
            name: hirsel_tape_format::SurvivorName {
                kind,
                stream,
                ordinal,
            },
            amount,
        };
        use hirsel_tape_format::SurvivorKind::{Agreement, KeyObject};
        assert_eq!(
            survivors,
            [
                survivor(KeyObject, 0, 64),
                survivor(KeyObject, 2, 512),
                survivor(KeyObject, 3, 16),
                survivor(KeyObject, 4, 1024),
                survivor(KeyObject, 5, 1024),
                survivor(KeyObject, 6, 512),
                survivor(Agreement, 1, 48),
            ],
            "{backend:?}: survivor names and amounts"
        );
    }
}

/// A `cross` variant whose cases carry a `String` and `Client` reply arms
/// (CONC-2, CONC-15): it crosses as a spawn seed, as a helper's result and as
/// a connection item, each case moving only its own fields.
const RL9_VARIANTS: &str = r#"
type Req: cross = Read(u64, String, Client[String]) | Stats(Client[u64])
type Reply: cross = Text(u64, String) | Count(u64)

fn answer(take r: Req) -> Reply waits {
    match take r {
        Read(n, take s, take back) => {
            result.unwrap(conn.send(back, string.from(s)))
            Text(n, take s)
        }
        Stats(take back) => {
            result.unwrap(conn.send(back, 7))
            Count(7)
        }
    }
}

fn score(take r: Reply, want: String) -> u64 {
    match take r {
        Text(n, take s) => if s == want { n } else { 1000 },
        Count(k) => k
    }
}

fn serve(take hello: Client[Client[Req]]) -> u64 waits {
    let (c, i): (Client[Req], Inbox[Req]) = result.unwrap(conn.new(4, bytes 256))
    result.unwrap(conn.send(hello, take c))
    let a = answer(result.unwrap(conn.receive(i)))
    let b = answer(result.unwrap(conn.receive(i)))
    score(take a, "conn-read") + score(take b, "conn-read")
}

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let (sc, si): (Client[String], Inbox[String]) = result.unwrap(conn.new(2, bytes 256))
    let (nc, ni): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(2, bytes 0))
    let h1 = result.unwrap(task.spawn_unit(tasks, bytes 65536, Read(3, string.from("seed-read"), take sc), answer))
    if result.unwrap(conn.receive(si)) != "seed-read" { return Error(Exit(status: 3)) }
    if score(result.unwrap(task.join(take h1)), "seed-read") != 3 { return Error(Exit(status: 4)) }
    let h2 = result.unwrap(task.spawn_unit(tasks, bytes 65536, Stats(take nc), answer))
    if result.unwrap(conn.receive(ni)) != 7 { return Error(Exit(status: 5)) }
    if score(result.unwrap(task.join(take h2)), "seed-read") != 7 { return Error(Exit(status: 6)) }

    let (hello, hello_in): (Client[Client[Req]], Inbox[Client[Req]]) = result.unwrap(conn.new(1, bytes 0))
    let h3 = result.unwrap(task.spawn_unit(tasks, bytes 65536, take hello, serve))
    let req = result.unwrap(conn.receive(hello_in))
    let (sc2, si2): (Client[String], Inbox[String]) = result.unwrap(conn.new(2, bytes 256))
    let (nc2, ni2): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(2, bytes 0))
    let (left, _left_in): (Client[String], Inbox[String]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.send(req, Read(5, string.from("conn-read"), take sc2)))
    result.unwrap(conn.send(req, Stats(take nc2)))
    // Never received: the helper's inbox drops this item's String and Client.
    result.unwrap(conn.send(req, Read(9, string.from("never-read"), take left)))
    if result.unwrap(conn.receive(si2)) != "conn-read" { return Error(Exit(status: 7)) }
    if result.unwrap(conn.receive(ni2)) != 7 { return Error(Exit(status: 8)) }
    if result.unwrap(task.join(take h3)) != 12 { return Error(Exit(status: 9)) }
    result.unwrap(stdout.write(out, "variants"))
    Ok(())
}
"#;

/// The oracle is the program's own literals: each reply and each joined
/// result names the `String` the parent sent in that case, so a crossing
/// that moved another case's words, or none of the active case's, prints
/// nothing or exits with the status of the check it broke. (A units
/// recording does not replay yet: unit operations are not replayed, units.rs.)
#[test]
#[ignore = "explicit native integration target"]
fn rl9_variants_cross_by_case() {
    crate::openat2_required::require();
    let dir = tempfile::tempdir().unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        rl9_record(RL9_VARIANTS, dir.path(), "variants", backend, "variants");
    }
}
