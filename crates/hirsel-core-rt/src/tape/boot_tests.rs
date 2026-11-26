use super::*;

/// Wrong implementation caught: BOOT_NS replaying from the host, or replay
/// reconstructing one boot reading from another clock kind. The expected
/// values are the scripted host's independent answers.
#[test]
fn boot_clock_readings_replay_exactly() {
    struct Boots {
        values: VecDeque<u64>,
    }
    impl Boots {
        fn provider(&mut self) -> Provider {
            unsafe extern "C" fn issue(s: *mut u8, r: *mut Request, _: u64) -> Step {
                let boots = unsafe { &mut *s.cast::<Boots>() };
                let request = unsafe { &mut *r };
                assert_eq!(request.operation, BOOT_NS);
                Step {
                    kind: STEP_DONE,
                    outcome: Outcome {
                        value: boots.values.pop_front().unwrap(),
                        ..Outcome::default()
                    },
                    deadline_ns: 0,
                }
            }
            Provider {
                state: (self as *mut Self).cast(),
                wait: crate::provider::immediate::wait,
                issue,
                resume: crate::provider::immediate::resume,
                abort: crate::provider::immediate::abort,
                deliver: crate::provider::deliver,
            }
        }
    }

    let mut store = Box::<Store>::default();
    let mut boots = Boots {
        values: [41, 58].into(),
    };
    let (tape, _) = unsafe {
        Tape::open(
            REVISION,
            store.dir(),
            b"t",
            Mode::Record,
            &header(REVISION),
            &expect(),
        )
    }
    .unwrap();
    let clock = tape
        .on_tape(crate::entry::CLOCK, (boots.provider(), 0))
        .unwrap();
    let recorded = [plain(clock, BOOT_NS, 0, &[]), plain(clock, BOOT_NS, 0, &[])];
    tape.end(&Ending::returned(&[])).unwrap();
    let readings: Vec<_> = recorded
        .iter()
        .map(|o| (o.value, o.error, o.status))
        .collect();
    assert_eq!(readings, [(41, IoError::OK, 0), (58, IoError::OK, 0)]);

    let bytes = store.file().to_vec();
    let mut replay_store = Store::with(&bytes);
    let host = Counted::new(None);
    let (tape, _) = unsafe {
        Tape::open(
            REVISION,
            replay_store.dir(),
            b"t",
            Mode::Replay,
            &header(REVISION),
            &expect(),
        )
    }
    .unwrap();
    let clock = tape
        .on_tape(crate::entry::CLOCK, (host.provider(), 0))
        .unwrap();
    let replayed = [plain(clock, BOOT_NS, 0, &[]), plain(clock, BOOT_NS, 0, &[])];
    assert_eq!(replayed, recorded);
    assert_eq!(tape.end(&Ending::returned(&[])), Ok(()));
    assert_eq!(
        host.calls.get(),
        0,
        "a boot read reached the host under Replay"
    );
}
