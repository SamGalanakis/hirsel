use hirsel_wire_bench::codec::{self, Done, Entry, Frame, Outcome, Row};
use hirsel_wire_bench::{BITS, IMAGE_BYTES, Result, client, flip, reference};

#[test]
fn provisional_v2_byte_costs_match_the_hand_derived_spec_table() {
    fn wire(f: Frame) -> usize {
        let n = codec::encode(&f).len();
        n + if n <= 125 {
            2
        } else if n <= 65535 {
            4
        } else {
            10
        }
    }
    let image = vec![0; IMAGE_BYTES];
    let row = codec::chunk_row(1, 0, &image);
    assert_eq!(
        wire(Frame::Batch(1, vec![], vec![row.clone()], vec![])),
        118
    );
    assert_eq!(
        wire(Frame::Batch(
            1,
            vec![],
            vec![row],
            vec![Done {
                stream: 0,
                p: 0,
                outcome: Outcome::Returned(vec![0, 0, 0, 0, 1])
            }]
        )),
        145
    );
    assert_eq!(
        wire(Frame::Batch(
            1,
            vec![1],
            (0..1954).map(|c| codec::chunk_row(1, c, &image)).collect(),
            vec![]
        )),
        179806
    );
    assert_eq!(wire(Frame::Batch(0, vec![1], vec![], vec![])), 30);
    assert_eq!(
        wire(Frame::Welcome(vec![0; 21], 4, 1048576, vec![0; 32])),
        79
    );
}

#[test]
fn hand_derived_frame_vectors_and_strict_decode() -> Result<()> {
    fn bytes(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|h| u8::from_str_radix(h, 16).unwrap())
            .collect()
    }
    let cases = [
        (
            Frame::Hello(None, vec![]),
            "00 00 00 00 00 00 00 00 00 00 00 00",
        ),
        (
            Frame::Call(2, 3, 4, 5, vec![6]),
            "01 00 00 00 02 00 00 00 03 00 00 00 00 00 00 00 04 00 00 00 05 00 00 00 01 00 00 00 06",
        ),
        (
            Frame::Subscribe(2, 3, vec![4], Some(vec![5])),
            "02 00 00 00 02 00 00 00 03 00 00 00 01 00 00 00 04 01 00 00 00 01 00 00 00 05",
        ),
        (Frame::Unsubscribe(9), "03 00 00 00 09 00 00 00"),
        (
            Frame::Welcome(vec![1], 2, 3, vec![4]),
            "04 00 00 00 01 00 00 00 01 02 00 00 00 03 00 00 00 00 00 00 00 01 00 00 00 04",
        ),
        (
            Frame::Placed(Entry::Ready(7)),
            "05 00 00 00 00 00 00 00 07 00 00 00",
        ),
        (
            Frame::Batch(
                2,
                vec![3],
                vec![Row {
                    sub: 3,
                    key: vec![4],
                    value: None,
                }],
                vec![Done {
                    stream: 5,
                    p: 6,
                    outcome: Outcome::Returned(vec![1]),
                }],
            ),
            "06 00 00 00 02 00 00 00 00 00 00 00 01 00 00 00 03 00 00 00 01 00 00 00 03 00 00 00 01 00 00 00 04 00 00 00 00 01 00 00 00 05 00 00 00 06 00 00 00 00 00 00 00 00 00 00 00 01 00 00 00 01",
        ),
        (
            Frame::Ended(2, Outcome::Abandoned),
            "07 00 00 00 02 00 00 00 04 00 00 00",
        ),
        (
            Frame::Reset(3, vec![]),
            "08 00 00 00 03 00 00 00 00 00 00 00 00 00 00 00",
        ),
    ];
    for (frame, hex) in cases {
        let expected = bytes(hex);
        assert_eq!(codec::encode(&frame), expected);
        assert_eq!(codec::decode(&expected)?, frame);
        for end in 0..expected.len() {
            assert!(codec::decode(&expected[..end]).is_err());
        }
        let mut trailing = expected;
        trailing.push(0);
        assert!(codec::decode(&trailing).is_err());
    }
    assert!(codec::decode(&[255; 16]).is_err());
    assert!(codec::decode(&bytes("00 00 00 00 00 00 00 00 ff ff ff ff")).is_err());
    Ok(())
}

#[test]
fn bit_and_chunk_law_uses_key_codec_and_zero_tail() -> Result<()> {
    let mut image = vec![0; IMAGE_BYTES];
    for bit in [0, 7, 8, 511, 512, 999_999] {
        assert!(flip(&mut image, bit));
    }
    assert_eq!(image[0], 129);
    assert_eq!(image[1], 1);
    assert_eq!(image[63], 128);
    assert_eq!(image[64], 1);
    assert_eq!(image[124999], 128);
    let row = codec::chunk_row(1, 1953, &image);
    assert_eq!(row.key, [0, 0, 0, 0, 0, 0, 7, 161]);
    assert_eq!(row.value.as_ref().unwrap()[12..], [0; 56]);
    let mut copy = vec![0; IMAGE_BYTES];
    codec::apply_row(&mut copy, &row)?;
    assert_eq!(copy[124999], 128);
    assert!(
        codec::apply_row(
            &mut copy,
            &Row {
                key: BITS.to_be_bytes().to_vec(),
                ..row
            }
        )
        .is_err()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_session_and_new_principal_limits_refuse_at_distinct_boundaries() -> Result<()> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Error, Message};
    for sessions in [true, false] {
        let dir = tempfile::tempdir()?;
        let wal = dir.path().join("wal");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}/hirsel", listener.local_addr()?);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            reference::serve(
                listener,
                &wal,
                codec::checkbox_wants(),
                reference::Limits::default(),
                async {
                    let _ = rx.await;
                },
            )
            .await
        });
        let wants = codec::checkbox_wants();
        let mut clients = Vec::new();
        let first = client::Client::connect(&url, &wants, None).await?;
        let token = first.token.clone();
        clients.push(first);
        for _ in 1..if sessions { 32 } else { 10 } {
            clients.push(
                client::Client::connect(&url, &wants, sessions.then(|| token.clone())).await?,
            );
        }
        if sessions {
            let result = tokio_tungstenite::connect_async(&url).await;
            assert!(matches!(result, Err(Error::Http(response)) if response.status() == 503));
        } else {
            let (mut ws, _) = tokio_tungstenite::connect_async(&url).await?;
            ws.send(Message::Binary(
                codec::encode(&Frame::Hello(None, wants)).into(),
            ))
            .await?;
            assert!(
                matches!(ws.next().await.transpose()?, Some(Message::Close(Some(c))) if u16::from(c.code) == 1013)
            );
        }
        drop(clients);
        tx.send(()).unwrap();
        server.await??;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reference_converges_with_reconnect_and_recovered_state() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("wal");
    let wants = codec::checkbox_wants();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("ws://{}/hirsel", listener.local_addr()?);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let wal = path.clone();
    let entries = wants.clone();
    let server = tokio::spawn(async move {
        reference::serve(
            listener,
            &wal,
            entries,
            reference::Limits::benchmark(),
            async {
                let _ = rx.await;
            },
        )
        .await
    });
    let result = client::converge(&url, &wants, 8, 20, 1000).await?;
    assert_eq!(result.returned, 160);
    assert_eq!(result.reconnects, 8);
    let mut before = client::Client::connect(&url, &wants, None).await?;
    before.snapshot().await?;
    let saved = before.image.clone();
    let version = before.version;
    drop(before);
    tx.send(()).unwrap();
    let stats = server.await??;
    assert_eq!(stats.version, 160);
    assert!(stats.durable_groups > 0);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("ws://{}/hirsel", listener.local_addr()?);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        reference::serve(
            listener,
            &path,
            wants,
            reference::Limits::benchmark(),
            async {
                let _ = rx.await;
            },
        )
        .await
    });
    let mut after = client::Client::connect(&url, &codec::checkbox_wants(), None).await?;
    after.snapshot().await?;
    assert_eq!(after.version, version);
    assert_eq!(after.image, saved);
    drop(after);
    tx.send(()).unwrap();
    server.await??;
    Ok(())
}
