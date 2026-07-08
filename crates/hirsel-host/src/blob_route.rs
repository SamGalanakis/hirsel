use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{AUTHORIZATION, CONTENT_DISPOSITION, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct BlobQuery {
    token: Option<String>,
}

pub async fn blob_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<BlobQuery>,
    headers: HeaderMap,
) -> Result<Response, BlobRouteError> {
    if !is_authorized(&state, &query, &headers) {
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

fn is_authorized(state: &AppState, query: &BlobQuery, headers: &HeaderMap) -> bool {
    if query.token.as_deref() == Some(state.token.as_ref()) {
        return true;
    }
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .is_some_and(|(scheme, token)| {
            scheme.eq_ignore_ascii_case("bearer") && token == state.token.as_ref()
        })
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
}
