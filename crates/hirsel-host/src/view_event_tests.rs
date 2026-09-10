use super::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tokio::sync::Notify;
#[derive(Clone)]
struct Pause {
    arrived: Arc<Notify>,
    release: Arc<Notify>,
}
fn pauses() -> &'static Mutex<HashMap<String, Pause>> {
    static PAUSES: OnceLock<Mutex<HashMap<String, Pause>>> = OnceLock::new();
    PAUSES.get_or_init(|| Mutex::new(HashMap::new()))
}
pub(crate) async fn pause_after_lookup(id: &str) {
    let pause = pauses().lock().unwrap().remove(id);
    if let Some(pause) = pause {
        pause.arrived.notify_one();
        pause.release.notified().await;
    }
}
#[tokio::test]
async fn delayed_view_callback_cannot_accept_into_reused_thread_after_runtime_reset() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let original = state
        .storage
        .create_thread(
            "original",
            "Original",
            "",
            &serde_json::json!({}),
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    let old_history = state.storage.history_id().await.unwrap();
    let instance = format!("reset-view-{}", uuid::Uuid::new_v4());
    let spec = serde_json::json!({"type":"action","label":"Continue","action":"continue"});
    state
        .views
        .show(
            &old_history,
            original.id,
            None,
            Some(spec.clone()),
            None,
            Some(instance.clone()),
        )
        .await
        .unwrap();
    let pause = Pause {
        arrived: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    pauses()
        .lock()
        .unwrap()
        .insert(instance.clone(), pause.clone());
    let cloned = state.clone();
    let old_instance = instance.clone();
    let handler = tokio::spawn(async move {
        cloned
            .handle_view_event(
                old_instance,
                "continue".into(),
                serde_json::json!({"old":true}),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), pause.arrived.notified())
        .await
        .unwrap();
    state.agent.reset_history().await.unwrap();
    let fresh = state
        .storage
        .create_thread(
            "new",
            "New",
            "",
            &serde_json::json!({}),
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    assert_eq!(fresh.id, original.id);
    let history = state.storage.history_id().await.unwrap();
    assert_ne!(history, old_history);
    pause.release.notify_one();
    let error = handler.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("history"), "{error}");
    let detail = state
        .storage
        .thread_detail(fresh.id, None, 100)
        .await
        .unwrap();
    assert!(detail.messages.is_empty() && detail.turns.is_empty() && detail.activities.is_empty());
    assert!(
        state
            .storage
            .pending_thread_requests()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(state.views.snapshot().await.is_empty());
    assert!(
        state
            .views
            .show(
                &old_history,
                fresh.id,
                None,
                Some(spec.clone()),
                None,
                Some(instance.clone()),
            )
            .await
            .is_err()
    );
    state
        .views
        .show(
            &history,
            fresh.id,
            None,
            Some(spec),
            None,
            Some(instance.clone()),
        )
        .await
        .unwrap();
    let accepted = state
        .handle_view_event(instance, "continue".into(), serde_json::json!({"new":true}))
        .await
        .unwrap();
    assert_eq!(accepted.message.thread_id, fresh.id);
    assert!(!accepted.message.body.contains("old"));
    assert_eq!(
        state
            .storage
            .thread_detail(fresh.id, None, 100)
            .await
            .unwrap()
            .messages
            .iter()
            .filter(|m| m.author == hirsel_proto::ChatAuthor::Owner)
            .count(),
        1
    );
}
