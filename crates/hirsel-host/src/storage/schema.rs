//! The one supported store schema. Existing stores are never imported or altered.
use super::Storage;
use rusqlite::Connection;

const SCHEMA_VERSION: u32 = 16;

pub(super) fn state_list(terminal: Option<bool>) -> String {
    hirsel_proto::ThreadTurnState::ALL
        .into_iter()
        .filter(|state| terminal.is_none_or(|terminal| state.is_terminal() == terminal))
        .map(|state| {
            format!(
                "'{}'",
                serde_json::to_value(state)
                    .expect("turn state serializes")
                    .as_str()
                    .expect("turn state is a string")
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// The stored artifact discriminator, quoted for the artifacts CHECK. The list
/// comes from the enum itself so the store and the protocol cannot drift.
fn artifact_kind_list() -> String {
    hirsel_proto::ArtifactKind::TAGS
        .into_iter()
        .map(|tag| format!("'{tag}'"))
        .collect::<Vec<_>>()
        .join(",")
}

fn current_schema() -> String {
    include_str!("current.sql")
        .replace("$TURN_STATES", &state_list(None))
        .replace("$TERMINAL_TURN_STATES", &state_list(Some(true)))
        .replace("$ARTIFACT_KINDS", &artifact_kind_list())
}

/// Match the complete current layout before touching an existing store.
fn catalog(conn: &Connection) -> anyhow::Result<Vec<(String, String, String)>> {
    Ok(conn.prepare("SELECT type,name,sql FROM sqlite_master WHERE substr(name,1,7) != 'sqlite_' ORDER BY type,name")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
fn validate_existing(conn: &Connection) -> anyhow::Result<()> {
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(&current_schema())?;
    anyhow::ensure!(
        catalog(conn)? == catalog(&expected)?,
        "unsupported Hirsel history layout; existing store was not modified"
    );
    let history_id: String =
        conn.query_row("SELECT value FROM meta WHERE key='history_id'", [], |r| {
            r.get(0)
        })?;
    uuid::Uuid::parse_str(&history_id)?;
    Ok(())
}

pub(super) fn initialize(conn: &mut Connection) -> anyhow::Result<()> {
    let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let fresh = catalog(conn)?.is_empty();
    anyhow::ensure!(
        version == SCHEMA_VERSION || (version == 0 && fresh),
        "unsupported Hirsel history schema; back up the store and initialize a fresh current store"
    );
    if !fresh || version != 0 {
        validate_existing(conn)?;
    }
    // Validate schema and identity before any journal/schema writes.
    conn.execute_batch(
        "PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL;",
    )?;
    if fresh {
        let tx = conn.transaction()?;
        tx.execute_batch(&current_schema())?;
        tx.execute(
            "INSERT INTO meta(key,value) VALUES('history_id',?1)",
            [uuid::Uuid::new_v4().to_string()],
        )?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
    }
    Ok(())
}
impl Storage {
    pub async fn history_id(&self) -> anyhow::Result<String> {
        let conn = self.conn.lock().await;
        read_history_id(&conn)
    }
}

pub(super) fn read_history_id(conn: &Connection) -> anyhow::Result<String> {
    Ok(
        conn.query_row("SELECT value FROM meta WHERE key='history_id'", [], |r| {
            r.get(0)
        })?,
    )
}
#[cfg(test)]
mod tests;
