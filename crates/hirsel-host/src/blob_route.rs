use anyhow::Context;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CONTENT_DISPOSITION, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::{AppState, auth::owner_bearer_matches};

const SIGNED_BLOB_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Deserialize)]
pub struct BlobQuery {
    exp: Option<u64>,
    sig: Option<String>,
}

#[derive(Clone)]
pub struct BlobSigner {
    secret: Arc<[u8]>,
    ttl: Duration,
}

impl BlobSigner {
    pub fn new(secret: impl AsRef<[u8]>) -> Self {
        Self {
            secret: Arc::from(secret.as_ref()),
            ttl: SIGNED_BLOB_TTL,
        }
    }

    pub fn mint(&self, blob_id: &str) -> anyhow::Result<SignedBlobUrl> {
        self.mint_at(blob_id, SystemTime::now())
    }

    fn mint_at(&self, blob_id: &str, now: SystemTime) -> anyhow::Result<SignedBlobUrl> {
        let expires_at = now
            .duration_since(UNIX_EPOCH)
            .context("system clock is before Unix epoch")?
            .as_secs()
            .saturating_add(self.ttl.as_secs());
        let signature = self.signature(blob_id, expires_at)?;
        Ok(SignedBlobUrl {
            url: format!("/blob/{blob_id}?exp={expires_at}&sig={signature}"),
            expires_at,
        })
    }

    fn verify(&self, blob_id: &str, expires_at: u64, signature: &str) -> bool {
        self.verify_at(blob_id, expires_at, signature, SystemTime::now())
    }

    fn verify_at(&self, blob_id: &str, expires_at: u64, signature: &str, now: SystemTime) -> bool {
        let Ok(now) = now.duration_since(UNIX_EPOCH) else {
            return false;
        };
        if now.as_secs() > expires_at {
            return false;
        }
        let Ok(signature) = URL_SAFE_NO_PAD.decode(signature) else {
            return false;
        };
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&self.secret) else {
            return false;
        };
        mac.update(signing_input(blob_id, expires_at).as_bytes());
        mac.verify_slice(&signature).is_ok()
    }

    fn signature(&self, blob_id: &str, expires_at: u64) -> anyhow::Result<String> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret)
            .map_err(|_| anyhow::anyhow!("invalid blob signing key"))?;
        mac.update(signing_input(blob_id, expires_at).as_bytes());
        Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedBlobUrl {
    pub url: String,
    pub expires_at: u64,
}

fn signing_input(blob_id: &str, expires_at: u64) -> String {
    format!("hirsel-blob-v1\n{blob_id}\n{expires_at}")
}

pub async fn blob_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<BlobQuery>,
    headers: HeaderMap,
) -> Result<Response, BlobRouteError> {
    if !is_authorized(&state, &id, &query, &headers) {
        return Err(BlobRouteError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ));
    }
    let Some(stored) = state.storage.blob(&id).await? else {
        return Err(BlobRouteError::new(StatusCode::NOT_FOUND, "blob not found"));
    };
    let data = tokio::fs::read(&stored.path).await?;

    let mut response = Response::new(Body::from(data));
    let response_headers = response.headers_mut();
    response_headers.insert(CONTENT_TYPE, content_type_header(&stored.blob.mime));
    response_headers.insert(
        CONTENT_DISPOSITION,
        content_disposition_header(&stored.blob.name, &stored.blob.mime),
    );
    Ok(response)
}

fn is_authorized(state: &AppState, id: &str, query: &BlobQuery, headers: &HeaderMap) -> bool {
    query
        .exp
        .zip(query.sig.as_deref())
        .is_some_and(|(expires_at, signature)| state.blob_signer.verify(id, expires_at, signature))
        || owner_bearer_matches(headers, &state.token)
}

fn content_type_header(mime: &str) -> HeaderValue {
    HeaderValue::from_str(mime)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
}

fn content_disposition_header(name: &str, mime: &str) -> HeaderValue {
    let disposition = if mime.starts_with("image/") {
        "inline"
    } else {
        "attachment"
    };
    let value = format!("{disposition}; filename=\"{}\"", quoted_filename(name));
    HeaderValue::from_str(&value).unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

fn quoted_filename(name: &str) -> String {
    let sanitized = name
        .chars()
        .map(|ch| match ch {
            '"' | '\\' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "attachment".to_string()
    } else {
        sanitized
    }
}

#[derive(Debug)]
pub struct BlobRouteError {
    status: StatusCode,
    message: String,
}

impl BlobRouteError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for BlobRouteError {
    fn from(error: anyhow::Error) -> Self {
        tracing::warn!(%error, "blob route failed");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    }
}

impl From<std::io::Error> for BlobRouteError {
    fn from(error: std::io::Error) -> Self {
        tracing::warn!(%error, "blob route file read failed");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    }
}

impl IntoResponse for BlobRouteError {
    fn into_response(self) -> axum::response::Response {
        (self.status, self.message).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_is_inline_for_images() {
        let header = content_disposition_header("tiny.png", "image/png");
        assert_eq!(header.to_str().unwrap(), "inline; filename=\"tiny.png\"");
    }

    #[test]
    fn content_disposition_is_attachment_for_documents() {
        let header = content_disposition_header("note.txt", "text/plain");
        assert_eq!(
            header.to_str().unwrap(),
            "attachment; filename=\"note.txt\""
        );
    }

    #[test]
    fn signed_blob_urls_are_scoped_and_expire() {
        let signer = BlobSigner {
            secret: Arc::from(b"test-secret".as_slice()),
            ttl: Duration::from_secs(300),
        };
        let expires_at = 400;
        let signature = signer.signature("blob-a", expires_at).unwrap();
        let before_expiry = UNIX_EPOCH + Duration::from_secs(399);
        let after_expiry = UNIX_EPOCH + Duration::from_secs(401);

        assert!(signer.verify_at("blob-a", expires_at, &signature, before_expiry));
        assert!(!signer.verify_at("blob-b", expires_at, &signature, before_expiry));
        assert!(!signer.verify_at("blob-a", expires_at, &signature, after_expiry));
    }
}
