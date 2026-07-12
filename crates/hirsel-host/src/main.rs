use hirsel_host::{build_state, config::Config, iroh::IrohServer, router_from_state};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Install a process-default rustls CryptoProvider before any TLS is used.
    // The dep tree links both `ring` (iroh) and `aws-lc-rs` (reqwest), so rustls
    // cannot pick one automatically; without this, the first HTTPS request the
    // agent's model provider makes panics inside its task and the turn silently
    // produces no reply. aws-lc-rs is reqwest's default backend.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("install rustls CryptoProvider");

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hirsel_host=info,tower_http=info".into()),
        )
        .init();

    let config = Config::from_env()?;
    let listen = config.listen;
    let data_dir = config.data_dir.clone();
    let state = build_state(config).await?;
    let app = router_from_state(state.clone());
    let listener = TcpListener::bind(listen).await?;
    tracing::info!(%listen, "Hirsel Host listening");

    let _iroh_task = if iroh_enabled() {
        Some(tokio::spawn(async move {
            match IrohServer::start(state, data_dir).await {
                Ok(server) => {
                    std::future::pending::<()>().await;
                    drop(server);
                }
                Err(error) => {
                    tracing::warn!(
                        error = %format!("{error:#}"),
                        "failed to start optional iroh endpoint; continuing with WSS"
                    );
                }
            }
        }))
    } else {
        tracing::info!("iroh endpoint disabled by HIRSEL_IROH");
        None
    };

    axum::serve(listener, app).await?;
    Ok(())
}

fn iroh_enabled() -> bool {
    std::env::var("HIRSEL_IROH").map_or(true, |value| {
        value != "0" && !value.eq_ignore_ascii_case("false")
    })
}
