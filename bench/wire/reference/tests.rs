use super::*;
fn stream(n: u64) -> StreamId {
    StreamId { t: 0, n }
}
fn calls(principal: Principal) -> Arc<Calls> {
    Arc::new(Calls::new(
        principal,
        vec![Some(0)],
        Limits::benchmark(),
        Arc::new(Mutex::new(HashMap::new())),
    ))
}

#[test]
fn rows_allowance_keeps_the_v211_refusal_reserve() {
    fn send(count: u32) -> Output {
        let (tx, mut rx) = mpsc::channel(1);
        let mut peer = Peer {
            principal: [0; 16],
            slots: vec![],
            subs: HashSet::from([1, 2]),
            tx,
            pending: None,
            calls: calls([0; 16]),
            overrun: false,
        };
        let image = vec![0; IMAGE_BYTES];
        let mut groups = Vec::new();
        for sub in 0..count.div_ceil(CHUNKS) {
            let rows = (sub * CHUNKS..((sub + 1) * CHUNKS).min(count))
                .map(|i| codec::chunk_row(i % CHUNKS, &image))
                .collect();
            groups.push(Group::board(sub + 1, rows));
        }
        let done = vec![Done {
            stream: stream(0),
            p: 0,
            outcome: Outcome::Returned(vec![0, 0, 0, 0, 1]),
        }];
        assert!(matches!(
            peer.calls
                .admit(stream(0), 0, 0, 30000, &0u32.to_le_bytes()),
            Admission::Forward
        ));
        assert!(peer.send(Frame::Batch(1, vec![1], groups, done)));
        rx.try_recv().unwrap()
    }
    // At 90 B per row, 2,455 fit under the envelope and two group headers.
    assert!(matches!(
        send(2455),
        Output::Frame(Frame::Batch(1, _, _, _))
    ));
    let Output::Frame(Frame::Reset(1, done)) = send(2456) else {
        panic!("must retain the V1 refusal reserve");
    };
    assert_eq!(done[0].outcome, Outcome::Returned(vec![0, 0, 0, 0, 1]));
    assert_eq!(crate::DONE_MAX, 8192);
    assert_eq!(crate::REPLY_MAX, 8172);
}
#[test]
fn failed_group_sync_publishes_neither_rows_nor_outcomes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut wal = Wal::open(&dir.path().join("wal"))?;
    wal.fail_sync = true;
    let (tx, mut rx) = mpsc::channel(1);
    let mut head = Head {
        wal,
        peers: HashMap::from([(
            1,
            Peer {
                principal: [3; 16],
                slots: vec![Some(0)],
                subs: HashSet::from([1]),
                tx,
                pending: None,
                calls: calls([3; 16]),
                overrun: false,
            },
        )]),
        board_runs: 1,
        groups: 0,
    };
    assert!(matches!(
        head.peers[&1]
            .calls
            .admit(stream(0), 0, 0, 30000, &0u32.to_le_bytes()),
        Admission::Forward
    ));
    assert!(
        head.execute(vec![Command::Frame(
            1,
            Frame::Call(stream(0), 0, 0, 30_000, 0u32.to_le_bytes().to_vec())
        )])
        .is_err()
    );
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    Ok(())
}
#[test]
fn wal_recovery_replays_identity_and_truncates_torn_tail() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("wal");
    let mut wal = Wal::open(&path)?;
    let (o, fresh) = wal.toggle([7; 16], stream(3), 0, 999_999)?;
    assert!(fresh);
    assert_eq!(o, Outcome::Returned(vec![0, 0, 0, 0, 1]));
    wal.file.sync_data()?;
    drop(wal);
    OpenOptions::new()
        .append(true)
        .open(&path)?
        .write_all(&[7; 10])?;
    let mut wal = Wal::open(&path)?;
    assert_eq!(wal.bitmap[124999], 128);
    assert_eq!(wal.version, 1);
    assert_eq!(wal.toggle([7; 16], stream(3), 0, 999_999)?, (o, false));
    assert_eq!(
        wal.toggle([7; 16], stream(3), 0, 0)?,
        (Outcome::Refused(Refusal::RequestMismatch), false)
    );
    assert_eq!(wal.version, 1);
    assert_eq!(wal.file.metadata()?.len(), 8 + RECORD as u64);
    assert!(wal.toggle([7; 16], stream(3), 1, 0)?.1);
    assert_eq!(
        wal.toggle([7; 16], stream(3), 0, 999_999)?,
        (Outcome::Refused(Refusal::HistoryUnavailable), false)
    );
    Ok(())
}
#[test]
fn slow_reader_coalesces_latest_chunk_without_losing_outcomes() {
    let (tx, mut rx) = mpsc::channel(1);
    let mut p = Peer {
        principal: [0; 16],
        slots: vec![],
        subs: HashSet::new(),
        tx,
        pending: None,
        calls: calls([0; 16]),
        overrun: false,
    };
    for n in 1..=3 {
        let row = Row {
            slot: 0,
            key: vec![0],
            value: Some(vec![n as u8]),
        };
        assert!(matches!(
            p.calls.admit(stream(1), n, 0, 30000, &0u32.to_le_bytes()),
            Admission::Forward
        ));
        assert!(p.send(Frame::Batch(
            n,
            vec![],
            vec![Group::board(1, vec![row])],
            vec![Done {
                stream: stream(1),
                p: n,
                outcome: Outcome::Returned(vec![1])
            }]
        )));
    }
    assert!(matches!(
        rx.try_recv().unwrap(),
        Output::Frame(Frame::Batch(1, _, _, _))
    ));
    assert!(p.flush());
    let Output::Frame(Frame::Batch(v, _, rows, done)) = rx.try_recv().unwrap() else {
        panic!()
    };
    assert_eq!(v, 3);
    assert_eq!(rows[0].rows[0].value, Some(vec![3]));
    assert_eq!(done.iter().map(|d| d.p).collect::<Vec<_>>(), [2, 3]);
    let Some(Frame::Reset(version, kept)) = coalesce(
        Frame::Batch(3, vec![], vec![], done.clone()),
        Frame::Reset(4, vec![]),
    ) else {
        panic!("Reset must retain pending outcomes");
    };
    assert_eq!(version, 4);
    assert_eq!(kept, done);
}
