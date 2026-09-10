use super::super::Storage;

#[tokio::test]
async fn blobs_are_stored_as_raw_files_and_idempotent_by_client_id() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();

    let first = storage
        .store_blob(
            "upload-1",
            "note.txt",
            "text/plain",
            b"first bytes".to_vec(),
        )
        .await
        .unwrap();
    let duplicate = storage
        .store_blob(
            "upload-1",
            "other.txt",
            "text/plain",
            b"first bytes".to_vec(),
        )
        .await
        .unwrap();

    assert_eq!(first, duplicate);
    let persisted = storage.blob(&first.blob.id).await.unwrap().unwrap();
    assert_eq!(persisted, first);
    assert!(persisted.path.is_file());
    assert_eq!(tokio::fs::read(&first.path).await.unwrap(), b"first bytes");
    assert_eq!(
        first.path.file_name().and_then(|name| name.to_str()),
        Some(first.blob.id.as_str())
    );
    assert!(first.path.is_absolute());
    let files = std::fs::read_dir(dir.path().join("blobs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(files, vec![first.path]);
}

#[tokio::test]
async fn orphan_scan_reports_files_without_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let orphan = dir.path().join("blobs").join("orphan-file");
    tokio::fs::write(&orphan, b"orphan").await.unwrap();

    assert_eq!(storage.orphaned_blob_paths().await.unwrap(), vec![orphan]);
}

#[tokio::test]
async fn owner_message_attachments_are_joined_and_replayed() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let thread = storage
        .create_thread(
            "conversation",
            "Conversation",
            "",
            &serde_json::Value::Null,
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    let text = storage
        .store_blob("text-upload", "note.txt", "text/plain", b"hello".to_vec())
        .await
        .unwrap();
    let image = storage
        .store_blob(
            "image-upload",
            "tiny.png",
            "image/png",
            vec![137, 80, 78, 71],
        )
        .await
        .unwrap();
    let attachment_ids = vec![text.blob.id.clone(), image.blob.id.clone()];

    let (message, inserted) = storage
        .append_thread_owner_message(
            &storage.history_id().await.unwrap(),
            thread.id,
            "client-1",
            "see attached",
            None,
            &attachment_ids,
            &[],
            &[],
        )
        .await
        .unwrap();
    let replay = storage.all_chat().await.unwrap();
    let stored_blobs = storage.blobs_for_message(message.id).await.unwrap();

    assert!(inserted);
    assert_eq!(
        message.attachments,
        vec![text.blob.clone(), image.blob.clone()]
    );
    assert_eq!(replay[0].attachments, message.attachments);
    assert_eq!(
        stored_blobs
            .iter()
            .map(|stored| stored.path.as_path())
            .collect::<Vec<_>>(),
        vec![text.path.as_path(), image.path.as_path()]
    );
}

#[tokio::test]
async fn owner_message_rejects_unknown_attachment_ids() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let thread = storage
        .create_thread(
            "conversation",
            "Conversation",
            "",
            &serde_json::Value::Null,
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;

    let error = storage
        .append_thread_owner_message(
            &storage.history_id().await.unwrap(),
            thread.id,
            "client-1",
            "missing attachment",
            None,
            &[String::from("missing-blob")],
            &[],
            &[],
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("unknown blob id: missing-blob"));
    assert!(storage.all_chat().await.unwrap().is_empty());
}

#[derive(Clone)]
struct Pause {
    arrived: std::sync::Arc<tokio::sync::Notify>,
    release: std::sync::Arc<tokio::sync::Notify>,
}
fn pauses()
-> &'static std::sync::Mutex<std::collections::HashMap<(std::path::PathBuf, &'static str), Pause>> {
    static PAUSES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<(std::path::PathBuf, &'static str), Pause>>,
    > = std::sync::OnceLock::new();
    PAUSES.get_or_init(Default::default)
}
fn install_pause(path: &std::path::Path, phase: &'static str) -> Pause {
    let pause = Pause {
        arrived: Default::default(),
        release: Default::default(),
    };
    pauses()
        .lock()
        .unwrap()
        .insert((path.to_owned(), phase), pause.clone());
    pause
}
pub(crate) async fn pause(path: &std::path::Path, phase: &'static str) {
    let pause = pauses().lock().unwrap().remove(&(path.to_owned(), phase));
    if let Some(pause) = pause {
        pause.arrived.notify_one();
        pause.release.notified().await;
    }
}

#[tokio::test]
async fn old_upload_cannot_publish_metadata_or_receipt_after_reset() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let old_history = storage.history_id().await.unwrap();
    let pause = install_pause(storage.blobs_dir.as_ref(), "upload");
    let uploading = storage.clone();
    let upload = tokio::spawn(async move {
        uploading
            .store_blob("same-client", "old", "text/plain", b"old".to_vec())
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), pause.arrived.notified())
        .await
        .unwrap();
    storage.reset().await.unwrap();
    assert_ne!(old_history, storage.history_id().await.unwrap());
    pause.release.notify_one();
    let error = upload.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("history"), "{error}");
    {
        let conn = storage.conn.lock().await;
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM blobs", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM client_blobs", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
    let fresh = storage
        .store_blob("same-client", "fresh", "text/plain", b"fresh".to_vec())
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(fresh.path).await.unwrap(), b"fresh");
}

#[tokio::test]
async fn new_upload_waits_for_reset_filesystem_cleanup_and_keeps_valid_file() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let old = storage
        .store_blob("old", "old", "text/plain", b"old".to_vec())
        .await
        .unwrap();
    let pause = install_pause(storage.blobs_dir.as_ref(), "reset");
    let resetting = storage.clone();
    let reset = tokio::spawn(async move { resetting.reset().await });
    tokio::time::timeout(std::time::Duration::from_secs(5), pause.arrived.notified())
        .await
        .unwrap();
    assert!(old.path.exists());
    let uploading = storage.clone();
    let mut upload = tokio::spawn(async move {
        uploading
            .store_blob("new", "new", "text/plain", b"new history bytes".to_vec())
            .await
    });
    // SQL has committed, but upload cannot even capture that history until file cleanup completes.
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut upload)
            .await
            .is_err()
    );
    pause.release.notify_one();
    reset.await.unwrap().unwrap();
    let fresh = upload.await.unwrap().unwrap();
    assert!(!old.path.exists());
    assert_eq!(
        storage.blob(&fresh.blob.id).await.unwrap(),
        Some(fresh.clone())
    );
    assert_eq!(
        tokio::fs::read(&fresh.path).await.unwrap(),
        b"new history bytes"
    );
    assert_eq!(
        storage
            .store_blob("new", "retry", "text/plain", b"ignored".to_vec())
            .await
            .unwrap(),
        fresh
    );
}
