use hirsel_wire_bench::{
    DONE_MAX, MAX_FRAME, OUTCOME_RESERVE, REPLY_MAX, Result, WS_MAX_WRITE_BUFFER, WS_READ_BUFFER,
    codec, reference,
};

fn main() -> Result<()> {
    // Keep the existing default and let the multicore benchmark select the width.
    let workers = match std::env::var("TOKIO_WORKER_THREADS") {
        Ok(value) => value.parse()?,
        Err(std::env::VarError::NotPresent) => 2,
        Err(error) => return Err(error.into()),
    };
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()?
        .block_on(serve())
}

async fn serve() -> Result<()> {
    let mut listen = "127.0.0.1:7700".to_owned();
    let mut wal = "checkbox.wal".to_owned();
    let mut wants = codec::checkbox_wants();
    let mut limits = reference::Limits::default();
    let mut target = format!("{}-unknown-linux-gnu", std::env::consts::ARCH);
    let mut n_place = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args
            .next()
            .ok_or("expected --listen ADDRESS, --wal PATH or --wants HELLO_FILE")?;
        match arg.as_str() {
            "--target" => target = value,
            "--n-place" => n_place = Some(value.parse()?),
            "--listen" => listen = value,
            "--wal" => wal = value,
            "--per-ip-sessions" => limits.per_ip_sessions = value.parse()?,
            "--principal-rate" => limits.principal_rate = value.parse()?,
            "--principal-burst" => limits.principal_burst = value.parse()?,
            "--wants" => wants = codec::wants_file(std::path::Path::new(&value))?,
            _ => return Err(format!("unknown argument {arg}").into()),
        }
    }
    let launch = reference::Launch {
        target,
        n_place: n_place.ok_or("--n-place BYTES must be supplied by the launcher")?,
    };
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!(
        "LISTEN {} limits={limits:?} max_frame={MAX_FRAME} done_max={DONE_MAX} reply_max={REPLY_MAX} outcome_reserve={OUTCOME_RESERVE} rows_allowance_one={} budget=1048576 read_buffer={WS_READ_BUFFER} write_buffer=0 max_write_buffer={WS_MAX_WRITE_BUFFER}",
        listener.local_addr()?,
        MAX_FRAME - OUTCOME_RESERVE - 28
    );
    let stats = reference::serve(
        listener,
        std::path::Path::new(&wal),
        wants,
        limits,
        launch,
        async {
            let _ = tokio::signal::ctrl_c().await;
        },
    )
    .await?;
    println!(
        "version={} board_runs={} fdatasync_groups={}",
        stats.version, stats.board_runs, stats.durable_groups
    );
    Ok(())
}
