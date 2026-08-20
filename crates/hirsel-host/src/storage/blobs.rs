//! Blob storage: raw files on disk plus their metadata rows.

use super::Storage;
use super::common::parse_ts;
use anyhow::Context;
use chrono::DateTime;
use chrono::Utc;
use hirsel_proto::Blob;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use rusqlite::types::Type;
use std::collections::HashSet;
use std::path::PathBuf;
use uuid::Uuid;

impl Storage {
    pub async fn store_blob(
        &self,
        client_id: &str,
        name: impl Into<String>,
        mime: impl Into<String>,
        data: Vec<u8>,
    ) -> anyhow::Result<StoredBlob> {
        tokio::fs::create_dir_all(self.blobs_dir.as_ref()).await?;
        let id = Uuid::new_v4().to_string();
        let path = self.blobs_dir.join(&id);
        // Readers resolve paths from SQLite; none scan the blob directory.
        // Publishing first makes a pre-commit crash a detectable orphan file,
        // never an unreadable metadata row.
        tokio::fs::write(&path, &data)
            .await
            .with_context(|| format!("write blob file {}", path.display()))?;

        let record = StoredBlob {
            blob: Blob {
                id: id.clone(),
                name: name.into(),
                mime: mime.into(),
                size: data.len() as u64,
            },
            path: path.clone(),
            created_ts: Utc::now(),
        };
        let metadata_result: anyhow::Result<Option<StoredBlob>> = async {
            let mut conn = self.conn.lock().await;
            let tx = conn.transaction()?;
            tx.execute(
                "
                INSERT INTO blobs (id, name, mime, size, path, created_ts)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ",
                params![
                    record.blob.id,
                    record.blob.name,
                    record.blob.mime,
                    record.blob.size,
                    record.path.to_string_lossy(),
                    record.created_ts.to_rfc3339()
                ],
            )?;
            let inserted = tx.execute(
                "
                INSERT INTO client_blobs (client_id, blob_id)
                VALUES (?1, ?2)
                ON CONFLICT(client_id) DO NOTHING
                ",
                params![client_id, id],
            )?;
            let duplicate = if inserted == 0 {
                tx.execute("DELETE FROM blobs WHERE id = ?1", params![id])?;
                Some(blob_for_client_id(&tx, client_id)?)
            } else {
                None
            };
            tx.commit()?;
            Ok(duplicate)
        }
        .await;

        let result = match metadata_result {
            Ok(None) => return Ok(record),
            Ok(Some(duplicate)) => Ok(duplicate),
            Err(error) => Err(error),
        };
        if let Err(error) = tokio::fs::remove_file(&path).await {
            tracing::debug!(%error, path = %path.display(), "failed to remove unreferenced blob file");
        }
        result
    }

    pub(super) async fn log_orphaned_blobs(&self) -> anyhow::Result<()> {
        for path in self.orphaned_blob_paths().await? {
            tracing::warn!(path = %path.display(), "orphaned blob file has no SQLite metadata");
        }
        Ok(())
    }

    async fn orphaned_blob_paths(&self) -> anyhow::Result<Vec<PathBuf>> {
        let known = {
            let conn = self.conn.lock().await;
            let mut stmt = conn.prepare("SELECT path FROM blobs")?;
            stmt.query_map([], |row| row.get::<_, String>(0))?
                .map(|path| path.map(PathBuf::from))
                .collect::<rusqlite::Result<HashSet<_>>>()?
        };
        let mut entries = tokio::fs::read_dir(self.blobs_dir.as_ref()).await?;
        let mut orphans = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if entry.file_type().await?.is_file() && !known.contains(&path) {
                orphans.push(path);
            }
        }
        orphans.sort();
        Ok(orphans)
    }

    pub async fn blob(&self, id: &str) -> anyhow::Result<Option<StoredBlob>> {
        let conn = self.conn.lock().await;
        get_stored_blob_optional(&conn, id).map_err(Into::into)
    }

    pub async fn blobs_for_message(&self, message_id: u64) -> anyhow::Result<Vec<StoredBlob>> {
        let conn = self.conn.lock().await;
        message_attachments(&conn, message_id).map_err(Into::into)
    }
}

fn blob_for_client_id(conn: &Connection, client_id: &str) -> rusqlite::Result<StoredBlob> {
    conn.query_row(
        "
        SELECT b.id, b.name, b.mime, b.size, b.path, b.created_ts
        FROM blobs b
        JOIN client_blobs cb ON cb.blob_id = b.id
        WHERE cb.client_id = ?1
        ",
        params![client_id],
        stored_blob_from_row,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBlob {
    pub blob: Blob,
    pub path: PathBuf,
    pub created_ts: DateTime<Utc>,
}

pub(super) fn validate_blob_ids(conn: &Connection, blob_ids: &[String]) -> anyhow::Result<()> {
    for blob_id in blob_ids {
        if get_stored_blob_optional(conn, blob_id)?.is_none() {
            anyhow::bail!("unknown blob id: {blob_id}");
        }
    }
    Ok(())
}

pub(super) fn message_attachments(
    conn: &Connection,
    message_id: u64,
) -> rusqlite::Result<Vec<StoredBlob>> {
    let mut stmt = conn.prepare(
        "
        SELECT b.id, b.name, b.mime, b.size, b.path, b.created_ts
        FROM message_attachments ma
        JOIN blobs b ON b.id = ma.blob_id
        WHERE ma.message_id = ?1
        ORDER BY ma.position ASC
        ",
    )?;
    let rows = stmt.query_map(params![message_id], stored_blob_from_row)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
}

fn get_stored_blob_optional(conn: &Connection, id: &str) -> rusqlite::Result<Option<StoredBlob>> {
    conn.query_row(
        "
        SELECT id, name, mime, size, path, created_ts
        FROM blobs
        WHERE id = ?1
        ",
        params![id],
        stored_blob_from_row,
    )
    .optional()
}

fn stored_blob_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredBlob> {
    let created_ts: String = row.get(5)?;
    Ok(StoredBlob {
        blob: Blob {
            id: row.get(0)?,
            name: row.get(1)?,
            mime: row.get(2)?,
            size: blob_size_from_row(row, 3)?,
        },
        path: PathBuf::from(row.get::<_, String>(4)?),
        created_ts: parse_ts(&created_ts)?,
    })
}

fn blob_size_from_row(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let size: i64 = row.get(index)?;
    u64::try_from(size).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, Type::Integer, Box::new(error))
    })
}

#[cfg(test)]
mod tests;
