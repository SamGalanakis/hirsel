use std::{env, fs, path::PathBuf, process, time::SystemTime};

use anyhow::{Context, bail};
use image::{ImageFormat, Luma};
use qrcode::{QrCode, render::unicode};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Serialize)]
struct PairRequest<'a> {
    device_label: &'a str,
}

#[derive(Deserialize)]
struct PairResponse {
    ticket: String,
    code: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let host_url = required_env("HIRSEL_HOST_URL")?;
    let token = required_env("HIRSEL_TOKEN")?;
    let device_label = env::args()
        .nth(1)
        .unwrap_or_else(|| "Owner phone".to_string());
    if device_label.trim().is_empty() {
        bail!("device label must not be empty");
    }

    let endpoint = pair_endpoint(&host_url)?;
    let pair = reqwest::Client::new()
        .post(endpoint.clone())
        .bearer_auth(token)
        .json(&PairRequest {
            device_label: device_label.trim(),
        })
        .send()
        .await
        .with_context(|| format!("request pairing code from {endpoint}"))?
        .error_for_status()
        .with_context(|| format!("request pairing code from {endpoint}"))?
        .json::<PairResponse>()
        .await
        .context("decode /debug/pair response")?;

    let uri = pairing_uri(&pair.ticket, &pair.code)?;
    let code = QrCode::new(uri.as_bytes()).context("encode pairing URI as QR")?;
    let terminal = code.render::<unicode::Dense1x2>().build();
    let terminal_rows = terminal.lines().count();
    let terminal_columns = terminal
        .lines()
        .map(str::chars)
        .map(Iterator::count)
        .max()
        .unwrap_or(0);

    let png = code.render::<Luma<u8>>().module_dimensions(8, 8).build();
    let png_path = png_path()?;
    png.save_with_format(&png_path, ImageFormat::Png)
        .with_context(|| format!("write QR PNG to {}", png_path.display()))?;
    let png_bytes = fs::metadata(&png_path)
        .with_context(|| format!("inspect QR PNG at {}", png_path.display()))?
        .len();

    println!("Pairing URI: {uri}");
    println!("Terminal QR: {terminal_columns} columns x {terminal_rows} rows");
    for line in terminal.lines() {
        println!("\x1b[30;47m{line}\x1b[0m");
    }
    println!(
        "PNG: {} ({}x{} pixels, {png_bytes} bytes)",
        png_path.display(),
        png.width(),
        png.height()
    );

    Ok(())
}

fn required_env(name: &str) -> anyhow::Result<String> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => bail!("{name} must be set and non-empty"),
    }
}

fn pair_endpoint(host_url: &str) -> anyhow::Result<Url> {
    let mut url = Url::parse(host_url).context("HIRSEL_HOST_URL must be an absolute URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("HIRSEL_HOST_URL must use http or https");
    }
    url.set_path("/debug/pair");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn pairing_uri(ticket: &str, code: &str) -> anyhow::Result<String> {
    let mut uri = Url::parse("hirsel://pair").context("parse pairing URI base")?;
    uri.query_pairs_mut()
        .append_pair("ticket", ticket)
        .append_pair("code", code);
    Ok(uri.into())
}

fn png_path() -> anyhow::Result<PathBuf> {
    let timestamp = SystemTime::UNIX_EPOCH
        .elapsed()
        .context("system clock is before the Unix epoch")?
        .as_millis();
    Ok(env::temp_dir().join(format!("hirsel-pair-{timestamp}-{}.png", process::id())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_uri_encodes_both_query_values() {
        let uri = pairing_uri("ticket/with?reserved", "code with + symbols").unwrap();
        let parsed = Url::parse(&uri).unwrap();
        assert_eq!(parsed.scheme(), "hirsel");
        assert_eq!(parsed.host_str(), Some("pair"));
        assert_eq!(
            parsed.query_pairs().collect::<Vec<_>>(),
            vec![
                ("ticket".into(), "ticket/with?reserved".into()),
                ("code".into(), "code with + symbols".into())
            ]
        );
    }

    #[test]
    fn pair_endpoint_replaces_any_base_path_and_query() {
        assert_eq!(
            pair_endpoint("https://hirsel.example/old?ignored=yes")
                .unwrap()
                .as_str(),
            "https://hirsel.example/debug/pair"
        );
    }
}
