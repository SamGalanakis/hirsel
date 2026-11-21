use super::*;
fn address() -> Address {
    Address {
        lo: 0x7f000001,
        ..Address::default()
    }
}
fn pair(m: &Model) -> (u64, u64, u64) {
    let l = m
        .attempt(Operation::Listen(address(), 1), &[], 0, 1)
        .unwrap();
    assert_eq!(l.error, 0);
    let c = m.attempt(Operation::Connect(l.address), &[], 0, 2).unwrap();
    assert_eq!(c.error, 0);
    let s = m.attempt(Operation::Accept(l.value), &[], 0, 3).unwrap();
    assert_eq!(s.address.lo, 0x7f000001);
    assert_ne!(s.address.port, 0);
    (l.value, c.value, s.value)
}
fn close(m: &Model, handles: [u64; 3]) {
    m.repair();
    for key in handles {
        m.release(key);
    }
    assert_eq!(m.live_handles(), 0);
    assert_eq!(m.pending(), 0);
}
fn put(m: &Model, key: u64, bytes: &[u8]) {
    let mut at = 0;
    while at < bytes.len() {
        let r = m
            .attempt(Operation::Send(key), &bytes[at..], 0, 10)
            .unwrap();
        assert_eq!(r.error, 0);
        assert!(r.value > 0);
        at += r.value as usize;
    }
}
#[test]
fn partition_backpressure_repairs_and_drains_fifo_in_both_directions() {
    let m = Model::new(Config {
        queue_bytes: 4,
        ..Config::default()
    });
    let (l, c, s) = pair(&m);
    m.fault(Fault::Partition {
        socket: c,
        blocked: true,
    });
    put(&m, c, b"abcd");
    assert_eq!(m.queued_bytes(c), [4, 0, 0]);
    assert!(m.attempt(Operation::Send(c), b"e", 0, 11).is_none());
    assert!(m.attempt(Operation::Recv(s), &[], 4, 12).is_none());
    // The other direction is live during a directional partition.
    put(&m, s, b"xy");
    let mut reverse = vec![];
    while reverse.len() < 2 {
        reverse.extend(m.attempt(Operation::Recv(c), &[], 4, 13).unwrap().bytes);
    }
    assert_eq!(reverse, b"xy");
    m.repair();
    assert_eq!(m.poll(None, true), vec![11, 12]);
    put(&m, c, b"e");
    let (_, w) = m.split(c);
    m.release(w);
    let mut received = vec![];
    loop {
        let r = m.attempt(Operation::Recv(s), &[], 3, 12).unwrap();
        assert_eq!(r.error, 0);
        if r.value == 0 {
            break;
        }
        received.extend(r.bytes);
    }
    assert_eq!(received, b"abcde");
    m.release(c | 1);
    m.release(s);
    m.release(l);
    assert_eq!((m.live_handles(), m.pending()), (0, 0));
}
#[test]
fn reset_delivers_readable_bytes_and_drops_only_undelivered_bytes() {
    let m = Model::new(Config {
        latency_ns: 1,
        ..Config::default()
    });
    let (l, c, s) = pair(&m);
    put(&m, c, b"a");
    assert_eq!(m.queued_bytes(c), [1, 0, 0]);
    m.poll(None, true);
    assert_eq!(m.queued_bytes(c), [0, 1, 0]);
    m.poll(None, true);
    assert_eq!(m.queued_bytes(c), [0, 0, 1]);
    put(&m, c, b"b");
    m.poll(None, true);
    put(&m, c, b"c");
    assert_eq!(m.queued_bytes(c), [1, 1, 1]);
    m.fault(Fault::Reset { socket: c });
    assert_eq!(m.queued_bytes(c), [0, 0, 1]);
    assert_eq!(m.attempt(Operation::Send(s), b"z", 0, 4).unwrap().error, 5);
    let read = m.attempt(Operation::Recv(s), &[], 8, 5).unwrap();
    assert_eq!((read.error, read.bytes), (0, b"a".to_vec()));
    assert_eq!(m.attempt(Operation::Recv(s), &[], 8, 6).unwrap().error, 5);
    assert_eq!(m.attempt(Operation::Recv(c), &[], 8, 7).unwrap().error, 5);
    close(&m, [l, c, s]);
}
#[test]
fn time_enables_silent_deadline_and_readiness_wins_its_tie() {
    let m = Model::default();
    let (l, c, s) = pair(&m);
    assert!(m.attempt(Operation::Recv(s), &[], 1, 4).is_none());
    assert!(m.poll(Some(100), true).is_empty());
    assert_eq!(m.now(), 100);
    assert_eq!(m.abort(4, false).error, 6);
    m.fault(Fault::Partition {
        socket: c,
        blocked: true,
    });
    put(&m, c, b"a");
    assert!(m.attempt(Operation::Recv(s), &[], 1, 5).is_none());
    m.schedule(
        200,
        Fault::Partition {
            socket: c,
            blocked: false,
        },
    );
    assert_eq!(m.poll(Some(200), true), vec![5]);
    assert_eq!(m.now(), 200);
    assert_eq!(
        m.attempt(Operation::Recv(s), &[], 1, 5).unwrap().bytes,
        b"a"
    );
    close(&m, [l, c, s]);
}
#[test]
fn aborted_or_expired_waits_preserve_listener_bytes_and_send_prefix() {
    for cancelled in [false, true] {
        let m = Model::new(Config {
            queue_bytes: 1,
            ..Config::default()
        });
        let abort = |token| {
            if !cancelled {
                let at = m.now() + 1;
                assert!(m.poll(Some(at), true).is_empty());
                assert_eq!(m.now(), at);
            }
            m.abort(token, cancelled)
        };
        let signal = if cancelled { 7 } else { 6 };
        let l = m
            .attempt(Operation::Listen(address(), 1), &[], 0, 1)
            .unwrap();
        assert!(m.attempt(Operation::Accept(l.value), &[], 0, 2).is_none());
        assert_eq!(abort(2).error, signal);
        let c = m
            .attempt(Operation::Connect(l.address), &[], 0, 3)
            .unwrap()
            .value;
        // Backlog full: a cancelled connect takes no pending connection.
        assert!(
            m.attempt(Operation::Connect(l.address), &[], 0, 4)
                .is_none()
        );
        assert_eq!(abort(4).error, signal);
        let s = m
            .attempt(Operation::Accept(l.value), &[], 0, 5)
            .unwrap()
            .value;
        assert!(m.attempt(Operation::Recv(s), &[], 1, 6).is_none());
        assert_eq!(abort(6).error, signal);
        m.fault(Fault::Partition {
            socket: c,
            blocked: true,
        });
        put(&m, c, b"a");
        assert!(m.attempt(Operation::Send(c), b"b", 0, 7).is_none());
        assert_eq!(abort(7).error, signal);
        m.repair();
        assert_eq!(
            m.attempt(Operation::Recv(s), &[], 1, 8).unwrap().bytes,
            b"a"
        );
        assert_eq!(m.attempt(Operation::Recv(s), &[], 0, 8).unwrap().value, 0);
        assert_eq!(m.attempt(Operation::Send(c), &[], 0, 8).unwrap().value, 0);
        let (_, w) = m.split(c);
        m.release(w);
        assert_eq!(m.attempt(Operation::Recv(s), &[], 1, 8).unwrap().value, 0);
        // Read EOF leaves the reverse send direction usable.
        put(&m, s, b"z");
        assert_eq!(
            m.attempt(Operation::Recv(c), &[], 1, 9).unwrap().bytes,
            b"z"
        );
        m.release(c | 1);
        m.release(s);
        m.release(l.value);
        assert_eq!((m.live_handles(), m.pending()), (0, 0));
    }
}
/// Reaching a timeout creates a second fault choice absent in the baseline.
fn retry_scenario(
    prefix: BTreeMap<Choice, Vec<usize>>,
    seed: u64,
) -> (BTreeMap<Choice, Vec<Decision>>, usize) {
    let m = Model::new(Config {
        prefix,
        seed,
        ..Config::default()
    });
    let (l, c, s) = pair(&m);
    let mut attempts = 0;
    for retry in 0..2 {
        attempts += 1;
        if m.choose(Choice::Fault, 2) == 0 {
            break;
        }
        m.fault(Fault::Partition {
            socket: c,
            blocked: true,
        });
        if retry == 0 {
            put(&m, c, b"a");
        }
        assert!(m.attempt(Operation::Recv(s), &[], 1, 10).is_none());
        let at = m.now() + 10;
        m.poll(Some(at), true);
        assert_eq!(m.now(), at);
        assert_eq!(m.abort(10, false).error, 6);
    }
    m.repair();
    // Drain or signal every session even when both fault choices fired.
    let (_, w) = m.split(c);
    m.release(w);
    let mut bytes = vec![];
    loop {
        let r = m.attempt(Operation::Recv(s), &[], 8, 11).unwrap();
        if r.value == 0 {
            break;
        }
        bytes.extend(r.bytes);
    }
    assert!(bytes.is_empty() || bytes == b"a");
    m.release(c | 1);
    m.release(s);
    m.release(l);
    assert_eq!((m.pending(), m.live_handles()), (0, 0));
    (m.decisions(), attempts)
}
fn exact_prefixes(trace: &BTreeMap<Choice, Vec<Decision>>) -> BTreeMap<Choice, Vec<usize>> {
    trace
        .iter()
        .map(|(&kind, decisions)| (kind, decisions.iter().map(|d| d.selected).collect()))
        .collect()
}

#[test]
fn bounded_choice_prefixes_reach_fault_inside_fault_created_retry_and_replay() {
    // Dynamic prefix enumeration, adapted from TigerBeetle exhaustigen.zig.
    let mut prefix = vec![0];
    let mut explored = 0;
    let mut retry_fault = false;
    let mut complete = false;
    for _ in 0..128 {
        let (decisions, retries) =
            retry_scenario(BTreeMap::from([(Choice::Fault, prefix.clone())]), 1);
        assert_eq!(
            retry_scenario(exact_prefixes(&decisions), 999),
            (decisions.clone(), retries)
        );
        // Only the fault branches have multiple alternatives in this scenario.
        let trace = &decisions[&Choice::Fault];
        retry_fault |= retries == 2 && trace.iter().filter(|d| d.selected == 1).count() == 2;
        explored += 1;
        let Some(at) = (0..trace.len())
            .rev()
            .find(|&i| trace[i].selected + 1 < trace[i].alternatives)
        else {
            complete = true;
            break;
        };
        prefix = trace[..=at].iter().map(|d| d.selected).collect();
        prefix[at] += 1;
    }
    eprintln!(
        "fault-prefix runs={explored}, complete={complete}; bound=128; baseline-only cuts are partial"
    );
    assert!(complete, "bounded exploration incomplete");
    assert!(retry_fault);
}
#[test]
fn seeded_multifault_runs_repair_drain_and_replay_exact_choices() {
    for seed in 0..32 {
        let (trace, attempts) = retry_scenario(BTreeMap::new(), seed);
        assert_eq!(
            retry_scenario(exact_prefixes(&trace), seed ^ 1234),
            (trace, attempts)
        );
    }
}

#[test]
fn run_registry_releases_unsplit_and_remaining_directional_owners() {
    for released in [None, Some(false), Some(true)] {
        let model = Model::default();
        let (l, c, s) = pair(&model);
        {
            let mut sockets = crate::Sockets::with_model(model.clone());
            for key in [l, c, s] {
                sockets.adopt_model(&model, key);
            }
            if let Some(write) = released {
                let (r, w) = sockets.split(c);
                sockets.release(if write { w } else { r });
            }
            assert_eq!(sockets.len(), 3);
        }
        assert_eq!((model.live_handles(), model.pending()), (0, 0));
    }
}

#[test]
fn model_identities_never_alias_native_generations_or_wrap_onto_live_handles() {
    let model = Model::default();
    model.state().next = LAST_ID;
    let l = model
        .attempt(Operation::Listen(address(), 1), &[], 0, 1)
        .unwrap();
    assert_eq!(l.error, 0);
    assert!(l.value < 1 << 34);
    assert!(Model::owns(l.value));
    assert!(!Model::owns(1 << 34));
    assert!(!Model::owns((1 << 63) | 4));
    let refused = model
        .attempt(Operation::Connect(l.address), &[], 0, 2)
        .unwrap();
    assert_eq!((refused.error, refused.code), (8, 24));
    assert_eq!(model.live_handles(), 1);
    model.release(l.value);
    assert_eq!((model.live_handles(), model.pending()), (0, 0));
}

#[test]
fn shrinking_fault_prefix_preserves_schedule_decisions() {
    let run = |faults: Vec<usize>, expected_error| {
        let m = Model::new(Config {
            prefix: BTreeMap::from([(Choice::Schedule, vec![1, 0, 1]), (Choice::Fault, faults)]),
            // This seed chooses no fault on the third send with an empty prefix.
            seed: 1,
            ..Config::default()
        });
        let (l, c, s) = pair(&m);
        let mut errors = Vec::new();
        for send in 0..3 {
            m.choose(Choice::Schedule, 2);
            if m.choose(Choice::Fault, 2) == 1 {
                m.fault(Fault::Reset { socket: c });
            }
            errors.push(
                m.attempt(Operation::Send(c), b"x", 0, 10 + send)
                    .unwrap()
                    .error,
            );
        }
        assert_eq!(errors, [0, 0, expected_error]);
        m.choose(Choice::Schedule, 256);
        close(&m, [l, c, s]);
        m.decisions().remove(&Choice::Schedule).unwrap()
    };

    let faulted = run(vec![0, 0, 1], 5);
    let shrunk = run(vec![], 0);
    assert_eq!(
        faulted[..3].iter().map(|d| d.selected).collect::<Vec<_>>(),
        [1, 0, 1]
    );
    assert_eq!(shrunk, faulted);
}

#[test]
fn repair_stops_new_faults_and_drains_queues() {
    let m = Model::new(Config {
        prefix: BTreeMap::from([(Choice::Fault, vec![1])]),
        ..Config::default()
    });
    let (l, c, s) = pair(&m);
    m.schedule(
        5,
        Fault::Partition {
            socket: c,
            blocked: true,
        },
    );
    m.schedule(50, Fault::Reset { socket: c });
    m.advance_to(5);
    put(&m, c, b"x");
    assert_eq!(m.queued_bytes(c), [1, 0, 0]);
    assert!(m.attempt(Operation::Recv(s), &[], 1, 12).is_none());

    m.advance_to(10);
    m.repair();
    assert_eq!(m.choose(Choice::Fault, 2), 0);
    assert!(!m.decisions().contains_key(&Choice::Fault));
    m.schedule(51, Fault::Reset { socket: c });
    m.fault(Fault::Reset { socket: c });
    m.advance_to(51);
    assert_eq!(m.poll(None, false), [12]);
    assert_eq!(
        m.attempt(Operation::Recv(s), &[], 1, 12).unwrap().bytes,
        b"x"
    );
    assert_eq!(m.queued_bytes(c), [0; 3]);
    assert_eq!(m.pending(), 0);
    // Successful sends distinguish a healed connection from a reset with
    // readable bytes still buffered.
    put(&m, c, b"y");
    assert_eq!(
        m.attempt(Operation::Recv(s), &[], 1, 13).unwrap().bytes,
        b"y"
    );
    assert_eq!(m.queued_bytes(c), [0; 3]);
    close(&m, [l, c, s]);
}

#[test]
fn omitting_the_trace_preserves_prefix_and_seed_choices() {
    let recorded = Model::new(Config {
        prefix: BTreeMap::from([(Choice::Send, vec![0; 19]), (Choice::Receive, vec![0; 19])]),
        seed: 73,
        ..Config::default()
    });
    let unrecorded = Model::new(Config {
        prefix: BTreeMap::from([(Choice::Send, vec![0; 19]), (Choice::Receive, vec![0; 19])]),
        seed: 73,
        record_decisions: false,
        ..Config::default()
    });
    let kinds = [
        Choice::Send,
        Choice::Receive,
        Choice::Delivery,
        Choice::Fault,
    ];
    for i in 0..10000 {
        let kind = kinds[i % kinds.len()];
        let alternatives = 1 + i % 127;
        assert_eq!(
            recorded.choose(kind, alternatives),
            unrecorded.choose(kind, alternatives)
        );
    }
    assert_eq!(
        recorded.decisions().values().map(Vec::len).sum::<usize>(),
        10000
    );
    assert!(unrecorded.decisions().is_empty());
    // The setting also preserves actual short IO, not just harness choices.
    let a = pair(&recorded);
    let b = pair(&unrecorded);
    assert_eq!(a, b);
    let mut sent = 0;
    let mut received = vec![];
    let input = b"one complete message over many bounded short IO attempts";
    while received.len() < input.len() {
        if sent < input.len() {
            let left = recorded.attempt_parts(
                Operation::Send(a.1),
                [&input[sent..sent + 1], &input[sent + 1..]],
                0,
                10,
            );
            let right = unrecorded.attempt(Operation::Send(b.1), &input[sent..], 0, 10);
            assert_eq!(left, right);
            if let Some(reply) = left {
                sent += reply.value as usize;
            }
        }
        assert_eq!(recorded.poll(None, true), unrecorded.poll(None, true));
        let left = recorded.attempt(Operation::Recv(a.2), &[], 13, 11);
        let right = unrecorded.attempt(Operation::Recv(b.2), &[], 13, 11);
        assert_eq!(left, right);
        if let Some(reply) = left {
            received.extend(reply.bytes);
        }
        assert_eq!(recorded.now(), unrecorded.now());
    }
    assert_eq!(received, input);
    close(&recorded, [a.0, a.1, a.2]);
    close(&unrecorded, [b.0, b.1, b.2]);
    assert!(unrecorded.decisions().is_empty());
}

#[test]
fn starttls_refuses_unread_scripted_plaintext() {
    let model = Model::default();
    let address = address();
    let inject = b"S\x52\x00\x00\x00\x08\x00\x00\x00\x00";
    model.script_tls_peer(
        address,
        TlsScript {
            handshake: TlsHandshake::After {
                delay_ns: 0,
                result: Ok(TlsIdentity {
                    protocol: None,
                    peer: [0; 32],
                    binding: Default::default(),
                }),
            },
            steps: vec![TlsStep::BeforeHandshake {
                expect: Vec::new(),
                send: inject.to_vec(),
            }],
        },
    );
    let key = model
        .attempt(Operation::Connect(address), &[], 0, 1)
        .unwrap()
        .value;
    let (read, _) = model.split(key);
    let reply = model.attempt(Operation::Recv(read), &[], 1, 2).unwrap();
    assert_eq!(reply.bytes, b"S");
    assert!(model.rejoin_client(key));
    let reply = model.attempt_tls(key, &[] as &[&[u8]], 3).unwrap();
    assert_eq!((reply.error, reply.code), (9, 4));
    assert_eq!(model.queued_bytes(key), [0; 3]);
    model.release(key);
    assert_eq!((model.live_handles(), model.live_tls_sessions()), (0, 0));
}

#[test]
fn scram_peer_script_checks_the_exact_proof() {
    use crate::scram::{Binding, Error, Transcript, verify};

    const CLIENT: &[u8] = b"n,,n=user,r=rOprNGfwEbeRWgbNEkqO";
    const SERVER: &[u8] =
        b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
    const PROOF: &[u8] = b"c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=";
    const FINAL: &[u8] = b"v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=";
    let model = Model::default();
    let address = address();
    model.script_tls_peer(
        address,
        TlsScript {
            handshake: TlsHandshake::After {
                delay_ns: 0,
                result: Ok(TlsIdentity {
                    protocol: None,
                    peer: [0; 32],
                    binding: Binding::default(),
                }),
            },
            steps: vec![
                TlsStep::Expect(PROOF.to_vec()),
                TlsStep::Send(FINAL.to_vec()),
            ],
        },
    );
    let key = model
        .attempt(Operation::Connect(address), &[], 0, 1)
        .unwrap()
        .value;
    let secured = model.attempt_tls(key, &[] as &[&[u8]], 2).unwrap();
    assert_eq!(secured.error, 0);
    let (r, w) = model.split(key);
    let binding = secured.binding;
    for (client, server) in [
        (
            b"n,,n=,r=abc".as_slice(),
            b"r=xyz,s=c2FsdA==,i=4096".as_slice(),
        ),
        (b"n,,n=,r=abc", b"r=abc,s=c2FsdA==,i=4096"),
        (b"n,,n=,r=abc", b"m=required,r=abcd,s=c2FsdA==,i=4096"),
        (b"n,a=admin,n=,r=abc", b"r=abcd,s=c2FsdA==,i=4096"),
        (b"n,,n=,r=abc", b"r=abcd,s=invalid!,i=4096"),
    ] {
        assert!(matches!(
            Transcript::parse(client, server, &binding),
            Err(Error::Transcript)
        ));
        assert_eq!(model.queued_bytes(w), [0; 3]);
    }
    let transcript = Transcript::parse(CLIENT, SERVER, &binding).unwrap();
    assert_eq!(transcript.final_len(), 106);
    let (message, signature) = transcript.derive(b"pencil");
    let mut committed = 0;
    let parts = [b"".as_slice(), &message, b""];
    loop {
        if let Some(sent) = model.attempt_secret(w, parts, &mut committed, 3) {
            assert_eq!(sent.error, 0);
            break;
        }
        assert_eq!(model.poll(None, true), [3]);
    }
    let mut received = Vec::new();
    while received.len() < FINAL.len() {
        match model.attempt(Operation::Recv(r), &[], FINAL.len() - received.len(), 4) {
            Some(reply) => {
                assert_eq!(reply.error, 0);
                assert_ne!(reply.value, 0);
                received.extend(reply.bytes);
            }
            None => assert_eq!(model.poll(None, true), [4]),
        }
    }
    assert_eq!(received, FINAL);
    assert!(verify(&signature, &received));
    model.release(w);
    model.release(r);
}
