use std::{
    collections::HashSet,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex as StdMutex},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hirsel_proto::Ping;
use serde::{Deserialize, Serialize};

use crate::storage::Storage;

const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const OWNER_APP_NAME: &str = "Hirsel";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PushPayload {
    pub title: String,
    pub body: String,
    pub data: PushData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PushData {
    pub ping_id: u64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordedPush {
    pub tokens: Vec<String>,
    pub payload: PushPayload,
}

#[async_trait]
pub trait PushSender: Send + Sync {
    async fn send(&self, tokens: &[String], payload: &PushPayload) -> anyhow::Result<()>;
}

#[derive(Clone, Default)]
pub struct RecordingPushSender {
    pushes: Arc<StdMutex<Vec<RecordedPush>>>,
}

impl RecordingPushSender {
    pub fn pushes(&self) -> Vec<RecordedPush> {
        self.pushes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    pub fn clear(&self) {
        self.pushes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
    }
}

#[async_trait]
impl PushSender for RecordingPushSender {
    async fn send(&self, tokens: &[String], payload: &PushPayload) -> anyhow::Result<()> {
        tracing::info!(?tokens, ?payload, "FCM not configured — would push …");
        self.pushes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(RecordedPush {
                tokens: tokens.to_vec(),
                payload: payload.clone(),
            });
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ServiceAccount {
    client_email: String,
    private_key: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    DEFAULT_TOKEN_URI.to_string()
}

pub struct FcmPushSender {
    project: String,
    service_account: ServiceAccount,
    client: reqwest::Client,
}

impl FcmPushSender {
    pub async fn from_service_account_file(
        project: impl Into<String>,
        credentials_path: &Path,
    ) -> anyhow::Result<Self> {
        let project = project.into();
        if project.trim().is_empty() {
            anyhow::bail!("HIRSEL_FCM_PROJECT must not be empty");
        }
        let bytes = tokio::fs::read(credentials_path).await.with_context(|| {
            format!(
                "read FCM service-account JSON from {}",
                credentials_path.display()
            )
        })?;
        let service_account: ServiceAccount =
            serde_json::from_slice(&bytes).context("parse FCM service-account JSON")?;
        if service_account.client_email.trim().is_empty()
            || service_account.private_key.trim().is_empty()
        {
            anyhow::bail!("FCM service-account JSON lacks client_email or private_key");
        }
        Ok(Self {
            project,
            service_account,
            client: reqwest::Client::new(),
        })
    }

    async fn access_token(&self) -> anyhow::Result<String> {
        let service_account = self.service_account.clone();
        let assertion = tokio::task::spawn_blocking(move || service_account_jwt(&service_account))
            .await
            .context("join FCM JWT signing task")??;
        let response = self
            .client
            .post(&self.service_account.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .context("exchange FCM service-account JWT")?;
        let status = response.status();
        let body = response.text().await.context("read FCM OAuth response")?;
        if !status.is_success() {
            anyhow::bail!("FCM OAuth token exchange failed ({status}): {body}");
        }
        let token: AccessTokenResponse =
            serde_json::from_str(&body).context("parse FCM OAuth response")?;
        Ok(token.access_token)
    }
}

#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    access_token: String,
}

#[async_trait]
impl PushSender for FcmPushSender {
    async fn send(&self, tokens: &[String], payload: &PushPayload) -> anyhow::Result<()> {
        let access_token = self.access_token().await?;
        let endpoint = format!(
            "https://fcm.googleapis.com/v1/projects/{}/messages:send",
            self.project
        );
        for token in tokens {
            let response = self
                .client
                .post(&endpoint)
                .bearer_auth(&access_token)
                .json(&serde_json::json!({
                    "message": {
                        "token": token,
                        "notification": {
                            "title": payload.title,
                            "body": payload.body,
                        },
                        "data": {
                            "ping_id": payload.data.ping_id.to_string(),
                            "name": payload.data.name,
                        }
                    }
                }))
                .send()
                .await
                .with_context(|| format!("send FCM message to token {token}"))?;
            let status = response.status();
            let body = response.text().await.context("read FCM send response")?;
            if !status.is_success() {
                anyhow::bail!("FCM send failed for token {token} ({status}): {body}");
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct PushGateway {
    storage: Storage,
    sender: Arc<dyn PushSender>,
    recording: Option<RecordingPushSender>,
    sent_ping_ids: Arc<StdMutex<HashSet<u64>>>,
}

impl PushGateway {
    pub async fn from_env(storage: Storage) -> anyhow::Result<Self> {
        #[cfg(test)]
        {
            let (gateway, _) = Self::recording(storage);
            Ok(gateway)
        }

        #[cfg(not(test))]
        {
            let credentials = nonempty_env("HIRSEL_FCM_CREDENTIALS");
            let project = nonempty_env("HIRSEL_FCM_PROJECT");
            match (credentials, project) {
                (Some(credentials), Some(project)) => {
                    let sender =
                        FcmPushSender::from_service_account_file(project, Path::new(&credentials))
                            .await?;
                    tracing::info!("FCM HTTP-v1 push delivery configured");
                    Ok(Self::new(storage, Arc::new(sender), None))
                }
                (credentials, project) => {
                    if credentials.is_some() || project.is_some() {
                        tracing::warn!(
                            "FCM configuration incomplete; set both HIRSEL_FCM_CREDENTIALS and HIRSEL_FCM_PROJECT"
                        );
                    } else {
                        tracing::info!("FCM not configured; using log-only push sender");
                    }
                    let (gateway, _) = Self::recording(storage);
                    Ok(gateway)
                }
            }
        }
    }

    pub fn recording(storage: Storage) -> (Self, RecordingPushSender) {
        let recording = RecordingPushSender::default();
        let gateway = Self::new(
            storage,
            Arc::new(recording.clone()),
            Some(recording.clone()),
        );
        (gateway, recording)
    }

    fn new(
        storage: Storage,
        sender: Arc<dyn PushSender>,
        recording: Option<RecordingPushSender>,
    ) -> Self {
        Self {
            storage,
            sender,
            recording,
            sent_ping_ids: Arc::new(StdMutex::new(HashSet::new())),
        }
    }

    pub(crate) async fn enqueue_ping(&self, ping: &Ping) {
        if !ping.requires_response {
            return;
        }
        let first_send = self
            .sent_ping_ids
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(ping.id);
        if !first_send {
            return;
        }

        let tokens = match self.storage.push_tokens().await {
            Ok(tokens) => tokens
                .into_iter()
                .map(|registered| registered.token)
                .collect::<Vec<_>>(),
            Err(error) => {
                tracing::warn!(ping_id = ping.id, %error, "failed to load push tokens");
                return;
            }
        };
        if tokens.is_empty() {
            return;
        }

        let payload = PushPayload {
            title: OWNER_APP_NAME.to_string(),
            body: if ping.description.trim().is_empty() {
                ping.name.clone()
            } else {
                ping.description.clone()
            },
            data: PushData {
                ping_id: ping.id,
                name: ping.name.clone(),
            },
        };
        let sender = self.sender.clone();
        tokio::spawn(async move {
            if let Err(error) = sender.send(&tokens, &payload).await {
                tracing::warn!(%error, "push delivery failed");
            }
        });
    }

    pub fn recorded_pushes(&self) -> Vec<RecordedPush> {
        self.recording
            .as_ref()
            .map_or_else(Vec::new, RecordingPushSender::pushes)
    }

    pub fn clear_recorded_pushes(&self) {
        if let Some(recording) = &self.recording {
            recording.clear();
        }
    }
}

#[cfg(not(test))]
fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn service_account_jwt(account: &ServiceAccount) -> anyhow::Result<String> {
    let issued_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs();
    let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&serde_json::json!({
        "alg": "RS256",
        "typ": "JWT"
    }))?);
    let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&serde_json::json!({
        "iss": account.client_email,
        "scope": FCM_SCOPE,
        "aud": account.token_uri,
        "iat": issued_at,
        "exp": issued_at + 3600
    }))?);
    let signing_input = format!("{header}.{claims}");

    let mut key_file = tempfile::NamedTempFile::new().context("create temporary FCM key file")?;
    key_file
        .write_all(account.private_key.as_bytes())
        .context("write temporary FCM key file")?;
    key_file.flush().context("flush temporary FCM key file")?;
    let mut child = Command::new("openssl")
        .args(["dgst", "-sha256", "-sign"])
        .arg(key_file.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start openssl to sign FCM service-account JWT")?;
    child
        .stdin
        .take()
        .context("open openssl stdin")?
        .write_all(signing_input.as_bytes())
        .context("write FCM JWT signing input")?;
    let output = child
        .wait_with_output()
        .context("wait for openssl FCM JWT signing")?;
    if !output.status.success() {
        anyhow::bail!(
            "openssl failed to sign FCM JWT: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let signature = URL_SAFE_NO_PAD.encode(output.stdout);
    Ok(format!("{signing_input}.{signature}"))
}
