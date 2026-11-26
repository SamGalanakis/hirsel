use hirsel_tape_format::{
    Arrival, Cause, CauseDetail, Corrupt, Credit, Decoder, Encoder, Frame, HelperOutcome, Index,
    ReadError, Settlement, Stop, TakePoint, Value, block, xxh3,
};
use std::convert::Infallible;

const ID: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

fn root_header() -> Vec<u8> {
    // The published example, including the published check.
    [
        b"\x00HTB\0\0\0\0\0\0\0\0\0\0\0\0\x4b\0\0\0\x0f\0\x01".as_slice(),
        &ID,
        &[0; 37],
        b"\0\0\x01\0\0\0\0\0",
        &[0; 11],
        b"\x70\x23\x13\xbf\xd2\xdf\x26\xd6",
    ]
    .concat()
}

const EXAMPLE_END: &[u8] = b"\x02HTB\0\0\0\0\x01\0\0\0\0\0\0\0\x19\0\0\0\x0e\0\x01\0\x0a\x0a\x09\0\x30\0\0\0\0\0\0\0\x0a\0\0\0\x04\0\0\0\0\x91\x50\x4f\xbc\x0f\x1f\x01\x54";
const ROOT_END: &[u8] = b"\x0a\0\0\0\x04\0\0\0\0";

fn read(tape: &[u8]) -> impl Fn(u64, &mut [u8]) -> Result<usize, Infallible> + '_ {
    |offset, output| {
        let start = offset as usize;
        let n = output.len().min(tape.len().saturating_sub(start));
        if n != 0 {
            output[..n].copy_from_slice(&tape[start..start + n]);
        }
        Ok(n)
    }
}

fn sealed(mut frame: Vec<u8>) -> Vec<u8> {
    let check = xxh3(&[&ID, &frame]);
    frame.extend(check.to_le_bytes());
    frame
}

fn wire(stream: u32, seq: u64, flag: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![flag, b'H', b'T', b'B'];
    frame.extend(stream.to_le_bytes());
    frame.extend(seq.to_le_bytes());
    frame.extend((payload.len() as u32).to_le_bytes());
    frame.extend(payload);
    sealed(frame)
}

fn varint(mut n: usize) -> Vec<u8> {
    let mut output = Vec::new();
    while n >= 128 {
        output.push((n & 127) as u8 | 128);
        n >>= 7;
    }
    output.push(n as u8);
    output
}

fn continued(stream: u32, first_seq: u64, chunks: &[&[u8]], last_flag: u8) -> Vec<Vec<u8>> {
    chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let mut data = vec![9];
            data.extend(varint(chunk.len()));
            data.extend(*chunk);
            wire(
                stream,
                first_seq + i as u64,
                if i + 1 == chunks.len() { last_flag } else { 0 },
                &data,
            )
        })
        .collect()
}

fn helper(id: u32, parent: u32, spawn: u64) -> Vec<u8> {
    // 13 entry bytes. Descriptor [1], n = 0, absent bound, empty value.
    let mut payload = parent.to_le_bytes().to_vec();
    payload.extend(spawn.to_le_bytes());
    payload.extend([13, 13, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    wire(id, 0, 0, &payload)
}

fn assert_damaged(blocks: &[Vec<u8>], block: u64) {
    let source = [root_header(), blocks.concat()].concat();
    assert!(
        matches!(Index::open(read(&source)), Err(ReadError::Format(Stop::Damaged(n))) if n == block),
        "expected damaged block {block}"
    );
}

fn index_cases() {
    let choose = wire(0, 1, 0, &[6, 0, 0]);
    let last = wire(0, 2, 2, ROOT_END);
    let end = wire(0, 1, 2, ROOT_END);
    let mut moved = wire(3, 1, 0, &[6, 0, 0]);
    moved[4] = 4;
    let mut flipped = choose.clone();
    flipped[21] ^= 1;
    let mut bad_magic = choose.clone();
    bad_magic[1] ^= 1;
    let mut too_long = vec![0, b'H', b'T', b'B', 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0];
    too_long.extend(1_048_577u32.to_le_bytes());
    assert_damaged(&[last.clone()], 1);
    assert_damaged(&[last.clone(), choose.clone()], 1);
    assert_damaged(&[choose.clone(), choose.clone(), last.clone()], 2);
    assert_damaged(&[helper(3, 0, 0), helper(3, 0, 1)], 2);
    assert_damaged(&[root_header()], 1);
    assert_damaged(&[end.clone(), wire(0, 2, 0, &[6, 0, 0])], 2);
    assert_damaged(&[wire(0, 1, 4, &[]), last], 2);
    assert_damaged(&[wire(3, 1, 0, &[])], 1);
    assert_damaged(&[helper(3, 3, 0)], 1);
    assert_damaged(&[helper(3, 0, 0), helper(4, 0, 0)], 2);
    assert_damaged(&[helper(4, 3, 0), helper(3, 4, 0)], 2);
    assert_damaged(&[helper(2, 4, 1), helper(4, 3, 0), helper(3, 4, 0)], 3);
    assert_damaged(&[wire(3, 0, 0, &[0; 11])], 1);

    let orphan = [
        root_header(),
        helper(3, 9, 0),
        helper(1, 7, 0),
        helper(8, 3, 2),
    ]
    .concat();
    let (index, _) = Index::open(read(&orphan)).unwrap();
    assert_eq!(
        index.paths().collect::<Vec<_>>(),
        [
            (0, "root".into()),
            (1, "?.1".into()),
            (3, "?.3".into()),
            (8, "?.3.2".into()),
        ]
    );

    let invalid_carrier = [root_header(), wire(3, 0, 0, &[0; 12])].concat();
    let (mut index, _) = Index::open(read(&invalid_carrier)).unwrap();
    assert!(matches!(
        index.take(3).unwrap().decoder(read(&invalid_carrier)),
        Err(ReadError::Format(Stop::Corrupt))
    ));
    assert_damaged(&[helper(3, 0, 0), helper(4, 0, 1), moved], 3);
    assert_damaged(&[flipped, wire(0, 2, 2, ROOT_END)], 1);
    assert_damaged(&[bad_magic], 1);
    assert_damaged(&[too_long], 1);
    assert_damaged(&[wire(0, 1, 8, &[])], 1);
    assert_damaged(&[wire(0, 1, 6, &[])], 1);
    assert_damaged(&[wire(0, 1, 4, &[0])], 1);
    assert_damaged(&[end.clone(), vec![0]], 2);
    // No stream opens after the root's END.
    assert_damaged(&[end, helper(3, 0, 0)], 2);

    let source = [
        root_header(),
        helper(8, 2, 0),
        helper(3, 0, 10),
        helper(2, 0, 2),
        vec![0],
    ]
    .concat();
    let (mut index, _) = Index::open(read(&source)).unwrap();
    assert_eq!(
        index.paths().collect::<Vec<_>>(),
        [
            (0, "root".into()),
            (2, "root.2".into()),
            (8, "root.2.0".into()),
            (3, "root.10".into()),
        ]
    );
    assert!(matches!(
        index.take(2).unwrap().next(read(&source)),
        Err(ReadError::Format(Stop::Truncated))
    ));
}

fn take(point: u8, body: &[u8]) -> Vec<u8> {
    let mut frame = vec![14, 0, point, 0, body.len() as u8, body.len() as u8];
    frame.extend(body);
    frame
}

fn take_cases() {
    for (bytes, expected) in [
        (&[1, 4][..], Arrival::Close(4)),
        (&[2, 5], Arrival::SealAnswer(5)),
        (
            &[
                6, 3, 0, 0, 0, 4, 8, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0,
            ],
            Arrival::LastRelease(3, 4, 8, 9),
        ),
        (&[7, 5, 0, 0, 0, 6], Arrival::SharedRelease(5, 6)),
        (
            // Helper 3 Failed: a Fail at site 5 saying "no", then a Capacity at
            // site 6 asking 64 with 16 available; no survivors.
            &[
                4, 3, 1, 2, 4, 5, 0, 0, 0, 0, 0, 0, 0, 2, b'n', b'o', 6, 6, 0, 0, 0, 0, 0, 0, 0,
                64, 0, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
            Arrival::HelperEnd(
                3,
                HelperOutcome::Failed(vec![
                    Cause {
                        kind: 4,
                        site: 5,
                        detail: CauseDetail::Fail(b"no"),
                    },
                    Cause {
                        kind: 6,
                        site: 6,
                        detail: CauseDetail::Capacity {
                            requested: 64,
                            available: 16,
                        },
                    },
                ]),
                vec![],
            ),
        ),
        (&[8], Arrival::Cancel),
        (
            &[9, 2, 48, 0, 0, 0, 0, 0, 0, 0],
            Arrival::Refund(Credit {
                limit: 2,
                amount: 48,
            }),
        ),
    ] {
        let bytes = take(0, bytes);
        let decoded = Decoder::default().block(&bytes, false).unwrap();
        let [
            Frame::Take {
                point: TakePoint::Operation,
                body,
                ..
            },
        ] = decoded.as_slice()
        else {
            panic!("{decoded:?}");
        };
        assert_eq!(body.arrival(), expected);
        assert_eq!(Encoder::default().frame(&decoded[0]), (bytes, vec![]));
    }
    let mut decoder = Decoder::default();
    let item = take(1, &[0, 7, 1, 0x42, 0]);
    let decoded = decoder.block(&item, false).unwrap();
    let [
        Frame::Take {
            point: TakePoint::Pump,
            body,
            ..
        },
    ] = decoded.as_slice()
    else {
        panic!("{decoded:?}")
    };
    let Arrival::Item(7, val) = body.arrival() else {
        panic!("{body:?}")
    };
    assert_eq!(val.bytes(), &[1, 0x42, 0]);

    let issue = [0x11, 7, 165, 2, 0];
    let returned = [1, 0x42, 0];
    for (settled, expected) in [
        (&[3, 1, 2][..], Settlement::Withdrawn),
        (&[3, 1, 0][..], Settlement::Accepted),
        (
            &[3, 1, 1, 1, 0x42, 0],
            Settlement::Closed(Value::decode(&returned).unwrap()),
        ),
    ] {
        let bytes = [issue.as_slice(), &take(1, settled), &issue].concat();
        let decoded = Decoder::default().block(&bytes, false).unwrap();
        let [
            Frame::Issue { .. },
            Frame::Take { body, .. },
            Frame::Issue { .. },
        ] = decoded.as_slice()
        else {
            panic!("{decoded:?}")
        };
        assert_eq!(body.arrival(), Arrival::Settlement(1, expected));
    }
    assert_eq!(
        Decoder::default().block(&take(1, &[3, 1, 2]), false),
        Err(Corrupt)
    );
    // An `Accepted` settlement carries nothing after its outcome.
    assert_eq!(
        Decoder::default().block(&[issue.as_slice(), &take(1, &[3, 1, 0, 0])].concat(), false),
        Err(Corrupt)
    );

    for body in [
        &b"\x0a"[..],
        // Tag 5 (credits) was removed: no unit takes one.
        b"\x05\0",
        b"\0\x07\0\x01\x01\x0d\0",
        b"\x04\0\0\0\0\x01\x08\0\0\0\0\0\0\0\0\0",
        // Cause kind 8 is past OutOfTime.
        b"\x04\x03\x01\x01\x08\0\0\0\0\0\0\0\0\0",
    ] {
        assert_eq!(
            Decoder::default().block(&take(0, body), false),
            Err(Corrupt)
        );
    }
    assert_eq!(
        Decoder::default().block(&take(2, &[8]), false),
        Err(Corrupt)
    );
    let mut listener = vec![0, 7, 0, 1, 1, 3, 1, 1];
    listener.extend([0; 19]);
    // A malformed direction inside a continued value corrupts just its stream.
    let first = [
        vec![14, 0, 0, 0, listener.len() as u8, 7],
        listener[..7].to_vec(),
    ]
    .concat();
    let mut continuation = vec![9, 1, listener[7]];
    let mut decoder = Decoder::default();
    assert_eq!(decoder.block(&first, false), Ok(vec![]));
    assert_eq!(decoder.block(&continuation, false), Ok(vec![]));
    continuation = [vec![9, 19], listener[8..].to_vec()].concat();
    assert_eq!(decoder.block(&continuation, false), Err(Corrupt));

    // Each survivor names its stream before its ordinal, so one kept from a
    // grandchild (stream 5) differs from the helper's own ordinal 4.
    let mut survivor = vec![4, 3, 0, 0, 0, 3];
    for (kind, origin, stream) in [(2, Some(1), 3u32), (7, Some(2), 3), (1, None, 5)] {
        survivor.push(kind);
        survivor.extend(origin);
        survivor.extend(stream.to_le_bytes());
        survivor.push(4);
        survivor.extend(48u64.to_le_bytes());
    }
    let decoded = Decoder::default()
        .block(&take(0, &survivor), false)
        .unwrap();
    let Frame::Take { body, .. } = &decoded[0] else {
        panic!("{decoded:?}")
    };
    let Arrival::HelperEnd(3, HelperOutcome::Returned(value), survivors) = body.arrival() else {
        panic!("{body:?}");
    };
    assert_eq!(value.bytes(), &[0, 0]);
    assert_eq!(
        survivors
            .iter()
            .map(|entry| (
                entry.kind,
                entry.origin_kind,
                entry.stream,
                entry.ordinal,
                entry.amount
            ))
            .collect::<Vec<_>>(),
        [
            (2, Some(1), 3, 4, 48),
            (7, Some(2), 3, 4, 48),
            (1, None, 5, 4, 48)
        ]
    );
    survivor[7] = 6;
    assert_eq!(
        Decoder::default().block(&take(0, &survivor), false),
        Err(Corrupt)
    );
}

fn unit_ops() {
    for (op, object, args, value) in [
        (164, 0, &[1][..], 3),
        (165, 2, &[2], 1),
        (166, 3, &[3], 2),
        (167, 4, &[], 0),
    ] {
        let mut wire = vec![0x13, 0, op, object, args.len() as u8];
        wire.extend(args);
        wire.extend([0, value]);
        let frames = Decoder::default().block(&wire, false).unwrap();
        let [
            Frame::Op {
                op: got,
                object: ordinal,
                out,
                ..
            },
        ] = frames.as_slice()
        else {
            panic!("{frames:?}")
        };
        assert_eq!(
            (*got, *ordinal, out.status, out.value),
            (op, u64::from(object), 0, u64::from(value))
        );
        assert_eq!(Encoder::default().frame(&frames[0]).0, wire);
        let status = wire.len() - 2;
        wire[status] = 2;
        assert_eq!(Decoder::default().block(&wire, false), Err(Corrupt));
    }

    for wire in [
        [0x13, 0, 164, 1, 0, 0, 3],
        [0x13, 0, 164, 0, 0, 0, 0],
        [0x13, 0, 165, 1, 0, 0, 2],
        [0x13, 0, 166, 1, 0, 0, 3],
        [0x13, 0, 167, 1, 0, 0, 1],
    ] {
        assert_eq!(Decoder::default().block(&wire, false), Err(Corrupt));
    }
}

fn port_delivery() {
    let issue = [0x11, 7, 59, 1, 0];
    // Call 42: written 1, then the nine call words 1..=9 lead the payload.
    let words: Vec<u8> = (1..=9u64).flat_map(u64::to_le_bytes).collect();
    let mut first = vec![2, 0, 0, 42, 1];
    first.extend(varint(65_539));
    first.extend(varint(65_536));
    first.extend(&words);
    first.extend(vec![0x5a; 65_536 - 72]);
    let data = [9, 3, 6, 7, 8];
    let tape = [
        root_header(),
        wire(0, 1, 0, &[issue.as_slice(), &first].concat()),
        wire(0, 2, 0, &data),
        wire(0, 3, 2, ROOT_END),
    ]
    .concat();
    let (mut blocks, _) = Index::root(read(&tape)).unwrap();
    let mut decoder = blocks.decoder(read(&tape)).unwrap();
    let mut encoder = Encoder::default();
    let (_, payload) = blocks.next(read(&tape)).unwrap().unwrap();
    let frames = decoder.block(payload, false).unwrap();
    assert!(matches!(frames.as_slice(), [Frame::Issue { op: 59, .. }]));
    assert_eq!(encoder.frame(&frames[0]).0, issue);

    let (_, payload) = blocks.next(read(&tape)).unwrap().unwrap();
    let frames = decoder.block(payload, false).unwrap();
    let [frame @ Frame::Done { task: 7, out, .. }] = frames.as_slice() else {
        panic!("{frames:?}");
    };
    assert_eq!((out.value, out.status, out.written), (42, 0, 1));
    assert_eq!(
        out.payload,
        [words.clone(), vec![0x5a; 65_536 - 72], vec![6, 7, 8]].concat()
    );
    assert_eq!(encoder.frame(frame), (first, vec![data.to_vec()]));

    // A port fill never waits, so it is an OP and never an ISSUE.
    assert_eq!(
        Decoder::default().block(&[0x11, 7, 67, 1, 0], false),
        Err(Corrupt)
    );
    for done in [
        // A nonzero total with an empty first chunk.
        &[2, 0, 0, 42, 1, 72, 0][..],
        // A call shorter than its nine words.
        &[[2, 0, 0, 42, 1, 71, 71].as_slice(), &[0; 71]].concat(),
        // `None` with a call id or a payload.
        &[2, 0, 0, 42, 0, 0, 0],
        &[2, 0, 0, 0, 0, 1, 1, 0],
        // `written` past 1.
        &[[2, 0, 0, 42, 2, 72, 72].as_slice(), &[0; 72]].concat(),
    ] {
        assert_eq!(
            Decoder::default().block(&[issue.as_slice(), done].concat(), false),
            Err(Corrupt)
        );
    }
    let none = Decoder::default()
        .block(&[issue.as_slice(), &[2, 0, 0, 0, 0, 0, 0]].concat(), false)
        .unwrap();
    assert!(matches!(
        &none[1],
        Frame::Done { out, .. } if (out.value, out.written) == (0, 0) && out.payload.is_empty()
    ));
    // Total 73, a first chunk of 72, then a DATA of 2 overruns it.
    let mut decoder = Decoder::default();
    let overrun = [[2, 0, 0, 42, 1, 73, 72].as_slice(), &words].concat();
    assert!(
        decoder
            .block(&[issue.as_slice(), &overrun].concat(), false)
            .is_ok()
    );
    assert_eq!(decoder.block(&[9, 2, 0, 0], false), Err(Corrupt));
}

fn designation_and_returned_value() {
    // A Secret with receiver ordinal 130, stream 5 and origin ordinal 7.
    let mut full = vec![0, 1, 1, 7, 0x82, 1, 1, 5, 0, 0, 0, 7, 3, b'k', b'd', b'f'];
    let mut canonical = full.clone();
    canonical.drain(4..6);
    assert_eq!(Value::decode(&full).unwrap().canonical(), canonical);
    let sealed = canonical.clone();
    full[6] = 3;
    canonical[4] = 3;
    assert_eq!(Value::decode(&full).unwrap().canonical(), canonical);
    assert_ne!(canonical, sealed);

    let body = [vec![4, 3, 0], full.clone(), vec![0]].concat();
    let taken = Decoder::default().block(&take(1, &body), false).unwrap();
    let Frame::Take { body, .. } = &taken[0] else {
        panic!("{taken:?}")
    };
    let Arrival::HelperEnd(_, HelperOutcome::Returned(result), _) = body.arrival() else {
        panic!("{body:?}")
    };
    assert_eq!(result.bytes(), full);
    assert_eq!(result.canonical(), canonical);

    full[6] = 6;
    assert_eq!(Value::decode(&full), Err(Corrupt));
}

fn entry_and_end() {
    let mut value = varint(2 << 20);
    value.extend(vec![0x2a; 2 << 20]);
    value.push(0);
    let mut joined = vec![1, 1];
    joined.extend(16u64.to_le_bytes());
    joined.push(0);
    joined.extend(&value);
    let mut first = [0; 12].to_vec();
    first.extend(varint(joined.len()));
    let chunks: Vec<_> = joined.chunks(65536).collect();
    first.extend(varint(chunks[0].len()));
    first.extend(chunks[0]);
    let mut tape = vec![root_header(), wire(3, 0, 0, &first)];
    tape.extend(continued(3, 1, &chunks[1..], 0));

    let mut canonical = value[..value.len() - 1].to_vec();
    // The canonical Client snapshot starts straight with its stream number.
    canonical.extend([1, 1, 0, 5, 0, 0, 0, 2]);
    canonical.extend(8u64.to_le_bytes());
    canonical.extend(9u64.to_le_bytes());
    let mut first_end = vec![10, 0, 0, 0];
    first_end.extend(varint(canonical.len()));
    first_end.extend(varint(65536));
    first_end.extend(&canonical[..65536]);
    let end_seq = chunks.len() as u64;
    tape.push(wire(3, end_seq, 0, &first_end));
    let end_chunks: Vec<_> = canonical[65536..].chunks(65536).collect();
    tape.extend(continued(3, end_seq + 1, &end_chunks, 2));
    // The writer writes the root's END block last.
    tape.push(EXAMPLE_END.to_vec());
    let tape = tape.concat();

    let (mut index, _) = Index::open(read(&tape)).unwrap();
    let mut stream = index.take(3).unwrap();
    let mut decoder = stream.decoder(read(&tape)).unwrap();
    assert!(decoder.take_entry().is_none());
    let mut saw_entry = false;
    let mut saw_end = false;
    while let Some((flags, payload)) = stream.next(read(&tape)).unwrap() {
        let frames = decoder.block(payload, flags == 2).unwrap();
        if let Some(entry) = decoder.take_entry() {
            let body = entry.body();
            assert_eq!(body.descriptor, [1]);
            assert_eq!(body.n, 16);
            assert_eq!(body.value.bytes(), value);
            let mut encoded = Vec::new();
            assert_eq!(entry.encode(&mut encoded), &chunks[1..]);
            assert_eq!(encoded, first);
            saw_entry = true;
        }
        if let [frame @ Frame::HelperEnd { ending, .. }] = frames.as_slice() {
            let HelperOutcome::Returned(result) = ending.outcome() else {
                panic!("{ending:?}")
            };
            assert_eq!(result.bytes(), canonical);
            let (head, rest) = Encoder::default().frame(frame);
            assert_eq!(head, first_end);
            for (chunk, data) in end_chunks.iter().zip(rest) {
                assert_eq!(
                    data,
                    [vec![9], varint(chunk.len()), chunk.to_vec()].concat()
                );
            }
            saw_end = true;
        }
    }
    assert!(saw_entry && saw_end);
}

#[test]
fn format_15_bytes_rl6() {
    let root = root_header();
    let source = [root.clone(), EXAMPLE_END.to_vec()].concat();
    let (mut index, header) = Index::open(read(&source)).unwrap();
    assert_eq!(header.limit, 65_536);
    assert_eq!(block(&ID, 0, 0, &header.encode([0; 32]).unwrap()), root);
    let mut stream = index.take(0).unwrap();
    let (flags, payload) = stream.next(read(&source)).unwrap().unwrap();
    let frames = Decoder::default().block(payload, flags == 2).unwrap();
    let [
        Frame::Take {
            point: TakePoint::Pump,
            body,
            ..
        },
        Frame::End { .. },
    ] = frames.as_slice()
    else {
        panic!("{frames:?}")
    };
    assert_eq!(
        body.arrival(),
        Arrival::Refund(Credit {
            limit: 0,
            amount: 48
        })
    );
    let mut encoder = Encoder::default();
    let reencoded: Vec<u8> = frames
        .iter()
        .flat_map(|frame| encoder.frame(frame).0)
        .collect();
    assert_eq!(block(&ID, 2, 1, &reencoded), EXAMPLE_END);

    let mut old = root[..root.len() - 8].to_vec();
    old.drain(4..8);
    old[16] = 13;
    let mut old_end = wire(0, 1, 2, ROOT_END);
    old_end.truncate(old_end.len() - 8);
    old_end.drain(4..8);
    let old_tape = [sealed(old), sealed(old_end)].concat();
    assert!(Index::open(read(&old_tape)).is_err());

    index_cases();
    take_cases();
    unit_ops();
    port_delivery();
    designation_and_returned_value();
    entry_and_end();
}
