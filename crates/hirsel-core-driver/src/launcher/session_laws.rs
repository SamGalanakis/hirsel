use super::*;
use crate::code_admission::ObjectKind;
use hirsel_core_ids::ContentHash;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn group() -> Group {
    Group {
        name: "g".into(),
        incarnation: 3,
    }
}
fn attempt() -> Attempt {
    Attempt {
        hi: 0x0102030405060708,
        lo: 0x1112131415161718,
    }
}
fn serving() -> Child {
    Child {
        group: group(),
        role: Role::Serving(7),
    }
}
fn preparing() -> Child {
    Child {
        group: group(),
        role: Role::Prepare(attempt()),
    }
}
fn launch(mode: Mode) -> Request {
    Request::Start(Launch {
        group: group(),
        assembly: (0..32).collect(),
        workdir: "/w".into(),
        args: vec!["a".into()],
        mode,
        recording: None,
    })
}

// The expected bytes are shared with core/programs/platform/launcher_wire.hirsel. They
// were derived from core/std/api/codec.md's format by an encoder written apart from
// both implementations.
#[test]
fn session_vectors() {
    let requests = [
        (Request::Hello, "00000000".to_owned()),
        (
            Request::Bind(group(), "127.0.0.1:9".into()),
            "01000000010000006703000000000000000b0000003132372e302e302e313a39".into(),
        ),
        (
            Request::Close(group()),
            "0200000001000000670300000000000000".into(),
        ),
        (
            launch(Mode::Install(1, InstallKind::Genesis)),
            "030000000100000067030000000000000020000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f020000002f770100000001000000610000000001000000000000000000000000000000".into(),
        ),
        (
            launch(Mode::Install(1, InstallKind::Standalone(3))),
            "030000000100000067030000000000000020000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f020000002f7701000000010000006100000000010000000000000001000000030000000000000000000000".into(),
        ),
        (
            launch(Mode::Serving(
                7,
                Serve {
                    activation: 5,
                    idle: 250,
                    candidate: Some(6),
                    store_id: Some((1, 2)),
                    witness: None,
                },
            )),
            "030000000100000067030000000000000020000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f020000002f770100000001000000610100000007000000000000000500000000000000fa0000000000000001000000060000000000000001000000010000000000000002000000000000000000000000000000".into(),
        ),
        (
            launch(Mode::Describe(attempt())),
            "030000000100000067030000000000000020000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f020000002f77010000000100000061020000000807060504030201181716151413121100000000".into(),
        ),
        (
            launch(Mode::Preview(attempt(), vec![1])),
            "030000000100000067030000000000000020000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f020000002f770100000001000000610300000008070605040302011817161514131211010000000100000000".into(),
        ),
        (
            launch(Mode::Prepare(attempt(), "x".into(), vec![0xf0])),
            "030000000100000067030000000000000020000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f020000002f770100000001000000610400000008070605040302011817161514131211010000007801000000f000000000".into(),
        ),
        (
            Request::Stop(serving()),
            "0400000001000000670300000000000000010000000700000000000000".into(),
        ),
        (
            Request::Kill(preparing()),
            "05000000010000006703000000000000000400000008070605040302011817161514131211"
                .into(),
        ),
        (
            Request::Acknowledge(serving()),
            "0600000001000000670300000000000000010000000700000000000000".into(),
        ),
        (
            Request::Command(
                serving(),
                HostCommand::Cutover(vec![0xaa], vec![0xbb], 2000, false),
            ),
            "07000000010000006703000000000000000100000007000000000000000200000001000000aa01000000bbd00700000000000000".into(),
        ),
        (
            Request::Command(serving(), HostCommand::Abort(vec![0xaa], Some(vec![0xbb]))),
            "07000000010000006703000000000000000100000007000000000000000300000001000000aa0100000001000000bb".into(),
        ),
        (
            Request::Command(
                serving(),
                HostCommand::Import(vec![0xaa], vec![0xbb], vec![0xcc], vec![0xdd]),
            ),
            "07000000010000006703000000000000000100000007000000000000000100000001000000aa01000000bb01000000cc01000000dd".into(),
        ),
        (
            Request::Command(serving(), HostCommand::StoreAccepted),
            "070000000100000067030000000000000001000000070000000000000004000000".into(),
        ),
        (
            Request::Retire(group()),
            "0800000001000000670300000000000000".into(),
        ),
        (
            Request::Release(attempt()),
            "0900000008070605040302011817161514131211".into(),
        ),
        (Request::CollectBegin, "0a000000".into()),
        (
            Request::Collect(vec![vec![1, 2]]),
            "0b00000001000000020000000102".into(),
        ),
        (
            Request::Start(Launch {
                group: group(),
                assembly: (0..32).collect(),
                workdir: "/w".into(),
                args: vec!["a".into()],
                mode: Mode::Serving(
                    7,
                    Serve {
                        activation: 5,
                        idle: 250,
                        candidate: None,
                        store_id: None,
                        witness: None,
                    },
                ),
                recording: Some(Recording {
                    id: vec![0x21; 16],
                    destination: "/rec".into(),
                }),
            }),
            "030000000100000067030000000000000020000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f020000002f770100000001000000610100000007000000000000000500000000000000fa00000000000000000000000000000000000000010000001000000021212121212121212121212121212121040000002f726563".into(),
        ),
        (
            Request::Admit(
                group(),
                attempt(),
                Input::Attested(vec![0xcc; 32]),
                Approvals {
                    recording: true,
                    foreign: vec!["f".into()],
                    exec: vec!["e".into()],
                    workdir: "/w".into(),
                    old: Some((0..32).collect()),
                },
            ),
            "0c00000001000000670300000000000000080706050403020118171615141312110000000020000000cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc01010000000100000066010000000100000065020000002f770100000020000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f".into(),
        ),
        (
            Request::Admit(
                group(),
                attempt(),
                Input::Publication(vec![1]),
                Approvals {
                    workdir: "/w".into(),
                    ..Approvals::default()
                },
            ),
            "0c0000000100000067030000000000000008070605040302011817161514131211010000000100000001000000000000000000020000002f7700000000".into(),
        ),
        (
            Request::Admit(
                group(),
                attempt(),
                Input::Source(vec![1], true),
                Approvals {
                    workdir: "/w".into(),
                    ..Approvals::default()
                },
            ),
            "0c000000010000006703000000000000000807060504030201181716151413121102000000010000000101000000000000000000020000002f7700000000".into(),
        ),
        (
            Request::AdmitData(attempt(), vec![1, 2]),
            "0d00000008070605040302011817161514131211020000000102".into(),
        ),
        (Request::AdmitEnd(attempt()), "0e00000008070605040302011817161514131211".into()),
        (Request::Assemble(attempt()), "0f00000008070605040302011817161514131211".into()),
        (
            Request::CutoverAck(serving(), attempt()),
            "100000000100000067030000000000000001000000070000000000000008070605040302011817161514131211".into(),
        ),
        (Request::CollectAbort, "11000000".into()),
    ];
    let events = [
        (
            Event::Inventory(
                vec![
                    ChildState {
                        child: serving(),
                        phase: Phase::Started,
                        selection: None,
                        answer: None,
                    },
                    ChildState {
                        child: preparing(),
                        phase: Phase::Exited(End {
                            status: Status::Signal(9),
                            report: None,
                            readable: false,
                        }),
                        selection: None,
                        answer: None,
                    },
                ],
                vec![group()],
                vec![(serving(), attempt())],
            ),
            "000000000200000001000000670300000000000000010000000700000000000000000000000000000000000000010000006703000000000000000400000008070605040302011817161514131211010000000100000009000000000000000000000000000000000100000001000000670300000000000000010000000100000067030000000000000001000000070000000000000008070605040302011817161514131211",
        ),
        (
            Event::Launched(serving(), Phase::Started),
            "010000000100000067030000000000000001000000070000000000000000000000",
        ),
        (
            Event::Refused(serving(), Refusal::Held),
            "020000000100000067030000000000000001000000070000000000000000000000",
        ),
        (
            Event::Refused(preparing(), Refusal::Spawn("e".into())),
            "02000000010000006703000000000000000400000008070605040302011817161514131211050000000100000065",
        ),
        (
            Event::Ended(
                serving(),
                End {
                    status: Status::Code(0),
                    report: Some(Some(2000)),
                    readable: true,
                },
            ),
            "030000000100000067030000000000000001000000070000000000000000000000000000000100000001000000d00700000000000001",
        ),
        (
            Event::Arrived(group()),
            "0400000001000000670300000000000000",
        ),
        (Event::Bound(group()), "0500000001000000670300000000000000"),
        (
            Event::BindFailed(group(), "in use".into()),
            "060000000100000067030000000000000006000000696e20757365",
        ),
        (Event::Closed(group()), "0700000001000000670300000000000000"),
        (
            Event::Retired(group()),
            "0800000001000000670300000000000000",
        ),
        (
            Event::Placed(serving(), HostReply::Installed(4, true)),
            "090000000100000067030000000000000001000000070000000000000005000000040000000000000001",
        ),
        (
            Event::Placed(
                serving(),
                HostReply::Opened(PlaceReady {
                    version: 4,
                    store_hi: 1,
                    store_lo: 2,
                    release: vec![5],
                    retain_from: 1,
                    ready_ns: 99,
                    read: vec![],
                }),
            ),
            "09000000010000006703000000000000000100000007000000000000000000000004000000000000000100000000000000020000000000000001000000050100000000000000630000000000000000000000",
        ),
        (
            Event::Placed(
                serving(),
                HostReply::Imported(vec![0xaa], vec![0xbb], vec![b"t".to_vec()]),
            ),
            "09000000010000006703000000000000000100000007000000000000000400000001000000aa01000000bb010000000100000074",
        ),
        (
            Event::Placed(serving(), HostReply::StoreMismatch(1, 2, 3, 4)),
            "09000000010000006703000000000000000100000007000000000000000a0000000100000000000000020000000000000003000000000000000400000000000000",
        ),
        (
            Event::Placed(
                Child {
                    group: group(),
                    role: Role::Install(1),
                },
                HostReply::InstallRefused(InstallRefusal::WrongVersion(2)),
            ),
            "09000000010000006703000000000000000000000001000000000000000b000000080000000200000000000000",
        ),
        (
            Event::Answered(preparing(), vec![1, 2]),
            "0c000000010000006703000000000000000400000008070605040302011817161514131211020000000102",
        ),
        (Event::Fenced, "0a000000"),
        (Event::Collected(3), "0b0000000300000000000000"),
        (
            Event::Need(
                attempt(),
                vec![
                    ObjectKey {
                        kind: ObjectKind::Group,
                        hash: ContentHash([0xaa; 32]),
                    },
                    ObjectKey {
                        kind: ObjectKind::ForeignArchive,
                        hash: ContentHash([0xbb; 32]),
                    },
                ],
                true,
            ),
            "0d00000008070605040302011817161514131211020000000000000020000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa0300000020000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb01",
        ),
        (
            Event::Admitted(
                attempt(),
                vec![0xcc; 32],
                Summary {
                    root_limit: 1024,
                    old_root_limit: 2048,
                    grants: Grants {
                        added: vec!["Clock".into()],
                        removed: vec!["Dir=a".into()],
                    },
                    target: "t".into(),
                    n_place: 3,
                    gate: None,
                },
                "l".into(),
            ),
            "0e0000000807060504030201181716151413121120000000cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc000400000000000000080000000000000100000005000000436c6f636b01000000050000004469723d610100000074030000000000000000000000010000006c",
        ),
        (
            Event::Admitted(
                attempt(),
                vec![0xcc; 32],
                Summary {
                    root_limit: 1024,
                    old_root_limit: 2048,
                    grants: Grants {
                        added: vec!["Clock".into()],
                        removed: vec!["Dir=a".into()],
                    },
                    target: "t".into(),
                    n_place: 3,
                    gate: Some(Green {
                        tree: vec![0xaa],
                        root: vec![0xbb],
                        toolchain: vec![0xcc],
                    }),
                },
                "l".into(),
            ),
            "0e0000000807060504030201181716151413121120000000cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc000400000000000000080000000000000100000005000000436c6f636b01000000050000004469723d61010000007403000000000000000100000001000000aa01000000bb01000000cc010000006c",
        ),
        (
            Event::Assembled(attempt(), (0..32).collect(), vec![0xdd; 32], 5, 6),
            "0f0000000807060504030201181716151413121120000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20000000dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd05000000000000000600000000000000",
        ),
        (
            Event::AdmissionFailed(attempt(), Failure::Denied("no".into())),
            "100000000807060504030201181716151413121100000000020000006e6f",
        ),
        (
            Event::AdmissionFailed(attempt(), Failure::Red(vec!["a".into(), "b".into()])),
            "1000000008070605040302011817161514131211050000000200000001000000610100000062",
        ),
        (
            Event::AdmissionFailed(attempt(), Failure::GateLost("x".into())),
            "1000000008070605040302011817161514131211060000000100000078",
        ),
        (
            Event::CutoverResult(serving(), attempt(), HostReply::Installed(4, true)),
            "11000000010000006703000000000000000100000007000000000000000807060504030201181716151413121105000000040000000000000001",
        ),
    ];
    for (value, expected) in &requests {
        assert_eq!(value.encode(), hex(expected), "{value:?}");
        assert_eq!(&Request::decode(&hex(expected)).unwrap(), value);
    }
    for (value, expected) in &events {
        assert_eq!(value.encode(), hex(expected), "{value:?}");
        assert_eq!(&Event::decode(&hex(expected)).unwrap(), value);
    }
}

pub(in crate::launcher) fn serve_fixture(activation: u64, candidate: Option<u64>) -> Serve {
    Serve {
        activation,
        idle: 250,
        candidate,
        store_id: None,
        witness: None,
    }
}
