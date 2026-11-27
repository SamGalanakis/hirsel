//! EMB-4 callback payload wire codecs (`Change`, `SubEnd`, `RoomEvent`, `SessionEnd`).
use super::codec::{DecodeErr, Reader, Writer};
// ---------------------------------------------------------------------------
// EMB-4 callback payload types (docs/spec/embed.md lines 817-865)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValueChange {
    pub old: Option<Vec<u8>>,
    pub new: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RowChange {
    pub slot: u32,
    pub key: Vec<u8>,
    pub old: Option<Vec<u8>>,
    pub new: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Change {
    pub invalidated: bool,
    pub replaced: bool,
    pub value: Option<ValueChange>,
    pub rows: Vec<RowChange>,
}

impl Change {
    pub fn encode(&self, w: &mut Writer) {
        w.put_bool(self.invalidated);
        w.put_bool(self.replaced);
        w.put_option(self.value.as_ref(), |w, vc| {
            w.put_option(vc.old.as_ref(), |w, b| w.put_bytes(b));
            w.put_bytes(&vc.new);
        });
        w.put_list(&self.rows, |w, rc| {
            w.put_u32(rc.slot);
            w.put_bytes(&rc.key);
            w.put_option(rc.old.as_ref(), |w, b| w.put_bytes(b));
            w.put_option(rc.new.as_ref(), |w, b| w.put_bytes(b));
        });
    }

    pub fn encode_bytes(&self) -> Vec<u8> {
        let mut w = Writer::new();
        self.encode(&mut w);
        w.out
    }

    pub fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeErr> {
        Ok(Self {
            invalidated: r.take_bool()?,
            replaced: r.take_bool()?,
            value: r.take_option(|r| {
                Ok(ValueChange {
                    old: r.take_option(|r| r.take_bytes())?,
                    new: r.take_bytes()?,
                })
            })?,
            rows: r.take_list(|r| {
                Ok(RowChange {
                    slot: r.take_u32()?,
                    key: r.take_bytes()?,
                    old: r.take_option(|r| r.take_bytes())?,
                    new: r.take_option(|r| r.take_bytes())?,
                })
            })?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubEnd {
    Server(Vec<u8>),
    ClientCapacity,
}

impl SubEnd {
    pub fn encode(&self, w: &mut Writer) {
        match self {
            Self::Server(outcome) => {
                w.put_u32(0);
                w.put_bytes(outcome);
            }
            Self::ClientCapacity => w.put_u32(1),
        }
    }

    pub fn encode_bytes(&self) -> Vec<u8> {
        let mut w = Writer::new();
        self.encode(&mut w);
        w.out
    }

    pub fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeErr> {
        match r.take_u32()? {
            0 => Ok(Self::Server(r.take_bytes()?)),
            1 => Ok(Self::ClientCapacity),
            _ => Err(DecodeErr("invalid SubEnd tag")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Peer {
    pub peer: u64,
    pub caller: Vec<u8>,
    pub state: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerState {
    pub peer: u64,
    pub state: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeersChange {
    pub incarnation: Option<Vec<u8>>,
    pub revision: Option<u64>,
    pub renewed: bool,
    pub me: Option<u64>,
    pub joined: Vec<Peer>,
    pub states: Vec<PeerState>,
    pub left: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageIn {
    pub from: u64,
    pub message: Vec<u8>,
    pub direct: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoomEvent {
    Admission(Change),
    Peers(PeersChange),
    Message(MessageIn),
    Gap(u64),
    Lost(u64),
    Full,
    Ended(SubEnd),
}

impl RoomEvent {
    pub fn encode(&self, w: &mut Writer) {
        match self {
            Self::Admission(c) => {
                w.put_u32(0);
                c.encode(w);
            }
            Self::Peers(p) => {
                w.put_u32(1);
                w.put_option(p.incarnation.as_ref(), |w, b| w.put_bytes(b));
                w.put_option(p.revision.as_ref(), |w, &rev| w.put_u64(rev));
                w.put_bool(p.renewed);
                w.put_option(p.me.as_ref(), |w, &id| w.put_u64(id));
                w.put_list(&p.joined, |w, peer| {
                    w.put_u64(peer.peer);
                    w.put_bytes(&peer.caller);
                    w.put_option(peer.state.as_ref(), |w, b| w.put_bytes(b));
                });
                w.put_list(&p.states, |w, ps| {
                    w.put_u64(ps.peer);
                    w.put_option(ps.state.as_ref(), |w, b| w.put_bytes(b));
                });
                w.put_list(&p.left, |w, &id| w.put_u64(id));
            }
            Self::Message(m) => {
                w.put_u32(2);
                w.put_u64(m.from);
                w.put_bytes(&m.message);
                w.put_bool(m.direct);
            }
            Self::Gap(n) => {
                w.put_u32(3);
                w.put_u64(*n);
            }
            Self::Lost(n) => {
                w.put_u32(4);
                w.put_u64(*n);
            }
            Self::Full => w.put_u32(5),
            Self::Ended(e) => {
                w.put_u32(6);
                e.encode(w);
            }
        }
    }

    pub fn encode_bytes(&self) -> Vec<u8> {
        let mut w = Writer::new();
        self.encode(&mut w);
        w.out
    }

    pub fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeErr> {
        match r.take_u32()? {
            0 => Ok(Self::Admission(Change::decode(r)?)),
            1 => Ok(Self::Peers(PeersChange {
                incarnation: r.take_option(|r| r.take_bytes())?,
                revision: r.take_option(|r| r.take_u64())?,
                renewed: r.take_bool()?,
                me: r.take_option(|r| r.take_u64())?,
                joined: r.take_list(|r| {
                    Ok(Peer {
                        peer: r.take_u64()?,
                        caller: r.take_bytes()?,
                        state: r.take_option(|r| r.take_bytes())?,
                    })
                })?,
                states: r.take_list(|r| {
                    Ok(PeerState {
                        peer: r.take_u64()?,
                        state: r.take_option(|r| r.take_bytes())?,
                    })
                })?,
                left: r.take_list(|r| r.take_u64())?,
            })),
            2 => Ok(Self::Message(MessageIn {
                from: r.take_u64()?,
                message: r.take_bytes()?,
                direct: r.take_bool()?,
            })),
            3 => Ok(Self::Gap(r.take_u64()?)),
            4 => Ok(Self::Lost(r.take_u64()?)),
            5 => Ok(Self::Full),
            6 => Ok(Self::Ended(SubEnd::decode(r)?)),
            _ => Err(DecodeErr("invalid RoomEvent tag")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionEnd {
    Closed,
    RuntimeStopped,
    RuntimeEnded(u32, Vec<u8>),
    DialRefused(u16),
    ClosedByPlace(u16),
}

impl SessionEnd {
    pub fn encode_bytes(&self) -> Vec<u8> {
        let mut w = Writer::new();
        match self {
            Self::Closed => w.put_u32(0),
            Self::RuntimeStopped => w.put_u32(1),
            Self::RuntimeEnded(code, reason) => {
                w.put_u32(2);
                w.put_u32(*code);
                w.put_bytes(reason);
            }
            Self::DialRefused(code) => {
                w.put_u32(3);
                w.put_u16(*code);
            }
            Self::ClosedByPlace(code) => {
                w.put_u32(4);
                w.put_u16(*code);
            }
        }
        w.out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeErr> {
        let mut r = Reader::new(bytes);
        let v = match r.take_u32()? {
            0 => Self::Closed,
            1 => Self::RuntimeStopped,
            2 => Self::RuntimeEnded(r.take_u32()?, r.take_bytes()?),
            3 => Self::DialRefused(r.take_u16()?),
            4 => Self::ClosedByPlace(r.take_u16()?),
            _ => return Err(DecodeErr("invalid SessionEnd tag")),
        };
        r.done()?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::super::codec::{Caller, Entry, Frame, Part, StreamId, Want};
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        hirsel_core_ids::hex::decode(&clean).unwrap()
    }

    #[test]
    fn frame_vectors_match_codec_frames_mjs() {
        let cases: Vec<(Frame, &str)> = vec![
            (Frame::Hello(None, vec![]), "00000000 00000000 00000000"),
            (
                Frame::Hello(
                    Some(hex("aa")),
                    vec![Want {
                        identity: hex("bb"),
                        args_fp: 1,
                        result_fp: 2,
                    }],
                ),
                "00000000 01000000 01000000 aa 01000000 01000000 bb 0100000000000000 0200000000000000",
            ),
            (
                Frame::Call(StreamId { t: 0, n: 7 }, 9007199254740993, 2, 3, hex("ff")),
                "01000000 0000000000000000 0700000000000000 0100000000002000 02000000 03000000 01000000 ff",
            ),
            (
                Frame::Subscribe(1, 2, vec![], Some(hex("aa"))),
                "02000000 01000000 02000000 00000000 01000000 01000000 aa",
            ),
            (Frame::Unsubscribe(7), "03000000 07000000"),
            (
                Frame::Welcome {
                    caller: Caller {
                        issuer: 2,
                        subject: hex("aa"),
                    },
                    q: 2,
                    budget: 3,
                    digest: vec![],
                    target: "t".into(),
                    n_place: 4,
                    now_ms: 5,
                    retention_ms: 6,
                },
                "04000000 02 01000000 aa 02000000 0300000000000000 00000000 01000000 74 0400000000000000 0500000000000000 0600000000000000",
            ),
            (Frame::Placed(Entry::Ready(3)), "05000000 00000000 03000000"),
            (
                Frame::Placed(Entry::Resolve(3, hex("07"))),
                "05000000 01000000 03000000 01000000 07",
            ),
            (Frame::Placed(Entry::Gone), "05000000 02000000"),
            (
                Frame::Set(7, Some(hex("aabb"))),
                "09000000 07000000 01000000 02000000 aabb",
            ),
            (
                Frame::Send(7, Some(8), hex("aabb")),
                "0a000000 07000000 01000000 08000000 02000000 aabb",
            ),
            (
                Frame::Live(vec![Part {
                    sub: 7,
                    complete: true,
                    you: Some(8),
                    incarnation: (0x0706050403020100, 0x0f0e0d0c0b0a0908),
                    revision: 0x0102030405060708,
                    snapshot: true,
                    joined: vec![(
                        9,
                        Caller {
                            issuer: 2,
                            subject: hex("000102030405060708090a0b0c0d0e0f"),
                        },
                    )],
                    states: vec![(9, Some(hex("aabb")))],
                    msgs: vec![(9, true, hex("cafe"))],
                    left: vec![10],
                    gap: 11,
                    lost: 12,
                    full: true,
                }]),
                "0b000000 01000000 07000000 01 01000000 08000000 000102030405060708090a0b0c0d0e0f 0807060504030201 01 01000000 09000000 02 10000000 000102030405060708090a0b0c0d0e0f 01000000 09000000 01000000 02000000 aabb 01000000 09000000 01 02000000 cafe 01000000 0a000000 0b000000 0c000000 01",
            ),
        ];
        for (frame, expected_hex) in cases {
            let expected = hex(expected_hex);
            assert_eq!(frame.encode(), expected);
            assert_eq!(Frame::decode(&expected).unwrap(), frame);
        }
    }

    #[test]
    fn emb4_callback_payload_vectors() {
        let change = Change {
            invalidated: false,
            replaced: true,
            value: Some(ValueChange {
                old: None,
                new: vec![1, 2],
            }),
            rows: vec![RowChange {
                slot: 0,
                key: vec![0xaa],
                old: None,
                new: Some(vec![0xbb]),
            }],
        };
        let encoded = change.encode_bytes();
        let mut r = Reader::new(&encoded);
        assert_eq!(Change::decode(&mut r).unwrap(), change);
        r.done().unwrap();

        let se = SessionEnd::DialRefused(503);
        assert_eq!(se.encode_bytes(), hex("03000000 f701"));
        assert_eq!(SessionEnd::decode(&se.encode_bytes()).unwrap(), se);
    }
}
