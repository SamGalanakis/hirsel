//! Global artifact content and conversation references, committed together.
use super::{Storage, chat::get_chat_message, common::parse_ts, threads};
use hirsel_proto::{Artifact, ArtifactKind, ArtifactSummary, ChatMessage};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
pub(super) fn summary(c: &Connection, id: u64) -> anyhow::Result<ArtifactSummary> {
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
    artifact.thread_ids=c.prepare("SELECT m.thread_id FROM message_artifacts r JOIN chat_messages m ON m.id=r.message_id WHERE r.artifact_id=?1 UNION SELECT a.thread_id FROM activity_artifacts r JOIN thread_activities a ON a.id=r.activity_id WHERE r.artifact_id=?1 UNION SELECT id FROM threads WHERE showcased_artifact_id=?1 ORDER BY 1")?.query_map([id],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
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

pub(super) fn next_updated_at(c: &Connection, id: u64) -> anyhow::Result<String> {
    let previous: String =
        c.query_row("SELECT updated_at FROM artifacts WHERE id=?1", [id], |r| {
            r.get(0)
        })?;
    let previous = chrono::DateTime::parse_from_rfc3339(&previous)?.with_timezone(&chrono::Utc);
    Ok(chrono::Utc::now()
        .max(previous + chrono::Duration::nanoseconds(1))
        .to_rfc3339())
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
        let ids=c.prepare("SELECT a.id FROM artifacts a WHERE ?1 IS NULL OR EXISTS (SELECT 1 FROM message_artifacts r JOIN chat_messages m ON m.id=r.message_id WHERE r.artifact_id=a.id AND m.thread_id=?1) OR EXISTS (SELECT 1 FROM activity_artifacts r JOIN thread_activities t ON t.id=r.activity_id WHERE r.artifact_id=a.id AND t.thread_id=?1) OR EXISTS (SELECT 1 FROM threads t WHERE t.id=?1 AND t.showcased_artifact_id=a.id) ORDER BY a.updated_at DESC,a.id DESC")?.query_map([thread_id],|r|r.get::<_,u64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter().map(|id| summary(&c, id)).collect()
    }
    pub(crate) async fn artifact_operation(
        &self,
        operation_id: &str,
        caller: &super::ThreadCaller,
        input: &serde_json::Value,
    ) -> anyhow::Result<Option<(Artifact, Option<ChatMessage>)>> {
        let c = self.conn.lock().await;
        super::thread_scope::validate_caller(&c, caller)?;
        let thread_id = caller.thread_id;
        let receipt=c.query_row("SELECT payload,artifact_id,message_id FROM artifact_operations WHERE operation_id=?1",[operation_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,u64>(1)?,r.get::<_,Option<u64>>(2)?))).optional()?;
        receipt
            .map(|(payload, id, message_id)| {
                super::thread_scope::authorize_artifact(&c, thread_id, id)?;
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
    async fn publish_artifact_inner(
        &self,
        operation_id: &str,
        operation_input: &serde_json::Value,
        actor: Publication<'_>,
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
        let thread_id = match actor {
            Publication::Human(id) => id,
            Publication::Execution(caller) => caller.thread_id,
        };
        let payload = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(thread_id, operation_input))?)
        );
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        if let Publication::Execution(caller) = actor {
            super::thread_scope::validate_caller(&tx, caller)?;
            if let Some(id) = id {
                super::thread_scope::authorize_artifact(&tx, thread_id, id)?;
            }
        }
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
                let updated_at = next_updated_at(&tx, id)?;
                tx.execute("UPDATE artifacts SET title=?2,kind=?3,mime=?4,filename=?5,content=?6,updated_at=?7 WHERE id=?1",params![id,draft.title,serde_json::to_string(&draft.kind)?,draft.mime,draft.filename,draft.content,updated_at])?;
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
        if let Publication::Execution(caller) = actor {
            tx.execute(
                "INSERT OR IGNORE INTO turn_output_artifacts(turn_id,artifact_id) VALUES(?1,?2)",
                params![caller.turn_id, artifact_id],
            )?;
        }
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

impl Storage {
    pub(crate) async fn scoped_artifacts(
        &self,
        caller: &super::ThreadCaller,
        under: u64,
    ) -> anyhow::Result<Vec<ArtifactSummary>> {
        let c = self.conn.lock().await;
        super::thread_scope::validate_caller(&c, caller)?;
        super::thread_scope::authorize(&c, caller.thread_id, under)?;
        let ids=c.prepare("WITH RECURSIVE scope(id) AS (SELECT id FROM threads WHERE id=?1 UNION ALL SELECT t.id FROM threads t JOIN scope s ON t.parent_thread_id=s.id) SELECT r.artifact_id FROM message_artifacts r JOIN chat_messages m ON m.id=r.message_id JOIN scope s ON s.id=m.thread_id UNION SELECT r.artifact_id FROM activity_artifacts r JOIN thread_activities a ON a.id=r.activity_id JOIN scope s ON s.id=a.thread_id UNION SELECT t.showcased_artifact_id FROM threads t JOIN scope s ON s.id=t.id WHERE t.showcased_artifact_id IS NOT NULL ORDER BY 1")?.query_map([under],|r|r.get::<_,u64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| {
                let mut item = summary(&c, id)?;
                item.thread_ids
                    .retain(|id| super::thread_scope::authorize(&c, caller.thread_id, *id).is_ok());
                Ok(item)
            })
            .collect()
    }
}

impl Storage {
    pub(crate) async fn scoped_artifact(
        &self,
        caller: &super::ThreadCaller,
        id: u64,
    ) -> anyhow::Result<Artifact> {
        let c = self.conn.lock().await;
        super::thread_scope::validate_caller(&c, caller)?;
        super::thread_scope::authorize_artifact(&c, caller.thread_id, id)?;
        let mut artifact = get(&c, id)?;
        artifact
            .summary
            .thread_ids
            .retain(|id| super::thread_scope::authorize(&c, caller.thread_id, *id).is_ok());
        Ok(artifact)
    }
}

#[derive(Clone, Copy)]
enum Publication<'a> {
    Human(u64),
    Execution(&'a super::ThreadCaller),
}
impl Storage {
    pub(crate) async fn publish_artifact(
        &self,
        operation_id: &str,
        input: &serde_json::Value,
        caller: &super::ThreadCaller,
        id: Option<u64>,
        draft: Option<ArtifactDraft>,
    ) -> anyhow::Result<(Artifact, Option<ChatMessage>)> {
        self.publish_artifact_inner(
            operation_id,
            input,
            Publication::Execution(caller),
            id,
            draft,
        )
        .await
    }
    pub(crate) async fn publish_artifact_human(
        &self,
        operation_id: &str,
        input: &serde_json::Value,
        thread_id: u64,
        id: Option<u64>,
        draft: Option<ArtifactDraft>,
    ) -> anyhow::Result<(Artifact, Option<ChatMessage>)> {
        self.publish_artifact_inner(
            operation_id,
            input,
            Publication::Human(thread_id),
            id,
            draft,
        )
        .await
    }
}
