use std::time::Duration;

use iroh::{Endpoint, endpoint::presets};

const ALPN: &[u8] = b"hirsel/viability/1";

#[tokio::test]
async fn endpoints_connect_by_node_id_and_round_trip_a_bi_stream() -> anyhow::Result<()> {
    let server = Endpoint::builder(presets::N0)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;
    tokio::time::timeout(Duration::from_secs(30), server.online()).await?;
    let server_id = server.id();

    let accepting = tokio::spawn({
        let server = server.clone();
        async move {
            let incoming = server
                .accept()
                .await
                .ok_or_else(|| anyhow::anyhow!("server endpoint closed"))?;
            let connection = incoming.accept()?.await?;
            let (mut send, mut recv) = connection.accept_bi().await?;
            let request = recv.read_to_end(64).await?;
            anyhow::ensure!(request == b"iroh viability");
            send.write_all(b"iroh viable").await?;
            send.finish()?;
            connection.closed().await;
            anyhow::Ok(())
        }
    });

    let client = Endpoint::bind(presets::N0).await?;
    let response = tokio::time::timeout(Duration::from_secs(30), async {
        let connection = client.connect(server_id, ALPN).await?;
        let (mut send, mut recv) = connection.open_bi().await?;
        send.write_all(b"iroh viability").await?;
        send.finish()?;
        let response = recv.read_to_end(64).await?;
        connection.close(0u32.into(), b"viability complete");
        anyhow::Ok(response)
    })
    .await??;

    assert_eq!(response, b"iroh viable");
    accepting.await??;
    client.close().await;
    server.close().await;
    Ok(())
}
