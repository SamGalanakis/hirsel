use hirsel_host::{build_state, config::Config, iroh::IrohServer, router_from_state};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
    let _iroh = IrohServer::start(state, data_dir).await?;
    let listener = TcpListener::bind(listen).await?;
    tracing::info!(%listen, "Hirsel Host listening");
    axum::serve(listener, app).await?;
    Ok(())
}
