use hirsel_wire_bench::Result;
use hirsel_wire_bench::codec::{
    self, Bad, Done, Entry, Fault, Frame, Group, Outcome, Refusal, Room, Row, Want,
};

const N: u32 = 0x01020304;
const V: u64 = 0x0102030405060708;

fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}
fn want() -> Want {
    Want {
        identity: bytes("aabb"),
        args_fp: V,
        result_fp: 0x1112131415161718,
    }
}
fn room() -> Room {
    Room {
        requested: V,
        available: 0x1112131415161718,
    }
}
fn done() -> Done {
    Done {
        stream: N,
        p: V,
        outcome: Outcome::Returned(bytes("0000000001")),
    }
}
// Value construction follows W3a's frame-vector-helpers.hirsel; bytes come from
// its shared hand-derived JSON, independently of this Rust codec.
fn row(chunk: u32) -> Row {
    let mut key = N.to_be_bytes().to_vec();
    key.extend(chunk.to_be_bytes());
    let mut value = 64u32.to_le_bytes().to_vec();
    value.extend((0u8..64).map(|i| if chunk == 1953 && i >= 8 { 0 } else { i }));
    Row {
        slot: 0,
        key,
        value: Some(value),
    }
}

fn frame(name: &str) -> Frame {
    match name {
        "want" => Frame::Hello(None, vec![want()]),
        "entry_ready" | "placed_ready" => Frame::Placed(Entry::Ready(N)),
        "entry_resolve" | "placed_resolve" => Frame::Placed(Entry::Resolve(7, bytes("cafe"))),
        "entry_gone" | "placed_gone" => Frame::Placed(Entry::Gone),
        "hello_empty" => Frame::Hello(None, vec![]),
        "hello_credential" => Frame::Hello(Some(bytes("aabb")), vec![want()]),
        "call" => Frame::Call(N, V, 0x11121314, 0x21222324, bytes("aabb")),
        "subscribe_none" => Frame::Subscribe(N, 7, bytes("aabb"), None),
        "subscribe_some" => Frame::Subscribe(N, 7, bytes("aabb"), Some(bytes("cafe"))),
        "unsubscribe" => Frame::Unsubscribe(N),
        "welcome" => Frame::Welcome(
            codec::Caller {
                issuer: 2,
                subject: bytes("000102030405060708090a0b0c0d0e0f"),
            },
            4,
            1048576,
            (0..32).collect(),
            "x86_64-unknown-linux-gnu".to_owned(),
            16777216,
        ),
        "done" | "reset_outcome" => Frame::Reset(V, vec![done()]),
        "done_refused_request_mismatch" => Frame::Reset(
            V,
            vec![Done {
                stream: N,
                p: V,
                outcome: Outcome::Refused(Refusal::RequestMismatch),
            }],
        ),
        "done_refused_history_unavailable" => Frame::Reset(
            V,
            vec![Done {
                stream: N,
                p: V,
                outcome: Outcome::Refused(Refusal::HistoryUnavailable),
            }],
        ),
        "row_removed" => Frame::Batch(
            V,
            vec![],
            vec![Group::board(
                7,
                vec![Row {
                    slot: 3,
                    key: bytes("0102030405060708"),
                    value: None,
                }],
            )],
            vec![],
        ),
        "row" | "viewer_update" => Frame::Batch(
            V,
            vec![],
            vec![Group::board(7, vec![row(0x05060708)])],
            vec![],
        ),
        "batch_empty" => Frame::Batch(V, vec![], vec![], vec![]),
        "empty_initial" => Frame::Batch(V, vec![7], vec![Group::board(7, vec![])], vec![]),
        "caller_update" => Frame::Batch(
            V,
            vec![],
            vec![Group::board(7, vec![row(0x05060708)])],
            vec![done()],
        ),
        "full_board" => Frame::Batch(
            V,
            vec![7],
            vec![Group::board(7, (0..1954).map(row).collect())],
            vec![],
        ),
        "slot_move" => Frame::Batch(
            V,
            vec![],
            vec![Group {
                sub: 7,
                value: 19u32.to_le_bytes().to_vec(),
                rows: vec![
                    Row {
                        slot: 0,
                        key: bytes("0102030405060708"),
                        value: None,
                    },
                    Row {
                        slot: 2,
                        key: bytes("0102030405060708"),
                        value: Some(vec![7]),
                    },
                ],
            }],
            vec![],
        ),
        "reset_empty" => Frame::Reset(V, vec![]),
        other => {
            let outcome = match other {
                "ended_returned" => Outcome::Returned(bytes("0000000001")),
                "ended_failed_raised" => Outcome::Failed(Fault::Raised(9)),
                "ended_failed_out_of_bytes" => Outcome::Failed(Fault::OutOfBytes(room())),
                "ended_failed_out_of_time" => Outcome::Failed(Fault::OutOfTime(V)),
                "ended_failed_gone" => Outcome::Failed(Fault::Gone),
                "ended_failed_conflict" => Outcome::Failed(Fault::Conflict("unique_value".into())),
                "ended_refused_no_entry" => Outcome::Refused(Refusal::NoEntry),
                "ended_refused_malformed" => {
                    Outcome::Refused(Refusal::Malformed(Bad { kind: 6, at: V }))
                }
                "ended_refused_overloaded" => Outcome::Refused(Refusal::Overloaded),
                "ended_refused_deadline" => Outcome::Refused(Refusal::Deadline),
                "ended_refused_budget" => Outcome::Refused(Refusal::Budget(room())),
                "ended_refused_not_allowed" => Outcome::Refused(Refusal::NotAllowed),
                "ended_refused_request_mismatch" => Outcome::Refused(Refusal::RequestMismatch),
                "ended_refused_history_unavailable" => {
                    Outcome::Refused(Refusal::HistoryUnavailable)
                }
                "ended_abandoned" => Outcome::Abandoned,
                "ended_in_doubt" => Outcome::InDoubt,
                _ => panic!("new W3a vector needs a Rust value constructor: {other}"),
            };
            Frame::Ended(N, outcome)
        }
    }
}

#[test]
fn frame_vectors_match_w3a_and_decode_strictly() -> Result<()> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../core/programs/runner/frame-vectors.json");
    let fixtures: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let vectors = fixtures["vectors"].as_array().unwrap();
    for vector in vectors {
        let name = vector["name"].as_str().unwrap();
        let mut expected = bytes(vector["hex"].as_str().unwrap());
        // Wrap standalone type vectors in their corresponding Frame fields.
        // Prefixes and suffixes follow the shared declaration's fixed widths.
        let mut prefix = Vec::new();
        let mut suffix = Vec::new();
        match vector["type"].as_str().unwrap() {
            "Frame" => (),
            "Want" => prefix.extend(bytes("000000000000000001000000")),
            "Entry" => prefix.extend(5u32.to_le_bytes()),
            "Done" => {
                prefix.extend(8u32.to_le_bytes());
                prefix.extend(V.to_le_bytes());
                prefix.extend(1u32.to_le_bytes());
            }
            "Row" => {
                prefix.extend(6u32.to_le_bytes());
                prefix.extend(V.to_le_bytes());
                prefix.extend(0u32.to_le_bytes());
                prefix.extend(1u32.to_le_bytes());
                prefix.extend(7u32.to_le_bytes());
                prefix.extend(0u32.to_le_bytes());
                prefix.extend(1u32.to_le_bytes());
                suffix.extend(0u32.to_le_bytes());
            }
            other => panic!("unknown W3a type {other}"),
        }
        prefix.append(&mut expected);
        prefix.append(&mut suffix);
        expected = prefix;
        let value = frame(name);
        assert_eq!(codec::encode(&value), expected, "{name} encode");
        assert_eq!(codec::decode(&expected)?, value, "{name} decode");
        if let Some(wire_bytes) = vector["wire_bytes"].as_u64() {
            let header = if expected.len() <= 125 {
                2
            } else if expected.len() <= 65535 {
                4
            } else {
                10
            };
            assert_eq!(
                (expected.len() + header) as u64,
                wire_bytes,
                "{name} wire bytes"
            );
        }
        let mut trailing = expected.clone();
        trailing.push(0);
        assert!(codec::decode(&trailing).is_err(), "{name} trailing");
        let prefixes: Vec<usize> = if expected.len() < 512 {
            (0..expected.len()).collect()
        } else {
            vec![0, 3, 4, 12, 16, 20, 28, expected.len() - 1]
        };
        for end in prefixes {
            assert!(
                codec::decode(&expected[..end]).is_err(),
                "{name} prefix {end}"
            );
        }
    }
    println!("matched {} canonical hirsel.3 vectors", vectors.len());
    assert!(codec::decode(&[255; 16]).is_err());
    assert!(codec::decode(&bytes("0000000000000000ffffffff")).is_err());
    Ok(())
}
