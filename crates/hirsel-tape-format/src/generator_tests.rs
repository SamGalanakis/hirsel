use super::units::SURVIVOR_KINDS;
use super::*;
use std::convert::Infallible;

struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

fn bytes(random: &mut SplitMix64, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        out.extend_from_slice(&random.next().to_le_bytes());
    }
    out.truncate(len);
    out
}

fn encode(frames: &[Frame]) -> Vec<u8> {
    let mut encoder = Encoder::default();
    let mut payload = Vec::new();
    for frame in frames {
        let (frame_bytes, data) = encoder.frame(frame);
        payload.extend_from_slice(&frame_bytes);
        for chunk in data {
            payload.extend_from_slice(&chunk);
        }
    }
    payload
}

fn roundtrip(frames: &[Frame], end: bool, physical: bool) -> Result<Vec<Frame>, Corrupt> {
    let payload = encode(frames);
    let mut decoder = Decoder::default();
    decoder.physical = physical;
    decoder.block(&payload, end)
}

fn roundtrip_exact(frames: &[Frame]) {
    let decoded = roundtrip(frames, false, false).unwrap();
    assert_eq!(decoded, frames);
}

fn call_part(opcode: u8) -> bool {
    matches!(
        opcode,
        op::CALL_WORD | op::CALL_PAYLOAD | op::CALL_PAYLOAD_PREFIX
    )
}

/// docs/recording.md, "Unit operations" and the served-port paragraph: these
/// operations are only ever an `OP`.
fn op_only(opcode: u8) -> bool {
    matches!(
        opcode,
        op::UNIT_SPAWN | op::CONN_TRY_SEND | op::UNIT_CANCEL | op::PORT_REPLY | op::PORT_FILL
    )
}

/// The same paragraph: a served port's `PORT_NEXT` is always an `ISSUE` and a
/// `DONE`, and the call parts are no frames at all.
fn shape_allowed(opcode: u8, issue: bool) -> bool {
    !call_part(opcode)
        && if issue {
            !op_only(opcode)
        } else {
            opcode != op::PORT_NEXT
        }
}

fn address() -> Vec<u8> {
    let mut out = vec![1];
    out.resize(19, 0);
    out
}

fn tls_payload() -> Vec<u8> {
    let mut out = vec![0];
    out.extend_from_slice(&[0; 32]);
    out.push(0);
    out
}

fn temp_suffix(size: usize) -> Vec<u8> {
    vec![b'a'; size.clamp(1, 32)]
}

fn port_next_out(size: usize, entropy: &[u8]) -> Out {
    if size < PORT_CALL_WORDS {
        return Out::default();
    }
    let mut payload = Vec::with_capacity(size);
    for word in 1..=9u64 {
        payload.extend_from_slice(&word.to_le_bytes());
    }
    payload.extend_from_slice(&entropy[..size - PORT_CALL_WORDS]);
    Out {
        status: OK,
        value: size as u64,
        error: (0, 0),
        written: 1,
        payload,
    }
}

fn out_for(opcode: u8, size: usize, entropy: &[u8]) -> Out {
    if opcode == op::PORT_NEXT {
        return port_next_out(size, entropy);
    }
    let mut value = 0u64;
    let mut written = 0u64;
    let mut payload = Vec::new();
    if carries_payload(opcode) {
        if matches!(
            opcode,
            op::READ | op::READ_AT | op::RECV_BOUNDED | op::STDIN_READ
        ) {
            value = size as u64;
            payload = entropy[..size].to_vec();
        } else if opcode == op::RESOLVE {
            value = (size / hirsel_core_net::IP_RECORD_LEN) as u64;
            payload = entropy[..value as usize * hirsel_core_net::IP_RECORD_LEN].to_vec();
        } else if opcode == op::DATAGRAM_RECV {
            let total = size.max(1);
            written = (total - 1) as u64;
            payload = entropy[..total].to_vec();
        } else if opcode == op::UDP_RECV {
            written = size as u64;
            payload = entropy[..size].to_vec();
        } else if matches!(opcode, op::RANDOM | op::NEXT_ENTRIES) {
            written = size as u64;
            payload = entropy[..size].to_vec();
        } else if matches!(
            opcode,
            op::LISTEN | op::ACCEPT | op::LOCAL_ADDR | op::PEER_ADDR
        ) {
            payload = address();
        } else if matches!(opcode, op::MKDTEMP | op::MKSTEMP) {
            payload = temp_suffix(size);
        } else if opcode == op::TLS_CONNECT {
            payload = tls_payload();
        } else if opcode == op::TLS_HELLO {
            payload = vec![b'a'; size.min(253)];
        } else if opcode == op::TLS_ACCEPT {
            payload = vec![(size % 17) as u8];
        } else {
            payload = entropy[..size.min(PAYLOAD_CHUNK)].to_vec();
        }
    }
    if writes(opcode)
        && !matches!(
            opcode,
            op::RANDOM | op::NEXT_ENTRIES | op::DATAGRAM_RECV | op::UDP_RECV | op::PORT_NEXT
        )
    {
        written = size as u64;
    }
    if opcode == op::UNIT_SPAWN {
        value = size as u64 % u64::from(u32::MAX) + 1;
    } else if opcode == op::CONN_SEND {
        value = size as u64 % 2;
    } else if opcode == op::CONN_TRY_SEND {
        value = size as u64 % 3;
    } else if opcode == op::UNIT_CANCEL {
        value = 0;
    } else if matches!(
        opcode,
        op::MEDIA_P256_KEY | op::MEDIA_KEY_SHARE | op::SECRET_KDF
    ) {
        value = size as u64 + 1;
    } else if opcode == op::MEDIA_AGREEMENT {
        value = size as u64 % 3;
    } else if opcode == op::SCRAM_VERIFY {
        value = size as u64 % 2;
    } else if opcode == op::SCRAM {
        value = size as u64 + 1;
    }
    Out {
        status: OK,
        value,
        error: (0, 0),
        written,
        payload,
    }
}

fn args_for(opcode: u8, size: usize, entropy: &[u8]) -> Args {
    if opcode == op::UNIT_CANCEL {
        return Args::Literal(Vec::new());
    }
    let len = match size {
        0 | 1 | 15 | 16 | 17 => size,
        _ => 18 + size % 23,
    };
    Args::of(&entropy[..len])
}

fn op_case(opcode: u8, size: usize, entropy: &[u8]) -> Vec<Vec<Frame>> {
    let task = 7 + size as u64 % 5;
    let object = if opcode == op::UNIT_SPAWN {
        0
    } else {
        3 + size as u64 % 11
    };
    let args = args_for(opcode, size, entropy);
    let out = out_for(opcode, size, entropy);
    let mut cases = Vec::new();
    if shape_allowed(opcode, false) {
        cases.push(vec![Frame::Op {
            task,
            op: opcode,
            object,
            args: args.clone(),
            out: out.clone(),
        }]);
    }
    if opcode == op::CONN_SEND {
        let body = TakeBody::decode(vec![3, 1, 2]).unwrap();
        cases.push(vec![
            Frame::Issue {
                task,
                op: opcode,
                object,
                args,
            },
            Frame::Take {
                gap: 0,
                point: TakePoint::Operation,
                count: 0,
                body,
            },
        ]);
    } else if shape_allowed(opcode, true) {
        cases.push(vec![
            Frame::Issue {
                task,
                op: opcode,
                object,
                args,
            },
            Frame::Done { gap: 0, task, out },
        ]);
    }
    cases
}

/// Boundary values for one survivor field: the varint length steps and the
/// type's extremes.
const EDGES: [u64; 7] = [0, 1, 127, 128, 16_383, 16_384, u64::MAX];

/// A helper-end TAKE body written from docs/recording.md alone: arrival kind 4,
/// the helper ordinal, outcome `Returned` with the unit value, then each
/// survivor as kind `u8`, origin kind `u8` for kinds 2 and 7, stream `u32`,
/// ordinal `varint` and amount `u64`.
fn helper_end_body(survivors: &[(u8, Option<u8>, u32, u64, u64)]) -> Vec<u8> {
    let mut body = vec![4, 3, 0, 0, 0];
    put_varint(&mut body, survivors.len() as u64);
    for &(kind, origin, stream, ordinal, amount) in survivors {
        body.push(kind);
        body.extend(origin);
        body.extend_from_slice(&stream.to_le_bytes());
        put_varint(&mut body, ordinal);
        body.extend_from_slice(&amount.to_le_bytes());
    }
    body
}

fn generated_survivors(random: &mut SplitMix64) -> Vec<(u8, Option<u8>, u32, u64, u64)> {
    let mut out = Vec::new();
    for &kind in SURVIVOR_KINDS {
        for edge in 0..EDGES.len() {
            let pick = |random: &mut SplitMix64| EDGES[random.next() as usize % EDGES.len()];
            let origin = matches!(kind, 2 | 7).then(|| (random.next() % 6) as u8);
            let stream = [0, 1, u32::MAX, random.next() as u32][edge % 4];
            out.push((kind, origin, stream, EDGES[edge], pick(random)));
        }
    }
    out
}

#[test]
fn generated_survivors_name_their_spec_fields() {
    for seed in 0..32u64 {
        let mut random = SplitMix64::new(0x1414_2222_0000_0000 + seed);
        let mut expected = generated_survivors(&mut random);
        // Shuffle so every kind sits beside every other at some seed.
        for i in (1..expected.len()).rev() {
            expected.swap(i, random.next() as usize % (i + 1));
        }
        let body = TakeBody::decode(helper_end_body(&expected))
            .unwrap_or_else(|err| panic!("seed {seed}: a spec survivor list gave {err:?}"));
        let Arrival::HelperEnd(3, _, survivors) = body.arrival() else {
            panic!("seed {seed}: a helper end decoded as another arrival")
        };
        let found = survivors
            .iter()
            .map(|s| (s.kind, s.origin_kind, s.ordinal, s.amount))
            .collect::<Vec<_>>();
        let want = expected
            .iter()
            .map(|&(kind, origin, _, ordinal, amount)| (kind, origin, ordinal, amount))
            .collect::<Vec<_>>();
        assert_eq!(found, want, "seed {seed}");
        let streams = survivors.iter().map(|s| s.stream).collect::<Vec<_>>();
        let want = expected.iter().map(|s| s.2).collect::<Vec<_>>();
        assert_eq!(streams, want, "seed {seed}");
        // The kind after the last one the format names is Corrupt.
        let past = SURVIVOR_KINDS.iter().max().unwrap() + 1;
        let mut bad = helper_end_body(&expected[..1]);
        bad[6] = past;
        assert_eq!(
            TakeBody::decode(bad),
            Err(Corrupt),
            "seed {seed} kind {past}"
        );
        let frame = Frame::Take {
            gap: random.next() >> 1,
            point: TakePoint::Pump,
            count: random.next() % 4,
            body,
        };
        roundtrip_exact(&[frame]);
    }
}

#[test]
fn generated_opcodes_and_payload_boundaries_round_trip() {
    let sizes = [
        0,
        1,
        15,
        16,
        17,
        PAYLOAD_CHUNK - 1,
        PAYLOAD_CHUNK,
        PAYLOAD_CHUNK + 1,
    ];
    for seed in 0..16u64 {
        let mut random = SplitMix64::new(0x1414_0000_0000_0000 + seed);
        let entropy = bytes(&mut random, PAYLOAD_CHUNK + 1);
        for &opcode in op::OPERATIONS {
            for size in sizes {
                for case in op_case(opcode, size, &entropy) {
                    let decoded = roundtrip(&case, false, false).unwrap_or_else(|err| {
                        panic!("seed {seed} opcode {opcode} size {size} failed with {err:?}")
                    });
                    assert_eq!(decoded, case, "seed {seed} opcode {opcode} size {size}");
                }
            }
            // A shape the spec forbids never decodes as some other frame.
            let args = args_for(opcode, 17, &entropy);
            let (task, object) = (1 + seed, 3);
            if !shape_allowed(opcode, true) {
                let issue = Frame::Issue {
                    task,
                    op: opcode,
                    object,
                    args: args.clone(),
                };
                assert_eq!(
                    roundtrip(&[issue], false, false),
                    Err(Corrupt),
                    "seed {seed}: opcode {opcode} decoded as an ISSUE"
                );
            }
            if !shape_allowed(opcode, false) {
                let one = Frame::Op {
                    task,
                    op: opcode,
                    object,
                    args,
                    out: Out::default(),
                };
                assert_eq!(
                    roundtrip(&[one], false, false),
                    Err(Corrupt),
                    "seed {seed}: opcode {opcode} decoded as an OP"
                );
            }
        }
    }
}

fn grant_frame(kind: u8, size: usize, entropy: &[u8]) -> Frame {
    let args = if kind == ARGS_KIND {
        Some(entropy[..size.min(entropy.len())].to_vec())
    } else if kind == EXEC_KIND {
        Some(entropy[..size.max(1)].to_vec())
    } else if is_stream_grant(kind) {
        Some(vec![(size % 2) as u8])
    } else {
        None
    };
    Frame::Grant {
        gap: size as u64,
        kind,
        args,
    }
}

#[test]
fn generated_frame_variants_round_trip() {
    let mut random = SplitMix64::new(0x1414_1111_0000_0000);
    let entropy = bytes(&mut random, PAYLOAD_CHUNK + 1);
    for frame in [
        Frame::Wait { gap: 3, task: 9 },
        Frame::Wake { gap: 4, task: 9 },
        Frame::Cancel { gap: 5, task: 9 },
        Frame::Retired {
            gap: 6,
            task: 9,
            pool: 2,
        },
        Frame::Choose { gap: 7, index: 2 },
    ] {
        roundtrip_exact(&[frame]);
    }
    let repeat = [
        Frame::Choose { gap: 0, index: 1 },
        Frame::Choose { gap: 1, index: 2 },
        Frame::Repeat { count: 5 },
    ];
    assert_eq!(roundtrip(&repeat, false, true).unwrap(), repeat);
    for kind in 0..=SECRET_KIND {
        for size in [0, 1, PAYLOAD_CHUNK, PAYLOAD_CHUNK + 1] {
            roundtrip_exact(&[grant_frame(kind, size, &entropy)]);
        }
    }
    for kind in [EXEC_KIND, CRED_KIND] {
        for size in [1, PAYLOAD_CHUNK, PAYLOAD_CHUNK + 1] {
            roundtrip_exact(&[grant_frame(kind, size, &entropy)]);
        }
    }
    for len in [0, 1, 15, 16, 17, 64] {
        let args = if len == 64 {
            Args::Literal(entropy[..16].to_vec()).buffered([3, 4])
        } else {
            Args::of(&entropy[..len])
        };
        roundtrip_exact(&[Frame::Op {
            task: 1,
            op: op::CLOSE,
            object: 2,
            args,
            out: Out::default(),
        }]);
    }
    let end = [Frame::End {
        gap: 0,
        ending: Ending::returned(&0u32.to_le_bytes()),
    }];
    assert_eq!(roundtrip(&end, true, false).unwrap(), end);
}

const ID: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

fn entry_body() -> Vec<u8> {
    let mut out = Vec::new();
    put_bytes(&mut out, &[1]);
    out.extend_from_slice(&0u64.to_le_bytes());
    out.push(0);
    out.extend_from_slice(&[0, 0]);
    out
}

fn helper_header(parent: u32, spawn: u64) -> Vec<u8> {
    let entry = Entry::decode(parent, spawn, entry_body()).unwrap();
    let mut out = Vec::new();
    let rest = entry.encode(&mut out);
    assert!(rest.is_empty());
    out
}

fn helper_end_frame() -> Frame {
    Frame::HelperEnd {
        gap: 0,
        ending: HelperEnding::decode(END_RETURNED, vec![0, 0]).unwrap(),
    }
}

fn root_end_frame() -> Frame {
    Frame::End {
        gap: 0,
        ending: Ending::returned(&0u32.to_le_bytes()),
    }
}

fn refund_take() -> Frame {
    let mut body = vec![9, 0];
    body.extend_from_slice(&48u64.to_le_bytes());
    Frame::Take {
        gap: 0,
        point: TakePoint::Pump,
        count: 0,
        body: TakeBody::decode(body).unwrap(),
    }
}

fn three_stream_tape() -> Vec<u8> {
    let header = Header {
        kind: KIND_RUN,
        tape_id: ID,
        identity: Vec::new(),
        revision: [0; 32],
        charge_policy: 0,
        limit: 65_536,
        bound: 0,
        chooser: CHOOSER_FIFO,
        arg: Vec::new(),
        net_rules: Vec::new(),
        secret_grants: Vec::new(),
        cred_grants: Vec::new(),
        env: None,
    }
    .encode([0; 32])
    .unwrap();
    let root0 = stream_block(&ID, 0, 0, 0, &header);
    let helper1 = stream_block(&ID, 0, 1, 0, &helper_header(0, 0));
    let helper2 = stream_block(&ID, 0, 2, 0, &helper_header(0, 1));
    let helper1_end = stream_block(&ID, END, 1, 1, &encode(&[helper_end_frame()]));
    let root_take = stream_block(&ID, 0, 0, 1, &encode(&[refund_take()]));
    let helper2_end = stream_block(&ID, END, 2, 1, &encode(&[helper_end_frame()]));
    let root_end = stream_block(&ID, END, 0, 2, &encode(&[root_end_frame()]));
    [
        root0,
        helper1,
        helper2,
        helper1_end,
        root_take,
        helper2_end,
        root_end,
    ]
    .concat()
}

fn positional(tape: &[u8]) -> impl FnMut(u64, &mut [u8]) -> Result<usize, Infallible> + '_ {
    |offset, output| {
        let start = usize::try_from(offset).unwrap();
        let n = output.len().min(tape.len().saturating_sub(start));
        output[..n].copy_from_slice(&tape[start..start + n]);
        Ok(n)
    }
}

#[derive(Debug)]
enum TapeReading {
    Sealed(Vec<(u32, Vec<Frame>)>),
    Damaged(u64),
    Corrupt(u64),
    Other(Stop),
}

fn stream_block_numbers() -> std::collections::BTreeMap<(u32, u64), u64> {
    [((1, 1), 3), ((0, 1), 4), ((2, 1), 5), ((0, 2), 6)]
        .into_iter()
        .collect()
}

fn read_tape(tape: &[u8]) -> TapeReading {
    let (mut index, _) = match Index::open(positional(tape)) {
        Ok(opened) => opened,
        Err(ReadError::Format(Stop::Damaged(block))) => return TapeReading::Damaged(block),
        Err(ReadError::Format(Stop::Corrupt)) => return TapeReading::Corrupt(0),
        Err(ReadError::Format(stop)) => return TapeReading::Other(stop),
        Err(ReadError::Source(error)) => match error {},
    };
    let ids = index.paths().map(|(id, _)| id).collect::<Vec<_>>();
    let numbers = stream_block_numbers();
    let mut streams = Vec::new();
    for id in ids {
        let Some(mut stream) = index.take(id) else {
            continue;
        };
        let mut decoder = match stream.decoder(positional(tape)) {
            Ok(decoder) => decoder,
            Err(ReadError::Format(Stop::Corrupt)) => return TapeReading::Corrupt(0),
            Err(ReadError::Format(Stop::Damaged(block))) => return TapeReading::Damaged(block),
            Err(ReadError::Format(stop)) => return TapeReading::Other(stop),
            Err(ReadError::Source(error)) => match error {},
        };
        let mut frames = Vec::new();
        let mut sequence = 0u64;
        loop {
            match stream.next(positional(tape)) {
                Ok(Some((flags, payload))) => {
                    sequence += 1;
                    match decoder.block(payload, flags & END != 0) {
                        Ok(decoded) => frames.extend(decoded),
                        Err(Corrupt) => {
                            let block = numbers.get(&(id, sequence)).copied().unwrap_or(0);
                            return TapeReading::Corrupt(block);
                        }
                    }
                }
                Ok(None) => break,
                Err(ReadError::Format(Stop::Damaged(block))) => return TapeReading::Damaged(block),
                Err(ReadError::Format(Stop::Corrupt)) => {
                    let block = numbers.get(&(id, sequence + 1)).copied().unwrap_or(0);
                    return TapeReading::Corrupt(block);
                }
                Err(ReadError::Format(stop)) => return TapeReading::Other(stop),
                Err(ReadError::Source(error)) => match error {},
            }
        }
        if decoder.unfinished() {
            return TapeReading::Corrupt(numbers.get(&(id, sequence)).copied().unwrap_or(0));
        }
        streams.push((id, frames));
    }
    TapeReading::Sealed(streams)
}

fn block_offsets(tape: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < tape.len() {
        let len = u32::from_le_bytes(tape[at + 16..at + 20].try_into().unwrap()) as usize;
        let end = at + BLOCK_HEAD + len + BLOCK_CHECK;
        out.push((at, end));
        at = end;
    }
    out
}

#[test]
fn every_byte_flip_is_damage_corruption_or_the_same_sealed_tape() {
    let tape = three_stream_tape();
    let TapeReading::Sealed(original) = read_tape(&tape) else {
        panic!("the generated three-stream tape does not seal")
    };
    let blocks = block_offsets(&tape);
    for index in 0..tape.len() {
        let block = blocks
            .iter()
            .position(|(start, end)| (*start..*end).contains(&index))
            .unwrap() as u64;
        let mut flipped = tape.clone();
        flipped[index] ^= 0xff;
        match read_tape(&flipped) {
            TapeReading::Damaged(found) => assert_eq!(found, block, "byte {index}"),
            TapeReading::Corrupt(found) => assert_eq!(found, block, "byte {index}"),
            TapeReading::Sealed(sealed) => {
                assert_eq!(sealed, original, "byte {index} decoded to a different tape")
            }
            TapeReading::Other(Stop::Truncated)
                if blocks
                    .iter()
                    .any(|(start, _)| (start + 16..start + 20).contains(&index)) => {}
            TapeReading::Other(stop) => panic!("byte {index} gave {stop:?}"),
        }
    }
}
