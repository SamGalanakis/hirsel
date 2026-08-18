use super::super::Storage;
use hirsel_proto::PushPlatform;
use rusqlite::Connection;

#[tokio::test]
async fn push_token_registration_upserts_and_unregisters() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();

    let created = storage
        .register_push_token(PushPlatform::Android, "token-1")
        .await
        .unwrap();
    let refreshed = storage
        .register_push_token(PushPlatform::Web, "token-1")
        .await
        .unwrap();

    assert_eq!(refreshed.token, "token-1");
    assert_eq!(refreshed.platform, PushPlatform::Web);
    assert_eq!(refreshed.created_ts, created.created_ts);
    assert!(refreshed.last_seen_ts >= created.last_seen_ts);
    assert_eq!(storage.push_tokens().await.unwrap(), vec![refreshed]);
    assert!(storage.unregister_push_token("token-1").await.unwrap());
    assert!(!storage.unregister_push_token("token-1").await.unwrap());
    assert!(storage.push_tokens().await.unwrap().is_empty());
}

#[tokio::test]
async fn push_token_table_is_additive_for_an_existing_database() {
    let dir = tempfile::tempdir().unwrap();
    {
        let conn = Connection::open(dir.path().join("hirsel.sqlite")).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE chat_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                author TEXT NOT NULL,
                body TEXT NOT NULL,
                ref INTEGER NULL,
                ts TEXT NOT NULL,
                tool_calls TEXT NOT NULL DEFAULT '[]'
            );
            INSERT INTO chat_messages (author, body, ref, ts)
            VALUES ('agent', 'existing row', NULL, '2026-07-10T12:00:00Z');
            ",
        )
        .unwrap();
    }

    let storage = Storage::open(dir.path()).await.unwrap();
    assert_eq!(storage.all_chat().await.unwrap().len(), 1);
    storage
        .register_push_token(PushPlatform::Ios, "token-live")
        .await
        .unwrap();
    drop(storage);

    let reopened = Storage::open(dir.path()).await.unwrap();
    assert_eq!(reopened.all_chat().await.unwrap().len(), 1);
    assert_eq!(reopened.push_tokens().await.unwrap().len(), 1);
}
