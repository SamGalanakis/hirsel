use super::*;
use crate::lash_runtime::ScopedThreadTools;
use crate::storage::ThreadRef;
use hirsel_proto::ThreadAttention;
use hirsel_proto::{ReachTarget, ThreadGrantTarget};
use serde_json::json;

async fn thread(s: &Storage, key: &str, parent: Option<u64>) -> u64 {
    s.create_thread(
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
fn thread_target(thread_id: u64) -> ReachTarget {
    ReachTarget::Thread { thread_id }
}
async fn caller(s: &Storage, id: u64) -> ThreadCaller {
    let turn = s.start_thread_turn(id, None).await.unwrap();
    let history = s.history_id().await.unwrap();
    let launch = uuid::Uuid::new_v4().to_string();
    s.bind_thread_execution(&history, &launch, &launch, turn.id)
        .await
        .unwrap()
}

#[tokio::test]
async fn an_owner_grant_widens_reach_and_revoking_it_narrows_again() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let history = s.history_id().await.unwrap();
    let billing = thread(&s, "billing", None).await;
    let detail = thread(&s, "invoice", Some(billing)).await;
    let worker = thread(&s, "worker", None).await;
    let actor = caller(&s, worker).await;

    assert!(
        s.resolve_thread(&actor, &ThreadRef::Id(billing))
            .await
            .is_err()
    );
    let granted = s
        .set_thread_reach(
            "grant-1",
            &history,
            worker,
            thread_target(billing),
            Some("shared work"),
            true,
        )
        .await
        .unwrap();
    assert_eq!(granted.grants.len(), 1);
    assert_eq!(
        granted.grants[0].target,
        ThreadGrantTarget::Thread {
            thread_id: billing,
            title: "billing".into(),
            thread_kind: hirsel_proto::ThreadKind::Task
        }
    );
    assert_eq!(granted.grants[0].note.as_deref(), Some("shared work"));
    assert_eq!(
        granted.grants[0].granted_by,
        hirsel_proto::ThreadGrantSource::Owner
    );

    // The grant carries the target's whole subtree, exactly like the default.
    assert_eq!(
        s.resolve_thread(&actor, &ThreadRef::Id(billing))
            .await
            .unwrap(),
        billing
    );
    assert_eq!(
        s.resolve_thread(&actor, &ThreadRef::Id(detail))
            .await
            .unwrap(),
        detail
    );
    assert_eq!(
        s.thread_reach(&actor).await.unwrap(),
        format!("self + subtree · +Task #{billing} \"billing\"")
    );
    // Reach is one-way: the target gains nothing.
    assert!(!s.thread_in_scope(billing, worker).await.unwrap());

    // Replay of the same client_id is the same durable state, not a second row.
    let replayed = s
        .set_thread_reach(
            "grant-1",
            &history,
            worker,
            thread_target(billing),
            Some("shared work"),
            true,
        )
        .await
        .unwrap();
    assert_eq!(replayed.grants.len(), 1);
    assert_eq!(replayed.revision, granted.revision);

    let revoked = s
        .set_thread_reach(
            "revoke-1",
            &history,
            worker,
            thread_target(billing),
            None,
            false,
        )
        .await
        .unwrap();
    assert!(revoked.grants.is_empty());
    assert!(
        s.resolve_thread(&actor, &ThreadRef::Id(detail))
            .await
            .is_err()
    );
    assert_eq!(s.thread_reach(&actor).await.unwrap(), "self + subtree");
}

#[tokio::test]
async fn default_reach_is_never_stored_as_a_grant() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let history = s.history_id().await.unwrap();
    let root = thread(&s, "root", None).await;
    let child = thread(&s, "child", Some(root)).await;
    assert!(
        s.set_thread_reach("a", &history, root, thread_target(root), None, true)
            .await
            .is_err()
    );
    assert!(
        s.set_thread_reach("b", &history, root, thread_target(child), None, true)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn out_of_reach_calls_return_a_readable_refusal_and_log_every_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let s = &state.storage;
    let history = s.history_id().await.unwrap();
    let worker = thread(s, "worker", None).await;
    let secret = thread(s, "secret", None).await;
    let actor = caller(s, worker).await;
    let mut tools = ScopedThreadTools {
        tools: state.tools.clone(),
        caller: actor.clone(),
        operation_id: "read-1".into(),
    };

    let refused = tools
        .execute("threads_read", &json!({"thread": secret}))
        .await
        .unwrap();
    assert_eq!(refused["refused"], json!(true));
    assert_eq!(refused["reason"], json!("outside_grant"));
    assert_eq!(
        refused["target"],
        json!({"kind":"thread","thread_id":secret})
    );
    assert_eq!(refused["grant_summary"], json!("self + subtree"));

    // A second probe in the same turn is a second fact, never deduplicated.
    tools.operation_id = "read-2".into();
    tools
        .execute("threads_read", &json!({"thread": secret}))
        .await
        .unwrap();
    let refusals = s
        .thread_detail(worker, None, 100)
        .await
        .unwrap()
        .activities
        .into_iter()
        .filter(|activity| activity.kind == "refusal")
        .collect::<Vec<_>>();
    assert_eq!(refusals.len(), 2);
    assert_eq!(refusals[0].data["tool"], json!("threads_read"));
    assert_eq!(refusals[0].turn_id, Some(actor.turn_id));

    // A widened grant flips the very same call to a real read.
    s.set_thread_reach("grant", &history, worker, thread_target(secret), None, true)
        .await
        .unwrap();
    tools.operation_id = "read-3".into();
    let read = tools
        .execute("threads_read", &json!({"thread": secret}))
        .await
        .unwrap();
    assert_eq!(read["thread"]["id"], json!(secret));
    assert!(read.get("refused").is_none());
}

#[tokio::test]
async fn an_ancestor_hands_on_only_reach_it_holds_and_never_widens_itself() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let s = &state.storage;
    let history = s.history_id().await.unwrap();
    let lead = thread(s, "lead", None).await;
    let worker = thread(s, "worker", Some(lead)).await;
    let billing = thread(s, "billing", None).await;
    let stranger = thread(s, "stranger", None).await;
    let lead_actor = caller(s, lead).await;
    let mut tools = ScopedThreadTools {
        tools: state.tools.clone(),
        caller: lead_actor,
        operation_id: "grant-unheld".into(),
    };

    // Reach it does not hold is refused, not silently granted.
    let refused = tools
        .execute(
            "threads_grant",
            &json!({"thread": worker, "target": billing}),
        )
        .await
        .unwrap();
    assert_eq!(refused["reason"], json!("outside_grant"));

    s.set_thread_reach(
        "owner-grant",
        &history,
        lead,
        thread_target(billing),
        None,
        true,
    )
    .await
    .unwrap();
    tools.operation_id = "grant-held".into();
    let granted = tools
        .execute(
            "threads_grant",
            &json!({"thread": worker, "target": billing, "note": "invoice work"}),
        )
        .await
        .unwrap();
    assert_eq!(
        granted["grants"][0]["target"],
        json!({"kind":"thread","thread_id":billing,"title":"billing","thread_kind":"task"})
    );
    assert_eq!(
        granted["grants"][0]["granted_by"],
        json!({"kind":"thread","thread_id":lead})
    );
    let worker_actor = caller(s, worker).await;
    assert_eq!(
        s.resolve_thread(&worker_actor, &ThreadRef::Id(billing))
            .await
            .unwrap(),
        billing
    );

    // Self-widening and widening a Thread that is not below the caller both fail.
    tools.operation_id = "grant-self".into();
    assert!(
        tools
            .execute("threads_grant", &json!({"thread": ".", "target": billing}))
            .await
            .is_err()
    );
    tools.operation_id = "grant-stranger".into();
    let refused = tools
        .execute(
            "threads_grant",
            &json!({"thread": stranger, "target": billing}),
        )
        .await
        .unwrap();
    assert_eq!(refused["reason"], json!("outside_grant"));

    tools.operation_id = "revoke".into();
    let revoked = tools
        .execute(
            "threads_revoke",
            &json!({"thread": worker, "target": billing}),
        )
        .await
        .unwrap();
    assert_eq!(revoked["grants"], json!([]));
    assert!(
        s.resolve_thread(&worker_actor, &ThreadRef::Id(billing))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_granted_peer_is_messageable_but_an_ancestor_never_is() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let s = &state.storage;
    let history = s.history_id().await.unwrap();
    let lead = thread(s, "lead", None).await;
    let worker = thread(s, "worker", Some(lead)).await;
    let peer = thread(s, "peer", None).await;
    let worker_actor = caller(s, worker).await;
    let mut tools = ScopedThreadTools {
        tools: state.tools.clone(),
        caller: worker_actor,
        operation_id: "send-upward".into(),
    };

    // The one fence a grant cannot open.
    s.set_thread_reach(
        "reach-lead",
        &history,
        worker,
        thread_target(lead),
        None,
        true,
    )
    .await
    .unwrap();
    let refused = tools
        .execute(
            "threads_send",
            &json!({"client_id":"m1","thread": lead, "text": "hello"}),
        )
        .await
        .unwrap();
    assert_eq!(refused["reason"], json!("owner_fence"));

    // Lateral messaging is an ordinary message once the reach exists.
    tools.operation_id = "send-peer-refused".into();
    let refused = tools
        .execute(
            "threads_send",
            &json!({"client_id":"m2","thread": peer, "text": "hello"}),
        )
        .await
        .unwrap();
    assert_eq!(refused["reason"], json!("outside_grant"));
    s.set_thread_reach(
        "reach-peer",
        &history,
        worker,
        thread_target(peer),
        None,
        true,
    )
    .await
    .unwrap();
    tools.operation_id = "send-peer".into();
    let accepted = tools
        .execute(
            "threads_send",
            &json!({"client_id":"m3","thread": peer, "text": "hello"}),
        )
        .await
        .unwrap();
    assert_eq!(accepted["thread_id"], json!(peer));
}

#[tokio::test]
async fn a_root_grant_reaches_threads_that_did_not_exist_when_it_was_made() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let history = s.history_id().await.unwrap();
    let stranger = thread(&s, "stranger", None).await;
    let worker = thread(&s, "worker", None).await;
    let actor = caller(&s, worker).await;

    assert!(
        s.resolve_thread(&actor, &ThreadRef::Id(stranger))
            .await
            .is_err()
    );
    let granted = s
        .set_thread_reach(
            "root-1",
            &history,
            worker,
            ReachTarget::Root,
            Some("runs the place"),
            true,
        )
        .await
        .unwrap();
    assert_eq!(granted.grants.len(), 1);
    assert_eq!(granted.grants[0].target, ThreadGrantTarget::Root);

    // Everything already there, and everything made afterwards.
    assert_eq!(
        s.resolve_thread(&actor, &ThreadRef::Id(stranger))
            .await
            .unwrap(),
        stranger
    );
    let later = thread(&s, "later", None).await;
    let deeper = thread(&s, "deeper", Some(later)).await;
    assert_eq!(
        s.resolve_thread(&actor, &ThreadRef::Id(deeper))
            .await
            .unwrap(),
        deeper
    );
    assert_eq!(s.thread_reach(&actor).await.unwrap(), "everything (root)");
    // Reach stays one-way, and one root grant is one row however often it is asked for.
    assert!(!s.thread_in_scope(stranger, worker).await.unwrap());
    let again = s
        .set_thread_reach("root-2", &history, worker, ReachTarget::Root, None, true)
        .await
        .unwrap();
    assert_eq!(again.grants.len(), 1);

    let revoked = s
        .set_thread_reach("root-off", &history, worker, ReachTarget::Root, None, false)
        .await
        .unwrap();
    assert!(revoked.grants.is_empty());
    assert!(
        s.resolve_thread(&actor, &ThreadRef::Id(stranger))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn only_a_root_holder_hands_root_on_and_the_refusal_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let s = &state.storage;
    let history = s.history_id().await.unwrap();
    let lead = thread(s, "lead", None).await;
    let worker = thread(s, "worker", Some(lead)).await;
    let stranger = thread(s, "stranger", None).await;
    let mut tools = ScopedThreadTools {
        tools: state.tools.clone(),
        caller: caller(s, lead).await,
        operation_id: "root-unheld".into(),
    };

    // Reach it does not hold is refused in the open, never invented.
    let refused = tools
        .execute(
            "threads_grant",
            &json!({"thread": worker, "target": "root"}),
        )
        .await
        .unwrap();
    assert_eq!(refused["reason"], json!("outside_grant"));
    assert_eq!(refused["target"], json!({"kind":"root"}));
    assert_eq!(refused["grant_summary"], json!("self + subtree"));

    s.set_thread_reach("owner-root", &history, lead, ReachTarget::Root, None, true)
        .await
        .unwrap();
    tools.operation_id = "root-held".into();
    let granted = tools
        .execute(
            "threads_grant",
            &json!({"thread": worker, "target": "root", "note": "stands in for me"}),
        )
        .await
        .unwrap();
    assert_eq!(granted["grants"][0]["target"], json!({"kind":"root"}));
    let worker_actor = caller(s, worker).await;
    assert_eq!(
        s.resolve_thread(&worker_actor, &ThreadRef::Id(stranger))
            .await
            .unwrap(),
        stranger
    );

    // A root holder still never messages its own ancestors, and the refusal it
    // reads names the reach it does hold.
    let worker_tools = ScopedThreadTools {
        tools: state.tools.clone(),
        caller: worker_actor.clone(),
        operation_id: "send-upward".into(),
    };
    let refused = worker_tools
        .execute(
            "threads_send",
            &json!({"client_id":"m1","thread": lead, "text": "hello"}),
        )
        .await
        .unwrap();
    assert_eq!(refused["reason"], json!("owner_fence"));
    assert_eq!(refused["grant_summary"], json!("everything (root)"));

    tools.operation_id = "revoke-root".into();
    let revoked = tools
        .execute(
            "threads_revoke",
            &json!({"thread": worker, "target": "root"}),
        )
        .await
        .unwrap();
    assert_eq!(revoked["grants"], json!([]));
    assert!(
        s.resolve_thread(&worker_actor, &ThreadRef::Id(stranger))
            .await
            .is_err()
    );
}
