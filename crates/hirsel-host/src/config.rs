use std::{
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
};

use anyhow::{Context, anyhow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverMode {
    Real,
    Fake,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub token: String,
    pub anthropic_api_key: Option<String>,
    pub model: String,
    pub data_dir: PathBuf,
    pub driver: DriverMode,
    pub fake_fixture: Option<PathBuf>,
    pub listen: SocketAddr,
    pub debug: bool,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let token = env::var("HIRSEL_TOKEN")
            .context("HIRSEL_TOKEN is required for the Owner WebSocket protocol")?;
        let anthropic_api_key = env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|value| !value.is_empty());
        let model = env::var("HIRSEL_MODEL").unwrap_or_else(|_| "claude-opus-4-8".to_string());
        let data_dir = env::var_os("HIRSEL_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("./data"));
        let driver = match env::var("HIRSEL_DRIVER")
            .unwrap_or_else(|_| "real".to_string())
            .as_str()
        {
            "real" => DriverMode::Real,
            "fake" => DriverMode::Fake,
            other => return Err(anyhow!("HIRSEL_DRIVER must be real or fake, got {other}")),
        };
        let fake_fixture = env::var_os("HIRSEL_FAKE_FIXTURE").map(PathBuf::from);
        let mut listen: SocketAddr = env::var("HIRSEL_LISTEN")
            .unwrap_or_else(|_| "127.0.0.1:8420".to_string())
            .parse()
            .context("HIRSEL_LISTEN must be a socket address")?;
        let debug = env::var("HIRSEL_DEBUG").ok().as_deref() == Some("1");
        if debug && !listen.ip().is_loopback() {
            listen = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), listen.port());
        }
        Ok(Self {
            token,
            anthropic_api_key,
            model,
            data_dir,
            driver,
            fake_fixture,
            listen,
            debug,
        })
    }
}
