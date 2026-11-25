//! The operator socket's wire vectors, shared with operator_wire.hirsel.
use super::*;
use crate::code_admission::ObjectKind;
use hirsel_core_ids::ContentHash;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn d() -> Vec<u8> {
    vec![0x21; 16]
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
fn state(phase: Phase) -> Answer {
    Answer::State(d(), group(), Input::Publication(vec![1]), phase)
}
fn report(pause: Pause) -> Report {
    Report {
        fetch_ns: None,
        assemble_ns: None,
        pause,
        preview: Preview::default(),
    }
}
fn grants() -> Grants {
    Grants {
        added: vec!["Clock".into()],
        removed: vec!["Dir=a".into()],
    }
}

// The expected bytes are shared with core/programs/platform/operator_wire.hirsel. They were
// derived from core/std/api/codec.md's format by an encoder written apart from both
// implementations.
#[test]
fn operator_vectors() {
    let asks = [
        (
            Ask::Begin(
                d(),
                group(),
                Input::Attested(vec![0xcc; 32]),
                vec!["f".into()],
                vec!["e".into()],
                Fates {
                    all: Some(HeldFate::Cancel),
                    entries: vec![(vec![0x70], HeldFate::Release)],
                },
            ),
            "000000001000000021212121212121212121212121212121010000006703000000000000000000000020000000cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc010000000100000066010000000100000065010000000100000001000000010000007000000000",
        ),
        (
            Ask::Data(d(), attempt(), vec![1, 2]),
            "01000000100000002121212121212121212121212121212108070605040302011817161514131211020000000102",
        ),
        (
            Ask::End(d(), attempt()),
            "02000000100000002121212121212121212121212121212108070605040302011817161514131211",
        ),
        (
            Ask::Confirm(d(), attempt(), vec![0xdd; 32]),
            "0300000010000000212121212121212121212121212121210807060504030201181716151413121120000000dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        ),
        (
            Ask::Status(d()),
            "040000001000000021212121212121212121212121212121",
        ),
        (
            Ask::Resolve(vec![0x31; 16]),
            "050000001000000031313131313131313131313131313131",
        ),
        (Ask::Lookup("g".into()), "060000000100000067"),
        (
            Ask::Decide(d(), grants(), true),
            "0800000010000000212121212121212121212121212121210100000005000000436c6f636b01000000050000004469723d6101",
        ),
        (
            Ask::Register(
                d(),
                Box::new(Registration {
                    name: "g".into(),
                    tenant: "t".into(),
                    workdir: "/w".into(),
                    trusted: false,
                    listen: "127.0.0.1:9".into(),
                    store: "/store".into(),
                    memory: Some(4096),
                    args: vec!["--port".into()],
                    record: Some(Record {
                        dir: "/tapes".into(),
                        retention_s: 60,
                    }),
                    front: Some(Front {
                        names: vec!["edge.example".into()],
                        forward: Forward::Stream,
                    }),
                }),
                Input::Publication(vec![1]),
                Vec::new(),
                Vec::new(),
            ),
            "07000000100000002121212121212121212121212121212101000000670100000074020000002f77000b0000003132372e302e302e313a39060000002f73746f726501000000001000000000000001000000060000002d2d706f727401000000060000002f74617065733c0000000000000001000000010000000c000000656467652e6578616d706c65010000000100000001000000010000000000000000",
        ),
    ];
    let answers = [
        (
            Answer::State(
                d(),
                group(),
                Input::Attested(vec![0xcc; 32]),
                Phase::AwaitInput(attempt()),
            ),
            "000000001000000021212121212121212121212121212121010000006703000000000000000000000020000000cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc0000000008070605040302011817161514131211",
        ),
        (
            state(Phase::Pending(attempt(), 5)),
            "0000000010000000212121212121212121212121212121210100000067030000000000000001000000010000000105000000080706050403020118171615141312110500000000000000",
        ),
        (
            state(Phase::Undecided(attempt(), 5, "lost".into())),
            "0000000010000000212121212121212121212121212121210100000067030000000000000001000000010000000106000000080706050403020118171615141312110500000000000000040000006c6f7374",
        ),
        (
            state(Phase::Installed(
                5,
                vec![0xaa; 32],
                Report {
                    fetch_ns: Some(7),
                    assemble_ns: None,
                    pause: Pause::Measured(2000),
                    preview: Preview {
                        grants: Grants {
                            added: vec!["Clock".into()],
                            removed: Vec::new(),
                        },
                        entries: vec![EntryLine {
                            identity: vec![0xee],
                            change: EntryChange::Changed,
                        }],
                        triggers: TriggerReport {
                            rows: vec![TriggerLine {
                                class: 1,
                                kind: 1,
                                path: vec![1],
                                schema: vec![2],
                                hold: Some(HeldBy::Request(true)),
                                fate: Some(HeldFate::Cancel),
                                count: 3,
                                earliest_due: 4,
                            }],
                            unmatched: vec![vec![3]],
                        },
                    },
                },
            )),
            "0000000010000000212121212121212121212121212121210100000067030000000000000001000000010000000107000000050000000000000020000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa0100000007000000000000000000000002000000d0070000000000000100000005000000436c6f636b000000000100000001000000ee0200000001000000010000000100000001000000010100000002010000000200000001010000000100000003000000000000000400000000000000010000000100000003",
        ),
        (
            state(Phase::Installed(
                0,
                vec![0xaa; 32],
                report(Pause::StartFailed("x".into())),
            )),
            "0000000010000000212121212121212121212121212121210100000067030000000000000001000000010000000107000000000000000000000020000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa00000000000000000400000001000000780000000000000000000000000000000000000000",
        ),
        (
            state(Phase::Failed(
                Class::Denied,
                "no".into(),
                report(Pause::NotStarted),
            )),
            "000000001000000021212121212121212121212121212121010000006703000000000000000100000001000000010800000001000000020000006e6f0000000000000000000000000000000000000000000000000000000000000000",
        ),
        (
            state(Phase::AwaitDecision(grants())),
            "00000000100000002121212121212121212121212121212101000000670300000000000000010000000100000001090000000100000005000000436c6f636b01000000050000004469723d61",
        ),
        (
            Answer::Need(
                d(),
                attempt(),
                vec![ObjectKey {
                    kind: ObjectKind::Group,
                    hash: ContentHash([0xaa; 32]),
                }],
                true,
            ),
            "01000000100000002121212121212121212121212121212108070605040302011817161514131211010000000000000020000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01",
        ),
        (
            Answer::Display(d(), attempt(), vec![0xcc; 32], "foreign: x\n".into()),
            "0200000010000000212121212121212121212121212121210807060504030201181716151413121120000000cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc0b000000666f726569676e3a20780a",
        ),
        (
            Answer::Pin(Resolved::Found(vec![0xaa; 32])),
            "030000000000000020000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        (Answer::Pin(Resolved::Gone), "0300000001000000"),
        (
            Answer::Error(Class::NotInstalledAfterRecovery, "x".into()),
            "040000000b0000000100000078",
        ),
        (Answer::Group(group()), "0500000001000000670300000000000000"),
        (
            Answer::Part(10, 0, vec![1, 2]),
            "060000000a000000000000000000000000000000020000000102",
        ),
    ];
    for (value, expected) in &asks {
        assert_eq!(value.encode(), hex(expected), "{value:?}");
        assert_eq!(&Ask::decode(&hex(expected)).unwrap(), value);
    }
    for (value, expected) in &answers {
        assert_eq!(value.encode(), hex(expected), "{value:?}");
        assert_eq!(&Answer::decode(&hex(expected)).unwrap(), value);
    }
}
