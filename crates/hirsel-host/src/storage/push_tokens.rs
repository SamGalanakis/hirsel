//! Push notification token registry.

use super::Storage;
use super::common::{collect_rows, parse_ts};
use chrono::DateTime;
use chrono::Utc;
use hirsel_proto::PushPlatform;
use rusqlite::Connection;
use rusqlite::params;
use rusqlite::types::Type;
use serde::Serialize;

impl Storage {
    pub async fn register_push_token(
        &self,
        platform: PushPlatform,
        token: impl Into<String>,
    ) -> anyhow::Result<PushToken> {
        let token = token.into();
        if token.trim().is_empty() {
            anyhow::bail!("push token must not be empty");
        }
        let now = Utc::now();
        let conn = self.conn.lock().await;
        conn.execute(
            "
            INSERT INTO push_tokens (token, platform, created_ts, last_seen_ts)
            VALUES (?1, ?2, ?3, ?3)
            ON CONFLICT(token) DO UPDATE SET
                platform = excluded.platform,
                last_seen_ts = excluded.last_seen_ts
            ",
            params![token, push_platform_to_str(platform), now.to_rfc3339()],
        )?;
        get_push_token(&conn, &token).map_err(Into::into)
    }

    pub async fn unregister_push_token(&self, token: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock().await;
        Ok(conn.execute("DELETE FROM push_tokens WHERE token = ?1", params![token])? > 0)
    }

    pub async fn push_tokens(&self) -> anyhow::Result<Vec<PushToken>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "
            SELECT token, platform, created_ts, last_seen_ts
            FROM push_tokens
            ORDER BY created_ts ASC, token ASC
            ",
        )?;
        let rows = stmt.query_map([], push_token_from_row)?;
        collect_rows(rows)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PushToken {
    pub token: String,
    pub platform: PushPlatform,
    pub created_ts: DateTime<Utc>,
    pub last_seen_ts: DateTime<Utc>,
}

fn get_push_token(conn: &Connection, token: &str) -> rusqlite::Result<PushToken> {
    conn.query_row(
        "
        SELECT token, platform, created_ts, last_seen_ts
        FROM push_tokens
        WHERE token = ?1
        ",
        params![token],
        push_token_from_row,
    )
}

fn push_token_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PushToken> {
    let platform: String = row.get(1)?;
    let created_ts: String = row.get(2)?;
    let last_seen_ts: String = row.get(3)?;
    Ok(PushToken {
        token: row.get(0)?,
        platform: push_platform_from_str(&platform)?,
        created_ts: parse_ts(&created_ts)?,
        last_seen_ts: parse_ts(&last_seen_ts)?,
    })
}

fn push_platform_to_str(platform: PushPlatform) -> &'static str {
    match platform {
        PushPlatform::Android => "android",
        PushPlatform::Web => "web",
        PushPlatform::Ios => "ios",
    }
}

fn push_platform_from_str(value: &str) -> rusqlite::Result<PushPlatform> {
    match value {
        "android" => Ok(PushPlatform::Android),
        "web" => Ok(PushPlatform::Web),
        "ios" => Ok(PushPlatform::Ios),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            1,
            Type::Text,
            format!("unknown push platform: {other}").into(),
        )),
    }
}

#[cfg(test)]
mod tests;
