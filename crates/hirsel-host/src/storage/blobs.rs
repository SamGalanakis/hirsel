//! Blob storage: raw files on disk plus their immutable metadata rows.

use super::Storage;
use super::common::parse_ts;
use anyhow::Context;
use chrono::{DateTime, Utc};
use hirsel_proto::Blob;
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashSet;
use std::ffi::OsString;
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
        let expected_history = self.history_id().await?;
        tokio::fs::create_dir_all(self.blobs_dir.as_ref()).await?;
        let id = Uuid::new_v4().to_string();
        let path = self.blob_path(&id);
        // Publishing first makes a pre-commit crash a detectable orphan file,
        // never an unreadable metadata row.
        tokio::fs::write(&path, &data)
            .await
            .with_context(|| format!("write blob file {}", path.display()))?;

        #[cfg(test)]
        tests::pause(self.blobs_dir.as_ref(), "upload").await;

        let metadata = BlobMetadata {
            blob: Blob {
                id: id.clone(),
                name: name.into(),
                mime: mime.into(),
                size: data.len() as u64,
            },
            created_ts: Utc::now(),
        };
        let metadata_result: anyhow::Result<Option<BlobMetadata>> = async {
            let mut conn = self.conn.lock().await;
            let tx = conn.transaction()?;
            super::thread_scope::validate_history(&tx, &expected_history)?;
            tx.execute(
                "
                INSERT INTO blobs (id, name, mime, size, created_ts)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ",
                params![
                    metadata.blob.id,
                    metadata.blob.name,
                    metadata.blob.mime,
                    metadata.blob.size,
                    metadata.created_ts.to_rfc3339()
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
            Ok(None) => return Ok(self.resolve_blob(metadata)),
            Ok(Some(duplicate)) => Ok(self.resolve_blob(duplicate)),
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
            let mut stmt = conn.prepare("SELECT id FROM blobs")?;
            stmt.query_map([], |row| row.get::<_, String>(0))?
                .map(|id| id.map(OsString::from))
                .collect::<rusqlite::Result<HashSet<_>>>()?
        };
        let mut entries = tokio::fs::read_dir(self.blobs_dir.as_ref()).await?;
        let mut orphans = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_file() && !known.contains(&entry.file_name()) {
                orphans.push(entry.path());
            }
        }
        orphans.sort();
        Ok(orphans)
    }

    pub async fn blob(&self, id: &str) -> anyhow::Result<Option<StoredBlob>> {
        let metadata = {
            let conn = self.conn.lock().await;
            get_blob_optional(&conn, id)?
        };
        Ok(metadata.map(|metadata| self.resolve_blob(metadata)))
    }

    pub async fn blobs_for_message(&self, message_id: u64) -> anyhow::Result<Vec<StoredBlob>> {
        let metadata = {
            let conn = self.conn.lock().await;
            message_blob_metadata(&conn, message_id)?
        };
        Ok(metadata
            .into_iter()
            .map(|metadata| self.resolve_blob(metadata))
            .collect())
    }

    pub(crate) fn blob_path(&self, id: &str) -> PathBuf {
        self.blobs_dir.join(id)
    }

    pub(crate) async fn read_blob(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        let path = self.blob_path(id);
        tokio::fs::read(&path)
            .await
            .with_context(|| format!("read blob file {}", path.display()))
    }

    fn resolve_blob(&self, metadata: BlobMetadata) -> StoredBlob {
        let path = self.blob_path(&metadata.blob.id);
        StoredBlob {
            blob: metadata.blob,
            path,
            created_ts: metadata.created_ts,
        }
    }
}

fn blob_for_client_id(conn: &Connection, client_id: &str) -> rusqlite::Result<BlobMetadata> {
    conn.query_row(
        "
        SELECT b.id, b.name, b.mime, b.size, b.created_ts
        FROM blobs b
        JOIN client_blobs cb ON cb.blob_id = b.id
        WHERE cb.client_id = ?1
        ",
        params![client_id],
        blob_metadata_from_row,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBlob {
    pub blob: Blob,
    pub path: PathBuf,
    pub created_ts: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlobMetadata {
    blob: Blob,
    created_ts: DateTime<Utc>,
}

pub(super) fn validate_blob_ids(conn: &Connection, blob_ids: &[String]) -> anyhow::Result<()> {
    for blob_id in blob_ids {
        if get_blob_optional(conn, blob_id)?.is_none() {
            anyhow::bail!("unknown blob id: {blob_id}");
        }
    }
    Ok(())
}

pub(super) fn message_attachments(
    conn: &Connection,
    message_id: u64,
) -> rusqlite::Result<Vec<Blob>> {
    Ok(message_blob_metadata(conn, message_id)?
        .into_iter()
        .map(|metadata| metadata.blob)
        .collect())
}

fn message_blob_metadata(
    conn: &Connection,
    message_id: u64,
) -> rusqlite::Result<Vec<BlobMetadata>> {
    let mut stmt = conn.prepare(
        "
        SELECT b.id, b.name, b.mime, b.size, b.created_ts
        FROM message_attachments ma
        JOIN blobs b ON b.id = ma.blob_id
        WHERE ma.message_id = ?1
        ORDER BY ma.position ASC
        ",
    )?;
    let rows = stmt.query_map(params![message_id], blob_metadata_from_row)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
}

fn get_blob_optional(conn: &Connection, id: &str) -> rusqlite::Result<Option<BlobMetadata>> {
    conn.query_row(
        "
        SELECT id, name, mime, size, created_ts
        FROM blobs
        WHERE id = ?1
        ",
        params![id],
        blob_metadata_from_row,
    )
    .optional()
}

fn blob_metadata_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BlobMetadata> {
    let created_ts: String = row.get(4)?;
    Ok(BlobMetadata {
        blob: Blob {
            id: row.get(0)?,
            name: row.get(1)?,
            mime: row.get(2)?,
            size: blob_size_from_row(row, 3)?,
        },
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
pub(super) mod tests;
