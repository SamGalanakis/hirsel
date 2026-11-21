use hirsel_wire_bench::codec::{self, Row};
use hirsel_wire_bench::{BITS, IMAGE_BYTES, Result, client, flip, reference};

fn launch() -> reference::Launch {
    reference::Launch {
        target: "x86_64-unknown-linux-gnu".to_owned(),
        n_place: 1048576,
    }
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
async fn default_session_limit_refuses_excess_sockets() -> Result<()> {
    use tokio_tungstenite::tungstenite::Error;
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
            launch(),
            async {
                let _ = rx.await;
            },
        )
        .await
    });
    let wants = codec::checkbox_wants();
    let mut clients = Vec::new();
    for _ in 0..32 {
        clients.push(client::Client::connect(&url, &wants, None).await?);
    }
    let result = tokio_tungstenite::connect_async(&url).await;
    assert!(matches!(result, Err(Error::Http(response)) if response.status() == 503));
    drop(clients);
    tx.send(()).unwrap();
    server.await??;
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
            launch(),
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
            launch(),
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
