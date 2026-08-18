use super::super::Storage;
use super::MAX_PAIRING_REDEMPTIONS_PER_MINUTE;
use rusqlite::Connection;
use std::time::Duration;

#[tokio::test]
async fn device_tokens_are_pinned_revocable_and_additive() {
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
    let token = storage
        .issue_device_token("Owner phone", "node-a")
        .await
        .unwrap();
    assert_eq!(token.len(), 64);
    storage
        .authenticate_device_token(&token, Some("node-a"))
        .await
        .unwrap();
    assert!(
        storage
            .authenticate_device_token(&token, Some("node-b"))
            .await
            .is_err()
    );
    assert_eq!(storage.list_devices().await.unwrap().len(), 1);
    assert_eq!(storage.revoke_device("Owner phone").await.unwrap(), 1);
    assert!(
        storage
            .authenticate_device_token(&token, Some("node-a"))
            .await
            .is_err()
    );
    drop(storage);

    let reopened = Storage::open(dir.path()).await.unwrap();
    assert_eq!(reopened.all_chat().await.unwrap().len(), 1);
    let devices = reopened.list_devices().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].device_label, "Owner phone");
    assert!(devices[0].revoked_ts.is_some());
}

#[tokio::test]
async fn pairing_codes_are_long_single_use_and_expire() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();

    let code = storage
        .mint_pairing_code("Owner phone", Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(code.len(), 64);
    assert_eq!(
        storage.redeem_pairing_code(&code).await.unwrap(),
        "Owner phone"
    );
    assert!(storage.redeem_pairing_code(&code).await.is_err());

    let expired = storage
        .mint_pairing_code("Old phone", Duration::ZERO)
        .await
        .unwrap();
    assert!(storage.redeem_pairing_code(&expired).await.is_err());
}

#[tokio::test]
async fn pairing_code_redemption_attempts_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let code = storage
        .mint_pairing_code("Rate limited phone", Duration::from_secs(60))
        .await
        .unwrap();

    for attempt in 0..MAX_PAIRING_REDEMPTIONS_PER_MINUTE {
        assert!(
            storage
                .redeem_pairing_code(&format!("unknown-{attempt}"))
                .await
                .is_err()
        );
    }
    let error = storage.redeem_pairing_code(&code).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "too many pairing-code redemption attempts"
    );
}
