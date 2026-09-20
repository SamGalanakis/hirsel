use super::*;
use hirsel_proto::{EffectAction, ThreadAttention, ThreadEffectKind, ThreadTurnState};
use serde_json::json;

async fn thread(storage: &Storage, key: &str, parent: Option<u64>) -> u64 {
    storage
        .create_thread(
            key,
            key,
            "",
            None,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            parent,
        )
        .await
        .unwrap()
        .0
        .id
}

async fn caller(storage: &Storage, thread_id: u64) -> ThreadCaller {
    let turn = storage.start_thread_turn(thread_id, None).await.unwrap();
    let history = storage.history_id().await.unwrap();
    storage
        .bind_thread_execution(&history, "effect-session", "effect-execution", turn.id)
        .await
        .unwrap()
}

#[tokio::test]
async fn delegation_receipts_replay_once_and_project_current_actions() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let source = thread(&storage, "source", None).await;
    let caller = caller(&storage, source).await;
    let assignment = Delegation {
        title: "Created child".into(),
        brief: "Do the work".into(),
        artifact_ids: vec![],
        child_thread_id: None,
        execution: None,
    };
    let invocation = serde_json::to_value(&assignment).unwrap();
    let accepted = storage
        .delegate_thread(
            &caller,
            "delegate-1",
            "threads_delegate",
            &assignment,
            &invocation,
        )
        .await
        .unwrap();
    let replay = storage
        .delegate_thread(
            &caller,
            "delegate-1",
            "threads_delegate",
            &assignment,
            &invocation,
        )
        .await
        .unwrap();
    assert_eq!(replay.turn_id, accepted.turn_id);

    let effects = storage.thread_effects(caller.turn_id).await.unwrap();
    assert_eq!(effects.len(), 2);
    assert_eq!(
        effects
            .iter()
            .map(|effect| effect.receipt.effect)
            .collect::<Vec<_>>(),
        [ThreadEffectKind::Created, ThreadEffectKind::Delegated]
    );
    assert_eq!(
        effects
            .iter()
            .map(|effect| effect.receipt.effect_index)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert!(effects[1].actions.contains(&EffectAction::CancelQueued {
        thread_id: accepted.thread_id,
        turn_id: accepted.turn_id
    }));

    storage.run_thread_turn(accepted.turn_id).await.unwrap();
    let running = storage.thread_effects(caller.turn_id).await.unwrap();
    assert!(running[1].actions.contains(&EffectAction::Stop {
        thread_id: accepted.thread_id,
        turn_id: accepted.turn_id
    }));
    assert!(
        !running[1]
            .actions
            .iter()
            .any(|action| matches!(action, EffectAction::CancelQueued { .. }))
    );

    storage
        .finish_thread_turn(accepted.turn_id, ThreadTurnState::Failed, None)
        .await
        .unwrap();
    let failed = storage.thread_effects(caller.turn_id).await.unwrap();
    assert!(!failed[1].actions.iter().any(|action| matches!(
        action,
        EffectAction::CancelQueued { .. } | EffectAction::Stop { .. }
    )));
}

#[tokio::test]
async fn refusal_receipts_distinguish_probes_but_dedupe_operation_replay() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let source = thread(&storage, "source", None).await;
    let outside = thread(&storage, "outside", None).await;
    let caller = caller(&storage, source).await;
    let detail = json!({
        "refused": true,
        "reason": "outside_grant",
        "target": {"kind":"thread","thread_id":outside},
        "tool": "threads_read",
        "grant_summary": "self + subtree",
        "detail": "Thread is outside this caller's reach",
    });

    let first = storage
        .record_refusal(&caller, "probe-1", "threads_read", &detail)
        .await
        .unwrap();
    let replay = storage
        .record_refusal(&caller, "probe-1", "threads_read", &detail)
        .await
        .unwrap();
    let second = storage
        .record_refusal(&caller, "probe-2", "threads_read", &detail)
        .await
        .unwrap();
    assert_eq!(replay.id, first.id);
    assert_ne!(second.id, first.id);
    let effects = storage.thread_effects(caller.turn_id).await.unwrap();
    assert_eq!(effects.len(), 2);
    assert!(
        effects
            .iter()
            .all(|effect| effect.receipt.effect == ThreadEffectKind::Refused)
    );
    let activities = storage
        .thread_detail(source, None, 100)
        .await
        .unwrap()
        .activities
        .into_iter()
        .filter(|activity| activity.kind == "refusal")
        .collect::<Vec<_>>();
    assert_eq!(activities.len(), 2);
    assert_eq!(
        activities
            .iter()
            .map(|activity| activity.data["effect_receipt_id"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        effects
            .iter()
            .map(|effect| effect.receipt.id)
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn failed_receipt_insert_rolls_back_the_effect_and_thread_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let source = thread(&storage, "source", None).await;
    let caller = caller(&storage, source).await;
    storage.conn.lock().await.execute_batch("CREATE TEMP TRIGGER reject_effect BEFORE INSERT ON thread_effect_receipts BEGIN SELECT RAISE(ABORT,'receipt unavailable'); END;").unwrap();
    let mutation = ThreadMutation::Update {
        thread: ThreadRef::default(),
        title: Some("Must roll back".into()),
        icon: None,
        showcased_artifact_id: None,
        description: None,
        instrument: None,
        attention: None,
    };
    assert!(
        storage
            .mutate_scoped_thread(&caller, "rollback-effect", &mutation)
            .await
            .unwrap_err()
            .to_string()
            .contains("receipt unavailable")
    );
    assert_eq!(
        storage.thread(source).await.unwrap().unwrap().title,
        "source"
    );
    let connection = storage.conn.lock().await;
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM thread_effect_receipts", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM thread_mutation_receipts", [], |row| {
                row.get::<_, u64>(0)
            })
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn exact_turn_cancellation_rejects_a_state_that_advanced() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let thread_id = thread(&storage, "target", None).await;
    let history = storage.history_id().await.unwrap();
    let turn = storage.queue_thread_turn(thread_id, None).await.unwrap();
    storage.run_thread_turn(turn.id).await.unwrap();

    let error = storage
        .cancel_exact_thread_turn(&history, thread_id, turn.id, ThreadTurnState::Queued)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("state changed"));
    assert_eq!(
        storage
            .conn
            .lock()
            .await
            .query_row(
                "SELECT cancel_requested_at FROM thread_turns WHERE id=?1",
                [turn.id],
                |row| row.get::<_, Option<String>>(0)
            )
            .unwrap(),
        None
    );

    let accepted = storage
        .cancel_exact_thread_turn(&history, thread_id, turn.id, ThreadTurnState::Running)
        .await
        .unwrap();
    assert_eq!(accepted.id, turn.id);
    assert_eq!(accepted.state, ThreadTurnState::Running);
    assert!(
        storage
            .conn
            .lock()
            .await
            .query_row(
                "SELECT cancel_requested_at IS NOT NULL FROM thread_turns WHERE id=?1",
                [turn.id],
                |row| row.get::<_, bool>(0)
            )
            .unwrap()
    );
}
