//! Global artifact content and conversation references, committed together.
use super::{Storage, chat::get_chat_message, common::parse_ts, threads};
use hirsel_proto::{Artifact, ArtifactKind, ArtifactSummary, ChatMessage};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(super) fn migrate(c: &Connection) -> anyhow::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS artifacts (
        id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, kind TEXT NOT NULL,
        mime TEXT NOT NULL, filename TEXT, content TEXT NOT NULL,
        created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS message_artifacts (
        message_id INTEGER NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
        artifact_id INTEGER NOT NULL REFERENCES artifacts(id), PRIMARY KEY(message_id,artifact_id));
        CREATE INDEX IF NOT EXISTS message_artifacts_by_artifact ON message_artifacts(artifact_id,message_id);
        CREATE TABLE IF NOT EXISTS artifact_operations (
        operation_id TEXT PRIMARY KEY, payload TEXT NOT NULL,
        artifact_id INTEGER NOT NULL REFERENCES artifacts(id),
        message_id INTEGER REFERENCES chat_messages(id) ON DELETE SET NULL);")?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArtifactDraft {
    pub title: String,
    pub kind: ArtifactKind,
    pub mime: String,
    pub filename: Option<String>,
    pub content: String,
    #[serde(skip)]
    pub expected_content: Option<String>,
}
impl ArtifactDraft {
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.title.trim().is_empty() && self.title.len() <= 200,
            "title must contain 1–200 bytes"
        );
        anyhow::ensure!(
            self.content.len() <= 1_048_576,
            "artifact content exceeds 1 MiB"
        );
        anyhow::ensure!(
            !self.mime.is_empty()
                && self.mime.len() <= 120
                && !self.mime.chars().any(char::is_control),
            "invalid artifact MIME type"
        );
        if let Some(name) = &self.filename {
            anyhow::ensure!(
                !name.is_empty()
                    && name.len() <= 255
                    && !name.contains(['/', '\\'])
                    && !name.chars().any(char::is_control),
                "filename must be a simple name of 1–255 bytes"
            );
        }
        Ok(())
    }
}

pub(super) fn message_artifacts(c: &Connection, message_id: u64) -> rusqlite::Result<Vec<u64>> {
    c.prepare("SELECT artifact_id FROM message_artifacts WHERE message_id=?1 ORDER BY artifact_id")?
        .query_map([message_id], |r| r.get(0))?
        .collect()
}
fn summary(c: &Connection, id: u64) -> anyhow::Result<ArtifactSummary> {
    let mut artifact = c
        .query_row(
            "SELECT id,title,kind,mime,filename,created_at,updated_at FROM artifacts WHERE id=?1",
            [id],
            |r| {
                let kind: String = r.get(2)?;
                Ok(ArtifactSummary {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    kind: serde_json::from_str(&kind).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    mime: r.get(3)?,
                    filename: r.get(4)?,
                    created_at: parse_ts(&r.get::<_, String>(5)?)?,
                    updated_at: parse_ts(&r.get::<_, String>(6)?)?,
                    thread_ids: vec![],
                })
            },
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("artifact {id} does not exist"))?;
    artifact.thread_ids=c.prepare("SELECT DISTINCT m.thread_id FROM message_artifacts r JOIN chat_messages m ON m.id=r.message_id WHERE r.artifact_id=?1 ORDER BY m.thread_id")?.query_map([id],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(artifact)
}
fn get(c: &Connection, id: u64) -> anyhow::Result<Artifact> {
    Ok(Artifact {
        summary: summary(c, id)?,
        content: c.query_row("SELECT content FROM artifacts WHERE id=?1", [id], |r| {
            r.get(0)
        })?,
    })
}
impl Storage {
    pub async fn artifact(&self, id: u64) -> anyhow::Result<Artifact> {
        get(&*self.conn.lock().await, id)
    }
    pub async fn artifacts(&self, thread_id: Option<u64>) -> anyhow::Result<Vec<ArtifactSummary>> {
        let c = self.conn.lock().await;
        if let Some(id) = thread_id {
            threads::get(&c, id)?;
        }
        let ids=c.prepare("SELECT a.id FROM artifacts a WHERE ?1 IS NULL OR EXISTS (SELECT 1 FROM message_artifacts r JOIN chat_messages m ON m.id=r.message_id WHERE r.artifact_id=a.id AND m.thread_id=?1) ORDER BY a.updated_at DESC,a.id DESC")?.query_map([thread_id],|r|r.get::<_,u64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter().map(|id| summary(&c, id)).collect()
    }
    pub(crate) async fn artifact_operation(
        &self,
        operation_id: &str,
        thread_id: u64,
        input: &serde_json::Value,
    ) -> anyhow::Result<Option<(Artifact, Option<ChatMessage>)>> {
        let c = self.conn.lock().await;
        let receipt=c.query_row("SELECT payload,artifact_id,message_id FROM artifact_operations WHERE operation_id=?1",[operation_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,u64>(1)?,r.get::<_,Option<u64>>(2)?))).optional()?;
        receipt
            .map(|(payload, id, message_id)| {
                anyhow::ensure!(
                    payload
                        == format!(
                            "{:x}",
                            Sha256::digest(serde_json::to_vec(&(thread_id, input))?)
                        ),
                    "artifact operation ID was already used with different input"
                );
                Ok((
                    get(&c, id)?,
                    message_id.map(|id| get_chat_message(&c, id)).transpose()?,
                ))
            })
            .transpose()
    }
    /// Create (no id), overwrite (id + draft), or show (id only). Content is inert
    /// text here. A durable execution key makes replay safe even after later edits.
    pub(crate) async fn publish_artifact(
        &self,
        operation_id: &str,
        operation_input: &serde_json::Value,
        thread_id: u64,
        id: Option<u64>,
        draft: Option<ArtifactDraft>,
    ) -> anyhow::Result<(Artifact, Option<ChatMessage>)> {
        anyhow::ensure!(
            !operation_id.is_empty() && operation_id.len() <= 2048,
            "invalid artifact operation ID"
        );
        anyhow::ensure!(
            id.is_some() || draft.is_some(),
            "artifact content is required"
        );
        if let Some(draft) = &draft {
            draft.validate()?;
        }
        let payload = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(thread_id, operation_input))?)
        );
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        threads::get(&tx, thread_id)?;
        if let Some((old_payload,artifact_id,message_id))=tx.query_row("SELECT payload,artifact_id,message_id FROM artifact_operations WHERE operation_id=?1",[operation_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,u64>(1)?,r.get::<_,Option<u64>>(2)?))).optional()? {
            anyhow::ensure!(old_payload==payload,"artifact operation ID was already used with different input");
            let result=(get(&tx,artifact_id)?,message_id.map(|id|get_chat_message(&tx,id)).transpose()?);
            tx.commit()?;
            return Ok(result);
        }
        let ts = chrono::Utc::now().to_rfc3339();
        let artifact_id = match (id, &draft) {
            (Some(id), Some(draft)) => {
                let current = get(&tx, id)?;
                if let Some(expected) = &draft.expected_content {
                    anyhow::ensure!(
                        &current.content == expected,
                        "artifact changed while editing; read it and retry"
                    );
                }
                tx.execute("UPDATE artifacts SET title=?2,kind=?3,mime=?4,filename=?5,content=?6,updated_at=?7 WHERE id=?1",params![id,draft.title,serde_json::to_string(&draft.kind)?,draft.mime,draft.filename,draft.content,ts])?;
                id
            }
            (None, Some(draft)) => {
                tx.execute("INSERT INTO artifacts(title,kind,mime,filename,content,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?6)",params![draft.title,serde_json::to_string(&draft.kind)?,draft.mime,draft.filename,draft.content,ts])?;
                tx.last_insert_rowid() as u64
            }
            (Some(id), None) => {
                summary(&tx, id)?;
                id
            }
            (None, None) => unreachable!("validated above"),
        };
        // Every explicit publication is visible in the addressed conversation.
        // The message stores only identity: even old cards open current content.
        let title = summary(&tx, artifact_id)?.title;
        tx.execute(
            "INSERT INTO chat_messages(author,body,ts,thread_id) VALUES('agent',?1,?2,?3)",
            params![format!("Artifact: {title}"), ts, thread_id],
        )?;
        let message_id = tx.last_insert_rowid() as u64;
        tx.execute(
            "INSERT INTO message_artifacts(message_id,artifact_id) VALUES(?1,?2)",
            params![message_id, artifact_id],
        )?;
        // Store a digest, rather than another copy of old content, as the receipt.
        tx.execute("INSERT INTO artifact_operations(operation_id,payload,artifact_id,message_id) VALUES(?1,?2,?3,?4)",params![operation_id,payload,artifact_id,message_id])?;
        let result = (
            get(&tx, artifact_id)?,
            Some(get_chat_message(&tx, message_id)?),
        );
        tx.commit()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
