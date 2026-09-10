//! Explicit Thread URL references. URLs carry no conversation authority.
use super::{Storage, thread_scope, threads};
use hirsel_proto::{Thread, ThreadRelatedItem, ThreadRelatedTarget};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ThreadRelated {
    pub history_id: String,
    pub thread_id: u64,
    pub thread: Thread,
    pub revision: u64,
    pub related_items: Vec<ThreadRelatedItem>,
}

fn canonical_url(value: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        value.len() <= 4096 && value.trim() == value && !value.chars().any(char::is_control),
        "URL must be at most 4096 bytes without surrounding whitespace or control characters"
    );
    let authority = value
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or_default());
    anyhow::ensure!(
        authority.is_some_and(|value| !value.is_empty() && !value.contains('@'))
            && !value.contains('\\'),
        "Related links require an absolute URL without credentials or backslashes"
    );
    let parsed = url::Url::parse(value).map_err(|_| anyhow::anyhow!("invalid URL"))?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "http" | "https")
            && parsed.host_str().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none(),
        "Related links require an absolute HTTP(S) URL without credentials"
    );
    let url = parsed.to_string();
    anyhow::ensure!(url.len() <= 4096, "URL exceeds 4096 bytes");
    Ok(url)
}
fn normalized_title(value: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(value) = value else { return Ok(None) };
    anyhow::ensure!(
        value.chars().count() <= 200 && !value.chars().any(char::is_control),
        "link title must be at most 200 characters without control characters"
    );
    Ok((!value.trim().is_empty()).then(|| value.trim().to_string()))
}
pub(super) fn list(c: &Connection, thread_id: u64) -> anyhow::Result<Vec<ThreadRelatedItem>> {
    let history_id: String =
        c.query_row("SELECT value FROM meta WHERE key='history_id'", [], |r| {
            r.get(0)
        })?;
    Ok(c.prepare("SELECT id,thread_id,url,target_thread_id,title,created_at FROM thread_related_items WHERE thread_id=?1 ORDER BY id")?
        .query_map([thread_id], |r| {
            let url:Option<String>=r.get(2)?;
            let target=match url { Some(url)=>ThreadRelatedTarget::Url {url},None=>ThreadRelatedTarget::Thread {history_id:history_id.clone(),thread_id:r.get(3)?} };
            Ok(ThreadRelatedItem {id:r.get(0)?,thread_id:r.get(1)?,target,title:r.get(4)?,created_at:super::common::parse_ts(&r.get::<_,String>(5)?)?})
        })?.collect::<rusqlite::Result<_>>()?)
}
pub(super) fn list_scoped(
    c: &Connection,
    thread_id: u64,
    caller: u64,
) -> anyhow::Result<Vec<ThreadRelatedItem>> {
    Ok(list(c, thread_id)?
        .into_iter()
        .filter(|item| match &item.target {
            ThreadRelatedTarget::Url { .. } => true,
            ThreadRelatedTarget::Thread { thread_id, .. } => {
                thread_scope::authorize(c, caller, *thread_id).is_ok()
            }
        })
        .collect())
}
pub(super) fn snapshot(c: &Connection, thread_id: u64) -> anyhow::Result<ThreadRelated> {
    let thread = threads::get(c, thread_id)?;
    Ok(ThreadRelated {
        history_id: c.query_row("SELECT value FROM meta WHERE key='history_id'", [], |r| {
            r.get(0)
        })?,
        thread_id,
        revision: thread.revision,
        thread,
        related_items: list(c, thread_id)?,
    })
}
fn advance(c: &Connection, thread_id: u64) -> anyhow::Result<()> {
    c.execute(
        "UPDATE threads SET revision=revision+1,updated_at=?2 WHERE id=?1",
        params![thread_id, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn normalize(
    c: &Connection,
    target: &ThreadRelatedTarget,
    title: Option<&str>,
) -> anyhow::Result<(ThreadRelatedTarget, Option<String>)> {
    let original_title_absent = title.is_none();
    let title = normalized_title(title)?;
    let target = match target {
        ThreadRelatedTarget::Url { url } => ThreadRelatedTarget::Url {
            url: canonical_url(url)?,
        },
        ThreadRelatedTarget::Thread {
            history_id,
            thread_id,
        } => {
            thread_scope::validate_history(c, history_id)?;
            anyhow::ensure!(
                original_title_absent,
                "Thread references use the current Thread title; omit title"
            );
            threads::get(c, *thread_id)?;
            target.clone()
        }
    };
    Ok((target, title))
}
pub(super) fn add(
    c: &Connection,
    thread_id: u64,
    target: &ThreadRelatedTarget,
    title: Option<&str>,
) -> anyhow::Result<ThreadRelated> {
    threads::get(c, thread_id)?;
    let (target, title) = normalize(c, target, title)?;
    let (url, target_id) = match target {
        ThreadRelatedTarget::Url { url } => (Some(url), None),
        ThreadRelatedTarget::Thread { thread_id, .. } => (None, Some(thread_id)),
    };
    let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM thread_related_items WHERE thread_id=?1 AND ((?2 IS NOT NULL AND url=?2) OR (?3 IS NOT NULL AND target_thread_id=?3)))",params![thread_id,url,target_id],|r|r.get(0))?;
    if !exists {
        let count: u64 = c.query_row(
            "SELECT count(*) FROM thread_related_items WHERE thread_id=?1",
            [thread_id],
            |r| r.get(0),
        )?;
        anyhow::ensure!(count < 100, "Thread already has 100 Related references");
        c.execute("INSERT INTO thread_related_items(thread_id,url,target_thread_id,title,created_at) VALUES(?1,?2,?3,?4,?5)",params![thread_id,url,target_id,title,chrono::Utc::now().to_rfc3339()])?;
        advance(c, thread_id)?;
    }
    snapshot(c, thread_id)
}
pub(super) fn remove(
    c: &Connection,
    thread_id: u64,
    item_id: u64,
) -> anyhow::Result<ThreadRelated> {
    threads::get(c, thread_id)?;
    if c.execute(
        "DELETE FROM thread_related_items WHERE id=?1 AND thread_id=?2",
        params![item_id, thread_id],
    )? > 0
    {
        advance(c, thread_id)?;
    }
    snapshot(c, thread_id)
}
fn replay(c: &Connection, client_id: &str, payload: &str) -> anyhow::Result<bool> {
    anyhow::ensure!(
        !client_id.trim().is_empty() && client_id.len() <= 200,
        "Related client_id must be 1..200 bytes"
    );
    let old: Option<String> = c
        .query_row(
            "SELECT payload FROM thread_related_receipts WHERE client_id=?1",
            [client_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(old) = old {
        anyhow::ensure!(old == payload, "Related client_id payload changed");
        Ok(true)
    } else {
        Ok(false)
    }
}
fn record(c: &Connection, client_id: &str, payload: &str) -> anyhow::Result<()> {
    c.execute(
        "INSERT INTO thread_related_receipts(client_id,payload) VALUES(?1,?2)",
        params![client_id, payload],
    )?;
    Ok(())
}
impl Storage {
    pub(crate) async fn related_publication_snapshot(
        &self,
        history_id: &str,
        thread_id: u64,
    ) -> anyhow::Result<(tokio::sync::MutexGuard<'_, Connection>, ThreadRelated)> {
        let guard = self.conn.lock().await;
        thread_scope::validate_history(&guard, history_id)?;
        let current = snapshot(&guard, thread_id)?;
        Ok((guard, current))
    }
    pub(crate) async fn add_thread_related(
        &self,
        client_id: &str,
        history_id: &str,
        thread_id: u64,
        target: &ThreadRelatedTarget,
        title: Option<&str>,
    ) -> anyhow::Result<ThreadRelated> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        thread_scope::validate_history(&tx, history_id)?;
        threads::get(&tx, thread_id)?;
        let (target, title) = normalize(&tx, target, title)?;
        let payload = serde_json::to_string(
            &serde_json::json!({"operation":"add","history_id":history_id,"thread_id":thread_id,"target":target,"title":title}),
        )?;
        let result = if replay(&tx, client_id, &payload)? {
            snapshot(&tx, thread_id)?
        } else {
            let result = add(&tx, thread_id, &target, title.as_deref())?;
            record(&tx, client_id, &payload)?;
            result
        };
        tx.commit()?;
        Ok(result)
    }
    pub(crate) async fn remove_thread_related(
        &self,
        client_id: &str,
        history_id: &str,
        thread_id: u64,
        item_id: u64,
    ) -> anyhow::Result<ThreadRelated> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        thread_scope::validate_history(&tx, history_id)?;
        threads::get(&tx, thread_id)?;
        let payload = serde_json::to_string(
            &serde_json::json!({"operation":"remove","history_id":history_id,"thread_id":thread_id,"item_id":item_id}),
        )?;
        let result = if replay(&tx, client_id, &payload)? {
            snapshot(&tx, thread_id)?
        } else {
            let result = remove(&tx, thread_id, item_id)?;
            record(&tx, client_id, &payload)?;
            result
        };
        tx.commit()?;
        Ok(result)
    }
}
#[cfg(test)]
#[path = "thread_related_tests.rs"]
mod tests;
