use hirsel_wire_bench::{Result, client, codec, load};

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mode = args
        .next()
        .ok_or("wire-load converge|run|memory-subscribed|memory-unsubscribed|echo [options]")?;
    if mode == "codec" {
        use std::io::{Read, Write};
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(hirsel_wire_bench::MAX_FRAME as u64 + 1)
            .read_to_end(&mut bytes)?;
        std::io::stdout().write_all(&codec::encode(&codec::decode(&bytes)?))?;
        return Ok(());
    }
    let mut url = "ws://127.0.0.1:7700/hirsel".to_owned();
    let mut wants = codec::checkbox_wants();
    let mut config = load::Config {
        clients: 1000,
        rate: 2000,
        seconds: 60,
    };
    let mut toggles = 1000;
    let mut pace = 10;
    let mut samples = 2000;
    let mut server_pid = None;
    let mut server_snapshots = None;
    while let Some(arg) = args.next() {
        let value = args.next().ok_or("missing option value")?;
        match arg.as_str() {
            "--url" => url = value,
            "--wants" => wants = codec::wants_file(std::path::Path::new(&value))?,
            "--clients" => config.clients = value.parse()?,
            "--rate" => config.rate = value.parse()?,
            "--seconds" => config.seconds = value.parse()?,
            "--toggles" => toggles = value.parse()?,
            "--pace" => pace = value.parse()?,
            "--samples" => samples = value.parse()?,
            "--server-pid" => server_pid = Some(value.parse()?),
            "--server-snapshots" => server_snapshots = Some(std::path::PathBuf::from(value)),
            _ => return Err(format!("unknown option {arg}").into()),
        }
    }
    if server_snapshots.is_some()
        && !matches!(mode.as_str(), "memory-subscribed" | "memory-unsubscribed")
    {
        return Err("--server-snapshots requires a memory mode".into());
    }
    match mode.as_str() {
        "echo" => println!("{}", load::echo(samples).await?),
        "run" => {
            let result = load::run(&url, &wants, config).await?;
            println!("{result}");
            if result["complete"] != true {
                return Err("load incomplete: missed arrivals or observations".into());
            }
        }
        "memory-subscribed" | "memory-unsubscribed" => {
            let pid = server_pid.ok_or("memory measurement needs --server-pid")?;
            let result = load::memory(
                &url,
                &wants,
                config.clients,
                mode == "memory-subscribed",
                pid,
                server_snapshots.as_deref(),
            )
            .await?;
            println!("{result}");
            if result["setup_error"].is_null() && result["drained"] == false {
                return Err("memory sample incomplete: held sessions did not all drain".into());
            }
            if result["complete"] != true {
                return Err("memory sample censored by session admission or early closure".into());
            }
        }
        "converge" => {
            let result = client::converge(&url, &wants, config.clients, toggles, pace).await?;
            println!(
                "Rust converge smoke PASS clients={} Returned={} reconnects={} repairs={} resubscribes={} version={} checksum={} required_server_overrides: per_ip_sessions=20000 per_ip_new_principals=20000/min; shared W4/W6 acceptance pending",
                result.clients,
                result.returned,
                result.reconnects,
                result.repairs,
                result.resubscribes,
                result.version,
                result.checksum
            );
        }
        _ => return Err("unknown wire-load mode".into()),
    }
    Ok(())
}
