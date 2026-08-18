//! Device tokens and pairing codes.

use super::Storage;
use super::common::{collect_rows, is_unique_constraint, parse_ts};
use chrono::DateTime;
use chrono::Utc;
use rusqlite::OptionalExtension;
use rusqlite::params;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::time::Duration;
use std::time::Instant;
use uuid::Uuid;

impl Storage {
    pub async fn issue_device_token(
        &self,
        device_label: impl Into<String>,
        node_id: impl Into<String>,
    ) -> anyhow::Result<String> {
        let device_label = device_label.into();
        let node_id = node_id.into();
        validate_device_label(&device_label)?;
        if node_id.trim().is_empty() {
            anyhow::bail!("device NodeId must not be empty");
        }

        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        for _ in 0..4 {
            let token = random_secret();
            match conn.execute(
                "
                INSERT INTO device_tokens (
                    token, device_label, node_id, created_ts, last_seen_ts, revoked_ts
                )
                VALUES (?1, ?2, ?3, ?4, ?4, NULL)
                ",
                params![token, device_label, node_id, now],
            ) {
                Ok(_) => return Ok(token),
                Err(error) if is_unique_constraint(&error) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("failed to generate a unique device token")
    }

    pub async fn authenticate_device_token(
        &self,
        token: &str,
        node_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let record = conn
            .query_row(
                "SELECT node_id, revoked_ts FROM device_tokens WHERE token = ?1",
                params![token],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        let Some((pinned_node_id, revoked_ts)) = record else {
            anyhow::bail!("unknown device token");
        };
        if revoked_ts.is_some() {
            anyhow::bail!("revoked device token");
        }
        if node_id.is_some_and(|node_id| node_id != pinned_node_id) {
            anyhow::bail!("device token NodeId mismatch");
        }
        conn.execute(
            "UPDATE device_tokens SET last_seen_ts = ?2 WHERE token = ?1",
            params![token, now],
        )?;
        Ok(())
    }

    pub async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "
            SELECT device_label, node_id, created_ts, last_seen_ts, revoked_ts
            FROM device_tokens
            ORDER BY created_ts ASC, device_label ASC
            ",
        )?;
        let rows = stmt.query_map([], device_from_row)?;
        collect_rows(rows)
    }

    pub async fn revoke_device(&self, token_or_label: &str) -> anyhow::Result<usize> {
        if token_or_label.trim().is_empty() {
            anyhow::bail!("device token or label must not be empty");
        }
        let conn = self.conn.lock().await;
        Ok(conn.execute(
            "
            UPDATE device_tokens
            SET revoked_ts = ?2
            WHERE revoked_ts IS NULL AND (token = ?1 OR device_label = ?1)
            ",
            params![token_or_label, Utc::now().to_rfc3339()],
        )?)
    }

    pub async fn mint_pairing_code(
        &self,
        device_label: impl Into<String>,
        ttl: Duration,
    ) -> anyhow::Result<String> {
        let device_label = device_label.into();
        validate_device_label(&device_label)?;
        let now = Instant::now();
        let expires_at = now
            .checked_add(ttl)
            .ok_or_else(|| anyhow::anyhow!("pairing-code TTL is too large"))?;
        let mut pairing_codes = self.pairing_codes.lock().await;
        pairing_codes
            .codes
            .retain(|_, entry| entry.expires_at > now);
        if pairing_codes.codes.len() >= MAX_PAIRING_CODES {
            anyhow::bail!("too many outstanding pairing codes");
        }
        for _ in 0..4 {
            let code = random_secret();
            if pairing_codes
                .codes
                .insert(
                    code.clone(),
                    PairingCode {
                        expires_at,
                        device_label: device_label.clone(),
                    },
                )
                .is_none()
            {
                return Ok(code);
            }
        }
        anyhow::bail!("failed to generate a unique pairing code")
    }

    pub async fn redeem_pairing_code(&self, code: &str) -> anyhow::Result<String> {
        let now = Instant::now();
        let mut pairing_codes = self.pairing_codes.lock().await;
        let window_start = now - Duration::from_secs(60);
        while pairing_codes
            .recent_redemptions
            .front()
            .is_some_and(|attempt| *attempt <= window_start)
        {
            pairing_codes.recent_redemptions.pop_front();
        }
        if pairing_codes.recent_redemptions.len() >= MAX_PAIRING_REDEMPTIONS_PER_MINUTE {
            anyhow::bail!("too many pairing-code redemption attempts");
        }
        pairing_codes.recent_redemptions.push_back(now);
        let entry = pairing_codes.codes.remove(code);
        drop(pairing_codes);
        let Some(entry) = entry else {
            anyhow::bail!("unknown pairing code");
        };
        if entry.expires_at <= Instant::now() {
            anyhow::bail!("expired pairing code");
        }
        Ok(entry.device_label)
    }
}

const MAX_PAIRING_CODES: usize = 1_024;

const MAX_PAIRING_REDEMPTIONS_PER_MINUTE: usize = 256;

#[derive(Default)]
pub(super) struct PairingCodes {
    codes: HashMap<String, PairingCode>,
    recent_redemptions: VecDeque<Instant>,
}

struct PairingCode {
    expires_at: Instant,
    device_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Device {
    pub device_label: String,
    pub node_id: String,
    pub created_ts: DateTime<Utc>,
    pub last_seen_ts: DateTime<Utc>,
    pub revoked_ts: Option<DateTime<Utc>>,
}

fn device_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Device> {
    let created_ts: String = row.get(2)?;
    let last_seen_ts: String = row.get(3)?;
    let revoked_ts: Option<String> = row.get(4)?;
    Ok(Device {
        device_label: row.get(0)?,
        node_id: row.get(1)?,
        created_ts: parse_ts(&created_ts)?,
        last_seen_ts: parse_ts(&last_seen_ts)?,
        revoked_ts: revoked_ts.as_deref().map(parse_ts).transpose()?,
    })
}

fn validate_device_label(device_label: &str) -> anyhow::Result<()> {
    if device_label.trim().is_empty() {
        anyhow::bail!("device label must not be empty");
    }
    if device_label.chars().count() > 128 {
        anyhow::bail!("device label must be at most 128 characters");
    }
    Ok(())
}

fn random_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests;
