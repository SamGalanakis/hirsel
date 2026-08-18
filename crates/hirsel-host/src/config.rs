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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    Lash,
    Scripted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderMode {
    Anthropic,
    Codex,
    OpenRouter,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub token: String,
    pub agent: AgentMode,
    pub provider: ProviderMode,
    pub anthropic_api_key: Option<String>,
    pub openrouter_api_key: Option<String>,
    pub model: String,
    pub data_dir: PathBuf,
    pub config_path: PathBuf,
    pub docs_path: PathBuf,
    pub templates_dir: PathBuf,
    pub driver: DriverMode,
    pub fake_fixture: Option<PathBuf>,
    pub listen: SocketAddr,
    pub debug: bool,
    /// Legacy side-session compatibility. `None` is the current-product
    /// default: no compatibility backend retention and no TTL reaper.
    pub compat_side_session_ttl_secs: Option<u64>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let token = env::var("HIRSEL_TOKEN")
            .context("HIRSEL_TOKEN is required for the Owner WebSocket protocol")?;
        validate_owner_token(&token)?;
        let agent = match env::var("HIRSEL_AGENT")
            .unwrap_or_else(|_| "lash".to_string())
            .as_str()
        {
            "lash" => AgentMode::Lash,
            "scripted" => AgentMode::Scripted,
            other => {
                return Err(anyhow!(
                    "HIRSEL_AGENT must be lash or scripted, got {other}"
                ));
            }
        };
        let provider = match env::var("HIRSEL_PROVIDER")
            .unwrap_or_else(|_| "anthropic".to_string())
            .as_str()
        {
            "anthropic" => ProviderMode::Anthropic,
            "codex" => ProviderMode::Codex,
            "openrouter" => ProviderMode::OpenRouter,
            other => {
                return Err(anyhow!(
                    "HIRSEL_PROVIDER must be anthropic, codex or openrouter, got {other}"
                ));
            }
        };
        let anthropic_api_key = env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|value| !value.is_empty());
        let openrouter_api_key = env::var("OPENROUTER_API_KEY")
            .ok()
            .filter(|value| !value.is_empty());
        let model = env::var("HIRSEL_MODEL").unwrap_or_else(|_| match provider {
            ProviderMode::Anthropic => "claude-opus-4-7".to_string(),
            ProviderMode::Codex => "gpt-5.6-sol".to_string(),
            ProviderMode::OpenRouter => "google/gemini-3.7-flash".to_string(),
        });
        let data_dir = env::var_os("HIRSEL_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("./data"));
        let config_path = absolute_path(
            env::var_os("HIRSEL_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(|| data_dir.join("hirsel.toml")),
        )?;
        let docs_path = absolute_path(
            env::var_os("HIRSEL_DOCS")
                .map(PathBuf::from)
                .unwrap_or_else(crate::templates::bundled_docs_path),
        )?;
        let templates_dir = env::var_os("HIRSEL_TEMPLATES_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("./templates"));
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
            .unwrap_or_else(|_| "127.0.0.1:3089".to_string())
            .parse()
            .context("HIRSEL_LISTEN must be a socket address")?;
        let debug = env::var("HIRSEL_DEBUG").ok().as_deref() == Some("1");
        let compat_side_session_ttl_secs = compatibility_side_session_ttl(
            env::var("HIRSEL_COMPAT_SIDE_SESSIONS").ok().as_deref(),
            env::var("HIRSEL_SIDECHAT_TTL_SECS").ok().as_deref(),
        )?;
        if debug && !listen.ip().is_loopback() {
            listen = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), listen.port());
        }
        Ok(Self {
            token,
            agent,
            provider,
            anthropic_api_key,
            openrouter_api_key,
            model,
            data_dir,
            config_path,
            docs_path,
            templates_dir,
            driver,
            fake_fixture,
            listen,
            debug,
            compat_side_session_ttl_secs,
        })
    }
}

fn compatibility_side_session_ttl(
    enabled: Option<&str>,
    ttl: Option<&str>,
) -> anyhow::Result<Option<u64>> {
    match enabled {
        None | Some("0") => Ok(None),
        Some("1") => {
            Ok(Some(ttl.unwrap_or("86400").parse().context(
                "HIRSEL_SIDECHAT_TTL_SECS must be an unsigned integer",
            )?))
        }
        Some(other) => Err(anyhow!(
            "HIRSEL_COMPAT_SIDE_SESSIONS must be 0 or 1, got {other}"
        )),
    }
}

fn absolute_path(path: PathBuf) -> anyhow::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .context("resolve current directory for Hirsel path")?
            .join(path)
    };
    Ok(std::fs::canonicalize(&absolute).unwrap_or(absolute))
}

fn validate_owner_token(token: &str) -> anyhow::Result<()> {
    if token.trim().is_empty() {
        anyhow::bail!("HIRSEL_TOKEN must not be empty or whitespace");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::compatibility_side_session_ttl;

    #[test]
    fn side_session_compatibility_is_explicit_and_ttl_is_parsed_only_when_enabled() {
        assert_eq!(
            compatibility_side_session_ttl(None, Some("broken")).unwrap(),
            None
        );
        assert_eq!(
            compatibility_side_session_ttl(Some("0"), Some("broken")).unwrap(),
            None
        );
        assert_eq!(
            compatibility_side_session_ttl(Some("1"), None).unwrap(),
            Some(86_400)
        );
        assert_eq!(
            compatibility_side_session_ttl(Some("1"), Some("60")).unwrap(),
            Some(60)
        );
        assert!(compatibility_side_session_ttl(Some("1"), Some("broken")).is_err());
        assert!(compatibility_side_session_ttl(Some("true"), None).is_err());
    }

    use super::validate_owner_token;

    #[test]
    fn owner_token_rejects_empty_and_whitespace_values() {
        for token in ["", " ", "\t\r\n"] {
            assert!(validate_owner_token(token).is_err(), "accepted {token:?}");
        }
        validate_owner_token("real-token").unwrap();
    }
}
