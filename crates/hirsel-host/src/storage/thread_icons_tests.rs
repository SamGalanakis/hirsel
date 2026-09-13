use super::*;
use crate::lash_runtime::ScopedThreadTools;
use hirsel_proto::{HostToClient, ThreadAttention};
use serde_json::json;

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
        width,
        height,
        image::Rgba([20, 120, 220, 255]),
    ));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

#[test]
fn compact_icon_validation_preserves_composed_emoji_and_null_semantics() {
    assert_eq!(parse_icon(&json!({})).unwrap(), None);
    assert_eq!(parse_icon(&json!({"icon":null})).unwrap(), Some(None));
    for value in ["🧑🏽‍💻", "👨‍👩‍👧‍👦", "☀️", "★", "🐙".repeat(16).as_str()]
    {
        assert_eq!(
            parse_icon(&json!({"icon":{"kind":"emoji","value":value}})).unwrap(),
            Some(Some(hirsel_proto::ThreadIcon::Emoji {
                value: value.into()
            }))
        );
    }
    for value in [
        "",
        " \t",
        "x\n",
        "x\r",
        "x\0",
        "x\u{0085}",
        "x\u{2028}",
        "x\u{2029}",
        "🐙".repeat(17).as_str(),
    ] {
        assert!(
            parse_icon(&json!({"icon":{"kind":"emoji","value":value}})).is_err(),
            "{value:?}"
        );
    }
    for value in [json!(42), json!({}), json!([]), json!(false), json!("🐙")] {
        assert!(parse_icon(&json!({"icon":value})).is_err());
    }
}

#[tokio::test]
async fn icons_roundtrip_through_agent_and_owner_edits_with_replay_and_revision_guards() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let root = state
        .storage
        .create_thread(
            "root",
            "Root",
            "",
            None,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap()
        .0;
    let turn = state
        .storage
        .start_thread_turn(root.id, None)
        .await
        .unwrap();
    let history = state.storage.history_id().await.unwrap();
    let caller = state
        .storage
        .bind_thread_execution(&history, "icons", "icons", turn.id)
        .await
        .unwrap();
    let mut tools = ScopedThreadTools {
        tools: state.tools.clone(),
        caller,
        operation_id: "create".into(),
    };
    let created = tools
        .execute(
            "threads_create",
            &json!({"client_id":"child","kind":"task","title":"Research","icon":{"kind":"emoji","value":"🔬"}}),
        )
        .await
        .unwrap();
    let id = created["thread_id"].as_u64().unwrap();
    assert_eq!(
        created["thread"]["icon"],
        json!({"kind":"emoji","value":"🔬"})
    );
    let replayed = tools
        .execute(
            "threads_create",
            &json!({"client_id":"child","kind":"task","title":"Research","icon":{"kind":"emoji","value":"🔬"}}),
        )
        .await
        .unwrap();
    assert_eq!(created, replayed);
    tools.operation_id = "preserve".into();
    let preserved = tools
        .execute(
            "threads_update",
            &json!({"thread":id,"title":"Better title"}),
        )
        .await
        .unwrap();
    assert_eq!(
        preserved["thread"]["icon"],
        json!({"kind":"emoji","value":"🔬"})
    );
    assert!(
        tools
            .execute(
                "threads_update",
                &json!({"thread":id,"title":"Better title","icon":null})
            )
            .await
            .unwrap_err()
            .contains("payload changed")
    );
    tools.operation_id = "set".into();
    let updated = tools
        .execute(
            "threads_update",
            &json!({"thread":id,"icon":{"kind":"emoji","value":"🧑🏽‍💻"}}),
        )
        .await
        .unwrap();
    assert_eq!(
        updated["thread"]["icon"],
        json!({"kind":"emoji","value":"🧑🏽‍💻"})
    );
    let revision = updated["thread"]["revision"].as_u64().unwrap();
    let before = state.storage.thread(id).await.unwrap().unwrap();
    for (data, expected) in [
        (json!({"icon":{"kind":"emoji","value":"🐙"}}), None),
        (
            json!({"icon":{"kind":"emoji","value":"🐙"}}),
            Some(revision - 1),
        ),
        (json!({"icon":{"kind":"emoji","value":""}}), Some(revision)),
        (
            json!({"icon":{"kind":"emoji","value":"x\n"}}),
            Some(revision),
        ),
        (
            json!({"icon":{"kind":"emoji","value":"x\u{2028}"}}),
            Some(revision),
        ),
        (json!({"icon":42}), Some(revision)),
        (
            json!({"icon":{"kind":"emoji","value":"🐙"}, "title":"unexpected"}),
            Some(revision),
        ),
        (json!([]), Some(revision)),
    ] {
        assert!(
            state
                .handle_addressed_thread_action(
                    &state.storage.history_id().await.unwrap(),
                    id,
                    "set_icon".into(),
                    data,
                    expected
                )
                .await
                .is_err()
        );
        assert_eq!(state.storage.thread(id).await.unwrap().unwrap(), before);
    }
    let omitted = state
        .handle_addressed_thread_action(
            &state.storage.history_id().await.unwrap(),
            id,
            "set_icon".into(),
            json!({}),
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(omitted, before);
    let manual = state
        .handle_addressed_thread_action(
            &state.storage.history_id().await.unwrap(),
            id,
            "set_icon".into(),
            json!({"icon":{"kind":"emoji","value":"🐙"}}),
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(
        manual.icon,
        Some(hirsel_proto::ThreadIcon::Emoji {
            value: "🐙".into()
        })
    );
    assert_eq!(manual.revision, revision + 1);
    assert_eq!(manual.last_activity_at, before.last_activity_at);
    assert!(state.broadcast_log.recent().iter().any(|frame| matches!(frame, HostToClient::ThreadUpsert { thread } if thread.id == id && thread.icon == Some(hirsel_proto::ThreadIcon::Emoji { value: "🐙".into() }))));
    let clear = state
        .handle_addressed_thread_action(
            &state.storage.history_id().await.unwrap(),
            id,
            "set_icon".into(),
            json!({"icon":null}),
            Some(manual.revision),
        )
        .await
        .unwrap();
    assert_eq!(clear.icon, None);
    assert_eq!(clear.revision, manual.revision + 1);
    tools.operation_id = "set-again".into();
    tools
        .execute(
            "threads_update",
            &json!({"thread":id,"icon":{"kind":"emoji","value":"☀️"}}),
        )
        .await
        .unwrap();
    tools.operation_id = "clear".into();
    let cleared = tools
        .execute("threads_update", &json!({"thread":id,"icon":null}))
        .await
        .unwrap();
    assert!(cleared["thread"]["icon"].is_null());
    assert_eq!(
        cleared,
        tools
            .execute("threads_update", &json!({"thread":id,"icon":null}))
            .await
            .unwrap()
    );
    let before = state.storage.thread(id).await.unwrap().unwrap();
    for (index, icon) in [
        json!({"kind":"emoji","value":""}),
        json!({"kind":"emoji","value":"x\n"}),
        json!({"kind":"emoji","value":"x\u{2029}"}),
        json!({"kind":"emoji","value":"🐙".repeat(17)}),
        json!(42),
    ]
    .into_iter()
    .enumerate()
    {
        tools.operation_id = format!("bad-{index}");
        assert!(
            tools
                .execute("threads_update", &json!({"thread":id,"icon":icon}))
                .await
                .is_err()
        );
        assert!(
            tools
                .execute(
                    "threads_create",
                    &json!({"client_id":"bad","kind":"task","title":"Bad","icon":icon})
                )
                .await
                .is_err()
        );
    }
    assert_eq!(state.storage.thread(id).await.unwrap().unwrap(), before);
    let read = tools
        .execute("threads_read", &json!({"thread":id}))
        .await
        .unwrap();
    assert!(read["thread"]["icon"].is_null());
    assert_eq!(state.storage.thread_snapshot().await.unwrap().len(), 2);
}

#[test]
fn existing_generated_update_action_remains_valid_but_icon_action_is_reserved() {
    let instrument = |action| json!({"type":"card","children":[{"type":"submit","action":action,"label":"Update","settles":false}]});
    assert!(threads::validate_instrument(Some(&instrument("update"))).is_ok());
    assert!(threads::validate_instrument(Some(&instrument("set_icon"))).is_err());
}

#[tokio::test]
async fn owner_image_icons_validate_uploaded_blobs_and_remain_reachable() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let thread = state
        .storage
        .create_thread(
            "image-owner",
            "Garden",
            "",
            None,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Space,
            None,
        )
        .await
        .unwrap()
        .0;
    let good = state
        .storage
        .store_blob("good-icon", "garden.png", "image/png", png(64, 64))
        .await
        .unwrap();
    let updated = state
        .handle_addressed_thread_action(
            &state.storage.history_id().await.unwrap(),
            thread.id,
            "set_icon".into(),
            json!({"icon":{"kind":"image","blob_id":good.blob.id}}),
            Some(thread.revision),
        )
        .await
        .unwrap();
    assert_eq!(
        updated.icon,
        Some(hirsel_proto::ThreadIcon::Image {
            blob_id: good.blob.id.clone()
        })
    );
    state.storage.log_orphaned_blobs().await.unwrap();
    assert!(state.storage.blob(&good.blob.id).await.unwrap().is_some());
    assert!(good.path.exists());
    assert_eq!(
        state.storage.thread_snapshot().await.unwrap()[0].icon,
        updated.icon
    );
    assert_eq!(
        state
            .storage
            .thread_detail(thread.id, None, 10)
            .await
            .unwrap()
            .thread
            .icon,
        updated.icon
    );

    for (client_id, mime, bytes) in [
        ("wrong-mime", "text/plain", png(32, 32)),
        (
            "svg",
            "image/svg+xml",
            b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec(),
        ),
        ("dimensions", "image/png", png(4_097, 1)),
        ("oversize", "image/png", vec![0; MAX_ICON_BYTES + 1]),
    ] {
        let invalid = state
            .storage
            .store_blob(client_id, "bad", mime, bytes)
            .await
            .unwrap();
        assert!(
            state
                .handle_addressed_thread_action(
                    &state.storage.history_id().await.unwrap(),
                    thread.id,
                    "set_icon".into(),
                    json!({"icon":{"kind":"image","blob_id":invalid.blob.id}}),
                    Some(updated.revision),
                )
                .await
                .is_err()
        );
        assert_eq!(
            state.storage.thread(thread.id).await.unwrap().unwrap(),
            updated
        );
    }
}

#[tokio::test]
async fn agent_image_artifacts_are_normalized_and_scope_is_enforced() {
    use base64::Engine;

    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let root = state.storage.test_running_caller().await;
    let mut tools = ScopedThreadTools {
        tools: state.tools.clone(),
        caller: root.clone(),
        operation_id: "artifact-image".into(),
    };
    let content = base64::engine::general_purpose::STANDARD.encode(png(320, 180));
    let artifact = tools
        .execute(
            "artifacts_create",
            &json!({"title":"Icon","kind":"file","mime":"image/png","filename":"icon.png","content":content}),
        )
        .await
        .unwrap();
    tools.operation_id = "set-artifact-image".into();
    let result = tools
        .execute(
            "threads_update",
            &json!({"thread":".","icon":{"kind":"image","artifact_id":artifact["id"]}}),
        )
        .await
        .unwrap();
    let blob_id = result["thread"]["icon"]["blob_id"].as_str().unwrap();
    let stored = state.storage.blob(blob_id).await.unwrap().unwrap();
    assert_eq!(stored.blob.mime, "image/webp");
    assert!(stored.blob.size <= MAX_ICON_BYTES as u64);
    validate_normalized_icon(
        &state.storage.read_blob(blob_id).await.unwrap(),
        "image/webp",
    )
    .unwrap();

    tools.operation_id = "bad-image".into();
    let bad = tools
        .execute(
            "artifacts_create",
            &json!({"title":"Bad","kind":"file","mime":"text/plain","content":content}),
        )
        .await
        .unwrap();
    tools.operation_id = "reject-bad-image".into();
    assert!(
        tools
            .execute(
                "threads_update",
                &json!({"thread":".","icon":{"kind":"image","artifact_id":bad["id"]}}),
            )
            .await
            .unwrap_err()
            .contains("PNG, JPEG, or WebP")
    );

    let foreign = state
        .storage
        .create_thread(
            "foreign-icon",
            "Foreign",
            "",
            None,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap()
        .0;
    tools.operation_id = "foreign-image".into();
    // The ID is addressable; the write is refused in the open and changes nothing.
    let refused = tools
        .execute(
            "threads_update",
            &json!({"thread":foreign.id,"icon":{"kind":"emoji","value":"⛔"}}),
        )
        .await
        .unwrap();
    assert_eq!(refused["refused"], json!(true));
    assert_eq!(
        refused["target"],
        json!({"kind":"thread","thread_id":foreign.id})
    );
    assert!(
        state
            .storage
            .thread(foreign.id)
            .await
            .unwrap()
            .unwrap()
            .icon
            .is_none()
    );
}
