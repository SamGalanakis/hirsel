use super::*;
use crate::tasks::{TaskLayout, ValueLayout, hirsel_core_join, hirsel_core_spawn};
use hirsel_core_net::{
    agreement_registry::{AgreementRegistry, ShareTicket},
    key_agreement::{Group, KeyShare},
    tls_kdf::Side,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering::Relaxed},
};

struct Paid(Arc<AtomicUsize>);
impl Paid {
    fn admit(payer: &Arc<AtomicUsize>) -> Self {
        payer.fetch_add(48, Relaxed);
        Self(payer.clone())
    }
}
impl Drop for Paid {
    fn drop(&mut self) {
        self.0.fetch_sub(48, Relaxed);
    }
}

struct Constructor {
    share: Option<KeyShare>,
    pending: Option<(ShareTicket<Paid>, Paid)>,
    peer: [u8; 32],
    input: [u8; 66],
    decisions: usize,
    delivered: u64,
    payer: Arc<AtomicUsize>,
}

unsafe extern "C" fn construct(context: *mut crate::Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the parent loans this constructor exclusively through its join.
    let job = unsafe { &mut **input.cast::<*mut Constructor>() };
    let peer = job.peer;
    // SAFETY: the task pins its request and shares no writable operand.
    let answer = unsafe {
        crate::secret::agreement::decide(
            context,
            &job.input,
            Side::Client,
            &mut job.pending,
            || {
                job.decisions += 1;
                job.share.take().unwrap().agree(&peer)
            },
            7,
        )
    };
    job.delivered = answer.value;
    0
}

#[test]
fn root_virtual_agreement_verdicts_are_recorded_and_replayed() {
    let mut store = Store::default();
    let mut header = header(REVISION);
    header.kind = KIND_RUN;

    let prepare = |registry: &Arc<AgreementRegistry<Paid>>, ordinal, scalar, peer: [u8; 32]| {
        let share = KeyShare::from_draws(
            Group::X25519,
            |draw| {
                *draw = [scalar; 32];
                Ok::<_, ()>(())
            },
            &mut [0; 32],
        )
        .unwrap();
        let payer = Arc::new(AtomicUsize::new(0));
        let mut input = [0x5e; 66];
        input[..2].copy_from_slice(&[0, ordinal]); // TLS 1.3, client; share origin.
        input[2..34].copy_from_slice(&peer);
        let pending = Some((registry.new_share().unwrap(), Paid::admit(&payer)));
        Constructor {
            share: Some(share),
            pending,
            peer,
            input,
            decisions: 0,
            delivered: 0,
            payer,
        }
    };
    let public = |scalar| {
        let mut public = [0; 32];
        KeyShare::from_draws(
            Group::X25519,
            |draw| {
                *draw = [scalar; 32];
                Ok::<_, ()>(())
            },
            &mut public,
        )
        .unwrap();
        public
    };
    let [a, b] = [public(0x31), public(0x42)];

    let mut recorded = [0; 2];
    for mode in [Mode::Record, Mode::Replay] {
        in_task(false, &mut |context| {
            // SAFETY: this root pins the store and both jobs until the tape
            // ends and the constructor tasks are joined.
            unsafe {
                let (provider, handle) = store.dir();
                let dir = Inner { provider, handle };
                let input = match mode {
                    Mode::Record => TapeInput::Record {
                        dir,
                        name: b"t",
                        header: &header,
                    },
                    Mode::Replay => TapeInput::Replay(ReplayInput::open(dir, b"t").unwrap()),
                };
                let (tape, _) = Tape::open_with_startup(REVISION, input, &expect()).unwrap();
                (*(*context).host_files).secret_mac_tape = Some(tape.secret_mac());
                let registry = Arc::new(AgreementRegistry::default());
                let mut first = prepare(&registry, 0, 0x31, b);
                let mut second = prepare(&registry, 1, 0x42, a);
                let spawn = |mut job: *mut Constructor| {
                    let task = hirsel_core_spawn(
                        context,
                        0,
                        1 << 16,
                        (&raw mut job).cast(),
                        construct,
                        TaskLayout {
                            input: ValueLayout {
                                bytes: size_of::<*mut Constructor>(),
                                align: align_of::<*mut Constructor>(),
                                logical: 0,
                                clean: None,
                            },
                            result: ValueLayout {
                                bytes: 0,
                                align: 1,
                                logical: 0,
                                clean: None,
                            },
                        },
                        0,
                    )
                    .task;
                    assert!(!task.is_null());
                    task
                };
                let left = spawn(&mut first);
                let right = spawn(&mut second);
                hirsel_core_join(context, left, ptr::null_mut(), 0);
                hirsel_core_join(context, right, ptr::null_mut(), 0);
                let delivered = [first.delivered, second.delivered];
                let mut sorted = delivered;
                sorted.sort_unstable();
                assert_eq!(sorted, [1, 2]);
                for job in [&first, &second] {
                    assert!(job.pending.is_none());
                    assert_eq!(job.payer.load(Relaxed), 0);
                    assert_eq!(job.decisions, usize::from(mode == Mode::Record));
                }
                if mode == Mode::Record {
                    recorded = delivered;
                } else {
                    assert_eq!(delivered, recorded);
                }
                tape.end(&ending()).unwrap();
            }
        });
        if mode == Mode::Record {
            let observed = tape_frames(store.file())
                .into_iter()
                .filter_map(|frame| match frame {
                    Frame::Op {
                        op: MEDIA_AGREEMENT,
                        out,
                        ..
                    } => Some(out.value),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(observed, recorded);
        }
    }
}

#[path = "media_tests.rs"]
mod media_tests;
