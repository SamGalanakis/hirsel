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
            b"other bytes".to_vec(),
        )
        .await
        .unwrap();

    assert_eq!(first, duplicate);
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
        .append_owner_message("client-1", "see attached", None, &attachment_ids)
        .await
        .unwrap();
    let replay = storage.replay_messages(None).await.unwrap();
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

    let error = storage
        .append_owner_message(
            "client-1",
            "missing attachment",
            None,
            &[String::from("missing-blob")],
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("unknown blob id: missing-blob"));
    assert!(storage.all_chat().await.unwrap().is_empty());
}
