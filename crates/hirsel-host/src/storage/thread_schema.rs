//! One-time ownership import. Legacy activity after this migration is never work.
use rusqlite::{Connection, params};

pub(super) fn migrate(conn: &mut Connection) -> anyhow::Result<()> {
    // SQLite rejects ADD COLUMN REFERENCES with a non-NULL default while FK
    // enforcement is enabled, even though the orchestrator row exists. The
    // caller exclusively holds this connection. Validate the entire transaction
    // before committing and restore enforcement even when migration rolls back.
    let enforcement: bool = conn.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    conn.pragma_update(None, "foreign_keys", false)?;
    let migrated = migrate_checked(conn);
    let restored = conn.pragma_update(None, "foreign_keys", enforcement);
    migrated?;
    restored?;
    Ok(())
}

fn migrate_checked(conn: &mut Connection) -> anyhow::Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS threads (
        id INTEGER PRIMARY KEY AUTOINCREMENT, client_id TEXT UNIQUE,
        title TEXT NOT NULL, description TEXT NOT NULL, instrument TEXT NOT NULL,
        attention TEXT NOT NULL CHECK(attention IN ('quiet','needs_owner')),
        settled_at TEXT, archived_at TEXT, snoozed_until TEXT, read INTEGER NOT NULL,
        created_at TEXT NOT NULL, updated_at TEXT NOT NULL, revision INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS thread_action_receipts (client_id TEXT PRIMARY KEY, payload TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS thread_requests (id INTEGER PRIMARY KEY AUTOINCREMENT, client_id TEXT NOT NULL UNIQUE, payload TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS thread_turns (
        id INTEGER PRIMARY KEY AUTOINCREMENT, thread_id INTEGER NOT NULL REFERENCES threads(id),
        owner_message_id INTEGER UNIQUE, agent_message_id INTEGER, state TEXT NOT NULL,
        started_at TEXT NOT NULL, finished_at TEXT);
        CREATE TABLE IF NOT EXISTS thread_activities (
        id INTEGER PRIMARY KEY AUTOINCREMENT, thread_id INTEGER NOT NULL REFERENCES threads(id),
        turn_id INTEGER REFERENCES thread_turns(id), kind TEXT NOT NULL, data TEXT NOT NULL, ts TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS thread_activity_keys (key TEXT PRIMARY KEY, activity_id INTEGER NOT NULL REFERENCES thread_activities(id));
        CREATE INDEX IF NOT EXISTS thread_turns_thread ON thread_turns(thread_id,id);
        CREATE INDEX IF NOT EXISTS thread_activities_thread ON thread_activities(thread_id,id);")?;
    let now = chrono::Utc::now().to_rfc3339();
    tx.execute("INSERT OR IGNORE INTO threads VALUES(0,NULL,'Orchestrator','', '{}','quiet',NULL,NULL,NULL,1,?1,?1,1)",params![now])?;
    let columns = tx
        .prepare("PRAGMA table_info(chat_messages)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|c| c == "thread_id") {
        tx.execute("ALTER TABLE chat_messages ADD COLUMN thread_id INTEGER NOT NULL DEFAULT 0 REFERENCES threads(id)",[])?;
        tx.execute(
            "ALTER TABLE chat_messages ADD COLUMN mentions TEXT NOT NULL DEFAULT '[]'",
            [],
        )?;
    }
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS chat_messages_thread ON chat_messages(thread_id,id)",
    )?;
    let imported: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM meta WHERE key='threads_import_v1')",
        [],
        |r| r.get(0),
    )?;
    if !imported {
        tx.execute_batch("INSERT INTO threads(id,title,description,instrument,attention,settled_at,archived_at,snoozed_until,read,created_at,updated_at,revision)
            SELECT id,name,description,ui,CASE WHEN requires_response THEN 'needs_owner' ELSE 'quiet' END,
            CASE WHEN status='done' THEN ts END,CASE WHEN archived THEN COALESCE(archived_at,ts) END,
            snoozed_until,read,ts,ts,1 FROM pings WHERE name != 'session-rotated';
            INSERT INTO thread_activities(thread_id,kind,data,ts) SELECT 0,'legacy_notification',json_object('event_id',id,'name',name,'description',description,'instrument',json(ui)),ts FROM pings WHERE name='session-rotated';
            WITH RECURSIVE ownership(message_id,thread_id) AS (
              SELECT anchor,id FROM pings WHERE anchor > 0 AND name != 'session-rotated'
              UNION
              SELECT m.id,o.thread_id FROM chat_messages m JOIN ownership o ON m.ref=o.message_id
            ), unique_owners AS (SELECT message_id,MIN(thread_id) thread_id FROM ownership GROUP BY message_id HAVING COUNT(DISTINCT thread_id)=1)
            UPDATE chat_messages SET thread_id=COALESCE((SELECT thread_id FROM unique_owners WHERE message_id=chat_messages.id),0);
            INSERT INTO meta(key,value) VALUES('threads_import_v1','complete');")?;
    }
    let violations: usize =
        tx.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    anyhow::ensure!(
        violations == 0,
        "thread import would leave {violations} foreign key violations"
    );
    tx.commit()?;
    Ok(())
}
