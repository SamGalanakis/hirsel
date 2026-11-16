use hirsel_wire_bench::{Result, codec, reference};

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
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
            "--per-ip-new-principals" => limits.per_ip_new_principals = value.parse()?,
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
        "LISTEN {} limits={limits:?} max_frame=262144 done_max=8192 budget=1048576 read_buffer=4096",
        listener.local_addr()?
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
