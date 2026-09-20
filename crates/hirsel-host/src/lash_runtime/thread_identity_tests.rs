//! The identity block that opens every agent turn prompt, asserted against a
//! live runtime: the block must track the Thread, not the session it opened on.
use super::thread_recovery_tests::{runtime_fixture, runtime_lane};
use hirsel_proto::ThreadAttention;

/// The identity block is rebuilt from storage before every queued turn, so a
/// Thread renamed, re-described or granted new reach mid-session is prompted
/// with the new values rather than the ones the session opened on.
#[tokio::test]
async fn the_identity_block_follows_the_thread_between_turns() {
    let (state, _dir) = runtime_fixture().await;
    let space = |client_id: &'static str, title: &'static str, parent: Option<u64>| {
        let storage = state.storage.clone();
        async move {
            storage
                .create_thread(
                    client_id,
                    title,
                    "",
                    None,
                    ThreadAttention::Quiet,
                    if parent.is_some() {
                        hirsel_proto::ThreadKind::Task
                    } else {
                        hirsel_proto::ThreadKind::Space
                    },
                    parent,
                )
                .await
                .unwrap()
                .0
        }
    };
    let parent = space("identity-parent", "Hirsel", None).await;
    let billing = space("identity-billing", "Billing", None).await;
    let thread = space("identity-child", "lash", Some(parent.id)).await;
    let runtime = runtime_lane(&state, Some(thread.id)).await;
    let _pump = runtime.pump_lock.lock().await;
    let prompt = || {
        serde_json::to_string(&runtime.session.policy_snapshot().prompt)
            .expect("prompt layer serializes")
    };

    runtime.apply_agent_prompt().await.unwrap();
    let first = prompt();
    assert!(
        first.contains(&format!(
            r#"You are the agent of Task #{} \"lash\"."#,
            thread.id
        )),
        "first turn names the Thread: {first}"
    );
    assert!(first.contains(&format!(r#"Ancestors: Space #{} \"Hirsel\""#, parent.id)));
    assert!(
        first.contains(
            "Role: Worker — do the work for this Task; read its brief and current state."
        ),
        "Native worker role is missing: {first}"
    );
    assert!(first.contains(r"Description: (none yet)"));

    state
        .storage
        .update_thread(
            thread.id,
            Some("lash runtime"),
            Some("Upstream lash work."),
            None,
            None,
        )
        .await
        .unwrap();
    let history_id = state.storage.history_id().await.unwrap();
    state
        .storage
        .set_thread_reach(
            "identity-grant",
            &history_id,
            thread.id,
            hirsel_proto::ReachTarget::Thread {
                thread_id: billing.id,
            },
            None,
            true,
        )
        .await
        .unwrap();

    runtime.apply_agent_prompt().await.unwrap();
    let second = prompt();
    assert!(
        second.contains(&format!(
            r#"You are the agent of Task #{} \"lash runtime\"."#,
            thread.id
        )),
        "second turn carries the new title: {second}"
    );
    assert!(second.contains(r"Description: Upstream lash work."));
    assert!(second.contains(&format!(
        r#"Reach: self + subtree · +Space #{} \"Billing\""#,
        billing.id
    )));
}

#[tokio::test]
async fn native_project_chat_role_and_profile_change_with_top_level_kind() {
    let (state, _dir) = runtime_fixture().await;
    let (project, _) = state
        .storage
        .create_thread(
            "identity-project-chat",
            "Hirsel",
            "Coordinate the project.",
            None,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Space,
            None,
        )
        .await
        .unwrap();
    let runtime = runtime_lane(&state, Some(project.id)).await;
    let _pump = runtime.pump_lock.lock().await;
    let prompt = || {
        serde_json::to_string(&runtime.session.policy_snapshot().prompt)
            .expect("prompt layer serializes")
    };

    runtime.apply_agent_prompt().await.unwrap();
    let project_prompt = prompt();
    assert!(
        project_prompt
            .contains("Role: Project chat — dispatch work to Task workers; do not do the work."),
        "project-chat role is missing: {project_prompt}"
    );

    let history = state.storage.history_id().await.unwrap();
    state
        .storage
        .set_addressed_thread_kind(
            &history,
            project.id,
            hirsel_proto::ThreadKind::Task,
            project.revision,
        )
        .await
        .unwrap();
    runtime.apply_agent_prompt().await.unwrap();
    let worker_prompt = prompt();
    assert!(
        worker_prompt.contains(
            "Role: Worker — do the work for this Task; read its brief and current state."
        ),
        "converted worker role is missing: {worker_prompt}"
    );
    assert!(!worker_prompt.contains("Role: Project chat"));
}
