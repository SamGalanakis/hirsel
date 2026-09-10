use std::{
    collections::{HashMap, HashSet},
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
use hirsel_proto::ThreadAttention;
use serde::{Deserialize, Serialize};

use crate::storage::{Storage, ThreadPublication};

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
    pub history_id: String,
    pub thread_id: u64,
    pub title: String,
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
            thread_id = payload.data.thread_id,
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
            .json(&fcm_request(token, payload))
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

fn fcm_request(token: &str, payload: &PushPayload) -> serde_json::Value {
    serde_json::json!({
        "message": {
            "token": token,
            "notification": {
                "title": payload.title,
                "body": payload.body,
            },
            "data": {
                "history_id": payload.data.history_id,
                "thread_id": payload.data.thread_id.to_string(),
                "title": payload.data.title,
            }
        }
    })
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
    history_id: String,
    episodes: HashMap<u64, (bool, u64)>,
    in_flight: HashSet<(u64, u64)>,
    delivered: HashSet<(u64, u64)>,
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

    pub(crate) async fn enqueue_thread(&self, publication: &ThreadPublication) {
        let history_id = publication.history_id().to_owned();
        let thread = publication.thread();
        let eligible = thread.attention == ThreadAttention::NeedsOwner
            && thread.settled_at.is_none()
            && thread.archived_at.is_none()
            && thread.snoozed_until.is_none_or(|until| until <= Utc::now());
        let Some(delivery) = self.claim_delivery(&history_id, thread.id, eligible) else {
            return;
        };

        let tokens = match self.storage.push_tokens().await {
            Ok(tokens) => tokens
                .into_iter()
                .map(|registered| registered.token)
                .collect::<Vec<_>>(),
            Err(error) => {
                tracing::warn!(thread_id = thread.id, %error, "failed to load push tokens");
                self.release_delivery(delivery);
                return;
            }
        };
        if tokens.is_empty() {
            self.release_delivery(delivery);
            return;
        }

        let payload = PushPayload {
            title: OWNER_APP_NAME.to_string(),
            body: if thread.description.trim().is_empty() {
                thread.title.clone()
            } else {
                thread.description.clone()
            },
            data: PushData {
                history_id: history_id.clone(),
                thread_id: thread.id,
                title: thread.title.clone(),
            },
        };
        let sender = self.sender.clone();
        let delivery_state = Arc::clone(&self.delivery_state);
        let thread_id = thread.id;
        tokio::spawn(async move {
            let result = send_with_retry(sender.as_ref(), &tokens, &payload).await;
            let mut state = delivery_state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if state.history_id != history_id {
                return;
            }
            state.in_flight.remove(&delivery);
            if result.is_ok() {
                if state.episodes.get(&thread_id) == Some(&(true, delivery.1)) {
                    state.delivered.insert(delivery);
                }
            } else if let Err(error) = result {
                tracing::warn!(thread_id, %error, "push delivery failed after retries");
            }
        });
    }

    fn claim_delivery(
        &self,
        history_id: &str,
        thread_id: u64,
        eligible: bool,
    ) -> Option<(u64, u64)> {
        let mut state = self
            .delivery_state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if state.history_id != history_id {
            *state = PushDeliveryState {
                history_id: history_id.to_owned(),
                ..Default::default()
            };
        }
        let episode = state.episodes.entry(thread_id).or_insert((false, 0));
        if episode.0 != eligible {
            episode.0 = eligible;
            episode.1 += 1;
        }
        let key = (thread_id, episode.1);
        state
            .delivered
            .retain(|old| old.0 != thread_id || *old == key);
        if !eligible || state.delivered.contains(&key) || !state.in_flight.insert(key) {
            return None;
        }
        Some(key)
    }

    fn release_delivery(&self, delivery: (u64, u64)) {
        self.delivery_state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .in_flight
            .remove(&delivery);
    }

    pub fn recorded_pushes(&self) -> Vec<RecordedPush> {
        self.recording
            .as_ref()
            .map_or_else(Vec::new, RecordingPushSender::pushes)
    }

    pub fn clear_recorded_pushes(&self) {
        *self
            .delivery_state
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = PushDeliveryState::default();
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
    use hirsel_proto::{PushPlatform, Thread};

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

    async fn attention_thread(storage: &Storage) -> Thread {
        storage
            .create_thread(
                "push-test",
                "Choose",
                "Decision",
                &serde_json::json!({}),
                ThreadAttention::NeedsOwner,
                None,
            )
            .await
            .unwrap()
            .0
    }

    async fn enqueue_current(gateway: &PushGateway, storage: &Storage, thread: &Thread) {
        gateway
            .enqueue_thread(&ThreadPublication::test(
                storage.history_id().await.unwrap(),
                thread.clone(),
            ))
            .await;
    }

    async fn wait_attempts(attempts: &AtomicUsize, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while attempts.load(Ordering::SeqCst) < expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn fcm_request_projects_the_captured_destination_as_string_data() {
        let payload = PushPayload {
            title: "Hirsel".to_string(),
            body: "Decision".to_string(),
            data: PushData {
                history_id: "history-a".to_string(),
                thread_id: 42,
                title: "Choose".to_string(),
            },
        };

        assert_eq!(
            fcm_request("device-token", &payload),
            serde_json::json!({
                "message": {
                    "token": "device-token",
                    "notification": {
                        "title": "Hirsel",
                        "body": "Decision",
                    },
                    "data": {
                        "history_id": "history-a",
                        "thread_id": "42",
                        "title": "Choose",
                    },
                },
            })
        );
    }

    async fn wait_recorded(gateway: &PushGateway, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while gateway.recorded_pushes().len() < expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn history_reset_retains_registration_for_new_history_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        storage
            .register_push_token(PushPlatform::Android, "durable-token")
            .await
            .unwrap();
        let (gateway, _) = PushGateway::recording(storage.clone());

        let old_history = storage.history_id().await.unwrap();
        enqueue_current(&gateway, &storage, &attention_thread(&storage).await).await;
        wait_recorded(&gateway, 1).await;

        storage.reset().await.unwrap();
        let new_history = storage.history_id().await.unwrap();
        assert_ne!(new_history, old_history);
        assert_eq!(
            storage
                .push_tokens()
                .await
                .unwrap()
                .into_iter()
                .map(|token| token.token)
                .collect::<Vec<_>>(),
            vec!["durable-token"]
        );

        enqueue_current(&gateway, &storage, &attention_thread(&storage).await).await;
        wait_recorded(&gateway, 2).await;
        let pushes = gateway.recorded_pushes();
        assert_eq!(pushes[0].payload.data.history_id, old_history);
        assert_eq!(pushes[1].payload.data.history_id, new_history);
        assert_eq!(pushes[1].tokens, vec!["durable-token"]);
    }

    #[tokio::test]
    async fn current_thread_delivery_retries_then_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        storage
            .register_push_token(PushPlatform::Android, "test-token")
            .await
            .unwrap();
        let sender = Arc::new(FailOnceSender::default());
        let gateway = PushGateway::new(storage.clone(), sender.clone(), None);
        let thread = attention_thread(&storage).await;
        enqueue_current(&gateway, &storage, &thread).await;
        wait_attempts(&sender.attempts, 2).await;
        enqueue_current(&gateway, &storage, &thread).await;
        assert_eq!(sender.attempts.load(Ordering::SeqCst), 2);
    }
    struct HeldSender {
        attempts: AtomicUsize,
        release: tokio::sync::Semaphore,
    }
    #[async_trait]
    impl PushSender for HeldSender {
        async fn send(&self, _: &[String], _: &PushPayload) -> anyhow::Result<()> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            self.release.acquire().await.unwrap().forget();
            Ok(())
        }
    }
    #[tokio::test]
    async fn new_attention_episode_survives_older_in_flight_completion() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        storage
            .register_push_token(PushPlatform::Android, "test-token")
            .await
            .unwrap();
        let sender = Arc::new(HeldSender {
            attempts: AtomicUsize::new(0),
            release: tokio::sync::Semaphore::new(0),
        });
        let gateway = PushGateway::new(storage.clone(), sender.clone(), None);
        let mut thread = attention_thread(&storage).await;
        enqueue_current(&gateway, &storage, &thread).await;
        wait_attempts(&sender.attempts, 1).await;
        thread.attention = ThreadAttention::Quiet;
        enqueue_current(&gateway, &storage, &thread).await;
        thread.attention = ThreadAttention::NeedsOwner;
        enqueue_current(&gateway, &storage, &thread).await;
        wait_attempts(&sender.attempts, 2).await;
        sender.release.add_permits(2);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if gateway.delivery_state.lock().unwrap().in_flight.is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Ordinary revisions/read changes stay in the same attention episode.
        thread.revision += 1;
        thread.read = true;
        enqueue_current(&gateway, &storage, &thread).await;
        assert_eq!(sender.attempts.load(Ordering::SeqCst), 2);
        thread.snoozed_until = Some(Utc::now() + chrono::Duration::hours(1));
        enqueue_current(&gateway, &storage, &thread).await;
        thread.snoozed_until = None;
        enqueue_current(&gateway, &storage, &thread).await;
        wait_attempts(&sender.attempts, 3).await;
        sender.release.add_permits(1);
    }
}
