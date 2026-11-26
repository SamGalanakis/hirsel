// Capabilities inside plain variants that cross units (docs/recording.md,
// "Values"), and the survivors a helper's key objects leave (unit tapes U3).
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
/// and one in an inactive case is plain bytes (recording.md, "Values"),
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
/// Bytes 1..8 after each `Option`'s tag are padding, and the whole of a
/// `None`'s payload is plain: neither is written by the program, so the law
/// masks them. On main a variant lists no owners: the words cross raw with no
/// parts.
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
        let mut items: Vec<_> = root
            .iter()
            .filter_map(|f| match f {
                Frame::Take { body, .. } if matches!(body.arrival(), Arrival::Item(..)) => {
                    Some(body.bytes().to_vec())
                }
                _ => None,
            })
            .collect();
        // Padding and a `None`'s payload: [item, from, to).
        for (item, from, to) in [(0, 4, 11), (1, 4, 19), (2, 31, 38)] {
            if let Some(bytes) = items.get_mut(item).and_then(|i| i.get_mut(from..to)) {
                bytes.fill(0);
            }
        }
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
                    Arrival::HelperEnd(_, _, survivors) => Some(survivors),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        let share = Survivor {
            kind: 3,
            origin_kind: None,
            stream: 1,
            ordinal: 0,
            amount: 64,
        };
        assert_eq!(ends, [vec![share]], "{backend:?}: the helper end's survivors");
    }
}
