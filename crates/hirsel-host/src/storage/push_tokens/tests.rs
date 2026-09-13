use super::super::Storage;
use hirsel_proto::PushPlatform;

#[tokio::test]
async fn push_token_registration_upserts_and_unregisters() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let device_token = storage
        .issue_device_token("Owner phone", "node-a")
        .await
        .unwrap();
    let other_device = storage
        .issue_device_token("Owner tablet", "node-b")
        .await
        .unwrap();

    let created = storage
        .register_push_token(&device_token, PushPlatform::Android, "token-1")
        .await
        .unwrap();
    let refreshed = storage
        .register_push_token(&device_token, PushPlatform::Web, "token-1")
        .await
        .unwrap();

    assert_eq!(refreshed.token, "token-1");
    assert_eq!(refreshed.platform, PushPlatform::Web);
    assert_eq!(refreshed.created_ts, created.created_ts);
    assert!(refreshed.last_seen_ts >= created.last_seen_ts);
    assert_eq!(storage.active_push_tokens().await.unwrap(), vec![refreshed]);
    assert!(
        !storage
            .unregister_push_token(&other_device, "token-1")
            .await
            .unwrap()
    );
    assert!(
        storage
            .unregister_push_token(&device_token, "token-1")
            .await
            .unwrap()
    );
    assert!(
        !storage
            .unregister_push_token(&device_token, "token-1")
            .await
            .unwrap()
    );
    assert!(storage.active_push_tokens().await.unwrap().is_empty());
}

#[tokio::test]
async fn revoked_devices_are_excluded_and_push_rows_enforce_device_and_platform() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let active_device = storage
        .issue_device_token("Owner phone", "node-a")
        .await
        .unwrap();
    let revoked_device = storage
        .issue_device_token("Owner tablet", "node-b")
        .await
        .unwrap();
    storage
        .register_push_token(&active_device, PushPlatform::Android, "active-push")
        .await
        .unwrap();
    storage
        .register_push_token(&revoked_device, PushPlatform::Ios, "revoked-push")
        .await
        .unwrap();
    assert_eq!(storage.revoke_device("Owner tablet").await.unwrap(), 1);

    let active = storage.active_push_tokens().await.unwrap();
    assert_eq!(
        active
            .into_iter()
            .map(|push| push.token)
            .collect::<Vec<_>>(),
        vec!["active-push"]
    );
    assert!(
        storage
            .register_push_token("unknown-device", PushPlatform::Web, "orphan")
            .await
            .is_err()
    );

    let conn = storage.conn.lock().await;
    let now = chrono::Utc::now().to_rfc3339();
    assert!(
        conn.execute(
            "INSERT INTO push_tokens(token,device_token,platform,created_ts,last_seen_ts) VALUES(?1,?2,'desktop',?3,?3)",
            rusqlite::params!["invalid-platform", active_device, now],
        )
        .is_err()
    );
}

#[tokio::test]
async fn push_tokens_survive_reopening_current_store() {
    let dir = tempfile::tempdir().unwrap();
    {
        let storage = Storage::open(dir.path()).await.unwrap();
        storage
            .append_thread_chat(
                storage
                    .create_thread(
                        "fixture-Conversation",
                        "Conversation",
                        "",
                        None,
                        hirsel_proto::ThreadAttention::Quiet,
                        hirsel_proto::ThreadKind::Task,
                        None,
                    )
                    .await
                    .unwrap()
                    .0
                    .id,
                hirsel_proto::ChatAuthor::Agent,
                "existing row",
                None,
                vec![],
            )
            .await
            .unwrap();
    }

    let storage = Storage::open(dir.path()).await.unwrap();
    assert_eq!(storage.all_chat().await.unwrap().len(), 1);
    let device_token = storage
        .issue_device_token("Owner phone", "node-a")
        .await
        .unwrap();
    storage
        .register_push_token(&device_token, PushPlatform::Ios, "token-live")
        .await
        .unwrap();
    drop(storage);

    let reopened = Storage::open(dir.path()).await.unwrap();
    assert_eq!(reopened.all_chat().await.unwrap().len(), 1);
    assert_eq!(reopened.active_push_tokens().await.unwrap().len(), 1);
}
