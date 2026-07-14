use std::{
    collections::HashSet,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use hirsel_proto::{Event, EventKind, EventStatus};
use serde::{Deserialize, Serialize};

use crate::storage::Storage;

const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const OWNER_APP_NAME: &str = "Hirsel";
const MAX_DELIVERY_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PushPayload {
    pub title: String,
    pub body: String,
    pub data: PushData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PushData {
    pub event_id: u64,
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
        tracing::info!(
            token_count = tokens.len(),
            event_id = payload.data.event_id,
            "FCM not configured — would send push"
        );
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

    async fn send_to_token(
        &self,
        endpoint: &str,
        access_token: &str,
        token: &str,
        payload: &PushPayload,
    ) -> anyhow::Result<()> {
        let response = self
            .client
            .post(endpoint)
            .bearer_auth(access_token)
            .json(&serde_json::json!({
                "message": {
                    "token": token,
                    "notification": {
                        "title": payload.title,
                        "body": payload.body,
                    },
                    "data": {
                        "event_id": payload.data.event_id.to_string(),
                        "name": payload.data.name,
                    }
                }
            }))
            .send()
            .await
            .context("send FCM message")?;
        let status = response.status();
        let _body = response.text().await.context("read FCM send response")?;
        if !status.is_success() {
            anyhow::bail!(
                "FCM send failed for token ending {} ({status})",
                token_suffix(token)
            );
        }
        Ok(())
    }
}

fn token_suffix(token: &str) -> String {
    let suffix = token.chars().rev().take(4).collect::<Vec<_>>();
    suffix.into_iter().rev().collect()
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
        let mut failures = Vec::new();
        for token in tokens {
            if let Err(error) = self
                .send_to_token(&endpoint, &access_token, token, payload)
                .await
            {
                failures.push(error.to_string());
            }
        }
        if !failures.is_empty() {
            anyhow::bail!("one or more FCM sends failed: {}", failures.join("; "));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct PushGateway {
    storage: Storage,
    sender: Arc<dyn PushSender>,
    recording: Option<RecordingPushSender>,
    delivery_state: Arc<StdMutex<PushDeliveryState>>,
}

#[derive(Default)]
struct PushDeliveryState {
    in_flight: HashSet<u64>,
    delivered: HashSet<u64>,
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
            delivery_state: Arc::new(StdMutex::new(PushDeliveryState::default())),
        }
    }

    pub(crate) async fn enqueue_event(&self, event: &Event) {
        if !matches!(event.kind, EventKind::Judgment)
            || event.status != EventStatus::Open
            || event.archived
            || event
                .snoozed_until
                .is_some_and(|snoozed_until| snoozed_until > Utc::now())
        {
            return;
        }
        if !self.claim_delivery(event.id) {
            return;
        }

        let tokens = match self.storage.push_tokens().await {
            Ok(tokens) => tokens
                .into_iter()
                .map(|registered| registered.token)
                .collect::<Vec<_>>(),
            Err(error) => {
                tracing::warn!(event_id = event.id, %error, "failed to load push tokens");
                self.release_delivery(event.id);
                return;
            }
        };
        if tokens.is_empty() {
            self.release_delivery(event.id);
            return;
        }

        let payload = PushPayload {
            title: OWNER_APP_NAME.to_string(),
            body: if event.description.trim().is_empty() {
                event.name.clone()
            } else {
                event.description.clone()
            },
            data: PushData {
                event_id: event.id,
                name: event.name.clone(),
            },
        };
        let sender = self.sender.clone();
        let delivery_state = Arc::clone(&self.delivery_state);
        let event_id = event.id;
        tokio::spawn(async move {
            let result = send_with_retry(sender.as_ref(), &tokens, &payload).await;
            let mut state = delivery_state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.in_flight.remove(&event_id);
            if result.is_ok() {
                state.delivered.insert(event_id);
            } else if let Err(error) = result {
                tracing::warn!(event_id, %error, "push delivery failed after retries");
            }
        });
    }

    #[cfg(test)]
    pub(crate) async fn enqueue_ping(&self, event: &Event) {
        self.enqueue_event(event).await;
    }

    pub(crate) async fn reenqueue_event(&self, event: &Event) {
        self.delivery_state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .delivered
            .remove(&event.id);
        self.enqueue_event(event).await;
    }

    fn claim_delivery(&self, ping_id: u64) -> bool {
        let mut state = self
            .delivery_state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.delivered.contains(&ping_id) || state.in_flight.contains(&ping_id) {
            return false;
        }
        state.in_flight.insert(ping_id);
        true
    }

    fn release_delivery(&self, ping_id: u64) {
        self.delivery_state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .in_flight
            .remove(&ping_id);
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

async fn send_with_retry(
    sender: &dyn PushSender,
    tokens: &[String],
    payload: &PushPayload,
) -> anyhow::Result<()> {
    let mut last_error = None;
    for attempt in 1..=MAX_DELIVERY_ATTEMPTS {
        match sender.send(tokens, payload).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                tracing::warn!(attempt, %error, "push delivery attempt failed");
                last_error = Some(error);
                if attempt < MAX_DELIVERY_ATTEMPTS {
                    tokio::time::sleep(retry_delay(attempt)).await;
                }
            }
        }
    }
    Err(last_error.expect("at least one push delivery attempt"))
}

fn retry_delay(attempt: usize) -> Duration {
    #[cfg(test)]
    const BASE: Duration = Duration::from_millis(5);
    #[cfg(not(test))]
    const BASE: Duration = Duration::from_millis(250);
    BASE.saturating_mul(1 << (attempt.saturating_sub(1).min(4)))
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::Utc;
    use hirsel_proto::{PingStatus, PushPlatform};

    use super::*;

    #[derive(Default)]
    struct FailOnceSender {
        attempts: AtomicUsize,
    }

    #[async_trait]
    impl PushSender for FailOnceSender {
        async fn send(&self, _tokens: &[String], _payload: &PushPayload) -> anyhow::Result<()> {
            if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                anyhow::bail!("transient failure");
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn failed_push_is_retried_before_marking_delivered() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        storage
            .register_push_token(PushPlatform::Android, "device-token")
            .await
            .unwrap();
        let sender = Arc::new(FailOnceSender::default());
        let gateway = PushGateway::new(storage, sender.clone(), None);
        let ping = Event {
            id: 42,
            kind: EventKind::Judgment,
            source: hirsel_proto::EventSource {
                kind: hirsel_proto::EventSourceKind::Agent,
                r#ref: None,
            },
            name: "decision".to_string(),
            description: "Choose".to_string(),
            ui: serde_json::json!({ "type": "card", "children": [] }),
            anchor: 1,
            requires_response: true,
            quick_replies: Vec::new(),
            status: PingStatus::Open,
            read: false,
            archived: false,
            snoozed_until: None,
            archived_at: None,
            fork_sc: None,
            ts: Utc::now(),
        };

        gateway.enqueue_ping(&ping).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while sender.attempts.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        gateway.enqueue_ping(&ping).await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        assert_eq!(sender.attempts.load(Ordering::SeqCst), 2);
    }
}
