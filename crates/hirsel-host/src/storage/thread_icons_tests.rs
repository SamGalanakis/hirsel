use super::*;
use crate::lash_runtime::ScopedThreadTools;
use hirsel_proto::{HostToClient, ThreadAttention};
use serde_json::json;

#[test]
fn compact_icon_validation_preserves_composed_emoji_and_null_semantics() {
    assert_eq!(parse_icon(&json!({})).unwrap(), None);
    assert_eq!(parse_icon(&json!({"icon":null})).unwrap(), Some(None));
    for value in ["🧑🏽‍💻", "👨‍👩‍👧‍👦", "☀️", "★", "🐙".repeat(16).as_str()]
    {
        assert_eq!(
            parse_icon(&json!({"icon":value})).unwrap(),
            Some(Some(value.into()))
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
        assert!(parse_icon(&json!({"icon":value})).is_err(), "{value:?}");
    }
    for value in [json!(42), json!({}), json!([]), json!(false)] {
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
            &Value::Null,
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
            &json!({"client_id":"child","kind":"task","title":"Research","icon":"🔬"}),
        )
        .await
        .unwrap();
    let id = created["thread_id"].as_u64().unwrap();
    assert_eq!(created["thread"]["icon"], "🔬");
    let replayed = tools
        .execute(
            "threads_create",
            &json!({"client_id":"child","kind":"task","title":"Research","icon":"🔬"}),
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
    assert_eq!(preserved["thread"]["icon"], "🔬");
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
        .execute("threads_update", &json!({"thread":id,"icon":"🧑🏽‍💻"}))
        .await
        .unwrap();
    assert_eq!(updated["thread"]["icon"], "🧑🏽‍💻");
    let revision = updated["thread"]["revision"].as_u64().unwrap();
    let before = state.storage.thread(id).await.unwrap().unwrap();
    for (data, expected) in [
        (json!({"icon":"🐙"}), None),
        (json!({"icon":"🐙"}), Some(revision - 1)),
        (json!({"icon":""}), Some(revision)),
        (json!({"icon":"x\n"}), Some(revision)),
        (json!({"icon":"x\u{2028}"}), Some(revision)),
        (json!({"icon":42}), Some(revision)),
        (json!({"icon":"🐙", "title":"unexpected"}), Some(revision)),
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
            json!({"icon":"🐙"}),
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(manual.icon.as_deref(), Some("🐙"));
    assert_eq!(manual.revision, revision + 1);
    assert_eq!(manual.last_activity_at, before.last_activity_at);
    assert!(state.broadcast_log.recent().iter().any(|frame| matches!(frame, HostToClient::ThreadUpsert { thread } if thread.id == id && thread.icon.as_deref() == Some("🐙"))));
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
        .execute("threads_update", &json!({"thread":id,"icon":"☀️"}))
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
        json!(""),
        json!("x\n"),
        json!("x\u{2029}"),
        json!("🐙".repeat(17)),
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
    assert!(threads::validate_instrument(&instrument("update")).is_ok());
    assert!(threads::validate_instrument(&instrument("set_icon")).is_err());
}
