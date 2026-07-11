use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::AppState;

#[derive(Serialize)]
struct HealthBody<'a> {
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    failed_check: Option<&'a str>,
}

pub async fn livez() -> impl IntoResponse {
    Json(HealthBody {
        status: "ok",
        failed_check: None,
    })
}

pub async fn readyz(State(state): State<AppState>) -> Response {
    if state.storage.latest_msg_id().await.is_err() {
        return not_ready("sqlite");
    }
    if state.agent.readiness().is_err() {
        return not_ready("lash_store");
    }
    if disk_has_space(&state.data_dir).is_err() {
        return not_ready("disk_space");
    }
    if state.iroh_ticket().is_none() {
        return not_ready("iroh_endpoint");
    }
    (
        StatusCode::OK,
        Json(HealthBody {
            status: "ready",
            failed_check: None,
        }),
    )
        .into_response()
}

fn not_ready(check: &'static str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(HealthBody {
            status: "not_ready",
            failed_check: Some(check),
        }),
    )
        .into_response()
}

#[cfg(unix)]
fn disk_has_space(path: &std::path::Path) -> anyhow::Result<()> {
    use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};

    let path = CString::new(path.as_os_str().as_bytes())?;
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a live NUL-terminated string and `stat` points to
    // writable storage for one `statvfs` value.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: a successful `statvfs` call initialized the output value.
    let stat = unsafe { stat.assume_init() };
    let available = (stat.f_bavail as u128).saturating_mul(stat.f_frsize as u128);
    if available < 1024 * 1024 {
        anyhow::bail!("less than 1 MiB available");
    }
    Ok(())
}

#[cfg(not(unix))]
fn disk_has_space(path: &std::path::Path) -> anyhow::Result<()> {
    std::fs::metadata(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::{extract::State, http::StatusCode};

    use super::readyz;
    use crate::{build_state, tests::test_config};

    #[tokio::test]
    async fn readyz_is_ok_when_all_checks_pass() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        state.set_iroh_ticket(Some("test-ticket".to_string()));

        assert_eq!(readyz(State(state)).await.status(), StatusCode::OK);
    }
}
