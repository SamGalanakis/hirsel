use super::*;
use crate::storage::{RelatedTargetInput, ThreadMutation, ThreadRef};

async fn fixture() -> (tempfile::TempDir, Storage, crate::storage::ThreadCaller) {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let caller = s.test_running_caller().await;
    (dir, s, caller)
}
fn url(value: &str) -> ThreadRelatedTarget {
    ThreadRelatedTarget::Url { url: value.into() }
}
async fn add(
    s: &Storage,
    c: &crate::storage::ThreadCaller,
    value: &str,
    title: Option<&str>,
) -> anyhow::Result<ThreadRelated> {
    s.add_thread_related(
        &uuid::Uuid::new_v4().to_string(),
        &c.history_id,
        c.thread_id,
        &url(value),
        title,
    )
    .await
}
#[tokio::test]
async fn related_urls_deduplicate_preserve_destinations_and_do_not_create_work() {
    let (_dir, s, c) = fixture().await;
    let before = s.thread_detail(c.thread_id, None, 100).await.unwrap();
    let context = s.thread_context(&c).await.unwrap();
    assert_eq!(context.history_id, c.history_id);
    assert_eq!(
        context.reference_url,
        format!("/t/{}?history={}", c.thread_id, c.history_id)
    );
    let read = s
        .scoped_thread_read(&c, &ThreadRef::default(), None, 1)
        .await
        .unwrap();
    assert_eq!(read.reference_url, context.reference_url);
    assert_eq!(read.history_id, c.history_id);
    let first = add(
        &s,
        &c,
        "HTTPS://EXAMPLE.COM:443/doc?q=1#section",
        Some(" Docs "),
    )
    .await
    .unwrap();
    assert_eq!(
        first.related_items[0].target,
        url("https://example.com/doc?q=1#section")
    );
    assert_eq!(first.related_items[0].title.as_deref(), Some("Docs"));
    let retry = add(
        &s,
        &c,
        "https://example.com/doc?q=1#section",
        Some("Replacement"),
    )
    .await
    .unwrap();
    assert_eq!(retry.related_items, first.related_items);
    assert_eq!(retry.revision, first.revision);
    add(&s, &c, "https://example.com/doc?q=2#section", None)
        .await
        .unwrap();
    add(&s, &c, "https://example.com/doc?q=1#other", None)
        .await
        .unwrap();
    let after = s.thread_detail(c.thread_id, None, 100).await.unwrap();
    assert_eq!(after.related_items.len(), 3);
    assert_eq!(after.messages, before.messages);
    assert_eq!(after.activities, before.activities);
    assert_eq!(after.turns, before.turns);
    assert_eq!(
        after.thread.last_activity_at,
        before.thread.last_activity_at
    );
    assert_eq!(after.thread.attention, before.thread.attention);
    assert_eq!(after.thread.read, before.thread.read);
    assert_eq!(
        s.thread_context(&c).await.unwrap().related_items,
        after.related_items
    );
    assert_eq!(
        s.scoped_thread_read(&c, &ThreadRef::default(), None, 1)
            .await
            .unwrap()
            .related_items,
        after.related_items
    );
}
#[tokio::test]
async fn related_human_receipts_prevent_resurrection_and_reject_changed_payloads() {
    let (_dir, s, c) = fixture().await;
    let target = url("https://example.com/repo");
    let first = s
        .add_thread_related("add-a", &c.history_id, c.thread_id, &target, None)
        .await
        .unwrap();
    let removed = s
        .remove_thread_related(
            "remove-b",
            &c.history_id,
            c.thread_id,
            first.related_items[0].id,
        )
        .await
        .unwrap();
    assert!(removed.related_items.is_empty());
    let replay = s
        .add_thread_related("add-a", &c.history_id, c.thread_id, &target, None)
        .await
        .unwrap();
    assert!(replay.related_items.is_empty());
    assert_eq!(replay.revision, removed.revision);
    assert!(
        s.add_thread_related(
            "add-a",
            &c.history_id,
            c.thread_id,
            &url("https://example.com/changed"),
            None
        )
        .await
        .is_err()
    );
    assert!(
        s.remove_thread_related("remove-b", &c.history_id, c.thread_id, 999)
            .await
            .is_err()
    );
    let other = s.test_running_caller().await;
    assert!(
        s.add_thread_related("add-a", &c.history_id, other.thread_id, &target, None)
            .await
            .is_err()
    );
    let again = s
        .remove_thread_related(
            "remove-c",
            &c.history_id,
            c.thread_id,
            first.related_items[0].id,
        )
        .await
        .unwrap();
    assert_eq!(again.revision, removed.revision);
}
#[tokio::test]
async fn related_url_safety_and_capacity_roll_back_while_duplicates_remain_allowed() {
    let (_dir, s, c) = fixture().await;
    for value in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "//example.com/a",
        "https://user:pass@example.com",
        "https://user@example.com",
        "https://@example.com",
        "https:example.com",
        "https:\\example.com",
        " https://example.com",
        "https://example.com\n",
        "https://",
        "",
    ] {
        assert!(
            add(&s, &c, value, None).await.is_err(),
            "accepted {value:?}"
        );
    }
    assert!(
        add(&s, &c, "https://example.com", Some("bad\ntitle"))
            .await
            .is_err()
    );
    assert!(
        add(&s, &c, "https://example.com", Some(&"a".repeat(201)))
            .await
            .is_err()
    );
    assert!(
        add(
            &s,
            &c,
            &format!("https://example.com/{}", "a".repeat(4096)),
            None
        )
        .await
        .is_err()
    );
    assert!(s.thread_context(&c).await.unwrap().related_items.is_empty());
    for i in 0..100 {
        add(&s, &c, &format!("https://example.com/{i}"), None)
            .await
            .unwrap();
    }
    assert!(
        add(&s, &c, "https://example.com/overflow", None)
            .await
            .is_err()
    );
    assert_eq!(
        add(&s, &c, "https://example.com/0", None)
            .await
            .unwrap()
            .related_items
            .len(),
        100
    );
}
#[tokio::test]
async fn related_thread_targets_are_typed_scoped_and_never_grant_foreign_context() {
    let (_dir, s, c) = fixture().await;
    let peer = s.test_running_caller().await;
    let foreign = ThreadRelatedTarget::Thread {
        history_id: c.history_id.clone(),
        thread_id: peer.thread_id,
    };
    let human = s
        .add_thread_related("human-foreign", &c.history_id, c.thread_id, &foreign, None)
        .await
        .unwrap();
    assert_eq!(human.related_items[0].target, foreign);
    for title in ["", "   "] {
        assert!(
            s.add_thread_related(
                "empty-thread-title",
                &c.history_id,
                c.thread_id,
                &foreign,
                Some(title)
            )
            .await
            .is_err()
        );
    }
    assert!(
        s.add_thread_related(
            "bad-title",
            &c.history_id,
            c.thread_id,
            &foreign,
            Some("Secret title")
        )
        .await
        .is_err()
    );
    assert!(
        s.add_thread_related(
            "bad-history",
            &c.history_id,
            c.thread_id,
            &ThreadRelatedTarget::Thread {
                history_id: "wrong".into(),
                thread_id: peer.thread_id
            },
            None
        )
        .await
        .is_err()
    );
    assert_eq!(
        s.thread_detail(c.thread_id, None, 1)
            .await
            .unwrap()
            .related_items
            .len(),
        1
    );
    assert!(s.thread_context(&c).await.unwrap().related_items.is_empty());
    assert!(
        s.scoped_thread_read(&c, &ThreadRef::default(), None, 1)
            .await
            .unwrap()
            .related_items
            .is_empty()
    );
    let reference = |source, target| ThreadMutation::AddRelated {
        thread: source,
        target: RelatedTargetInput::Thread { thread: target },
        title: None,
    };
    assert!(
        s.mutate_scoped_thread(
            &c,
            "foreign",
            &reference(ThreadRef::default(), ThreadRef::Id(peer.thread_id))
        )
        .await
        .is_err()
    );
    let child = s
        .mutate_scoped_thread(
            &c,
            "child",
            &ThreadMutation::Create {
                icon: None,
                client_id: "child".into(),
                kind: hirsel_proto::ThreadKind::Task,
                title: "Child".into(),
                parent: ThreadRef::default(),
                description: String::new(),
                instrument: serde_json::json!({}),
                attention: hirsel_proto::ThreadAttention::Quiet,
            },
        )
        .await
        .unwrap()["thread_id"]
        .as_u64()
        .unwrap();
    let mutation = reference(ThreadRef::default(), ThreadRef::Path(format!("./{child}")));
    let first = s
        .mutate_scoped_thread(&c, "reference", &mutation)
        .await
        .unwrap();
    assert_eq!(
        first["related_items"].as_array().unwrap().len(),
        1,
        "human foreign target remains hidden"
    );
    assert_eq!(first["related_items"][0]["target"]["thread_id"], child);
    assert_eq!(
        first,
        s.mutate_scoped_thread(&c, "reference", &mutation)
            .await
            .unwrap()
    );
    assert!(
        s.mutate_scoped_thread(
            &c,
            "reference",
            &reference(ThreadRef::default(), ThreadRef::default())
        )
        .await
        .is_err()
    );
    assert!(
        s.scoped_thread_read(&c, &ThreadRef::Id(peer.thread_id), None, 1)
            .await
            .is_err()
    );
    let (_, public) = s
        .related_publication_snapshot(&c.history_id, c.thread_id)
        .await
        .unwrap();
    assert_eq!(public.related_items.len(), 2);
    let turn = s.start_thread_turn(child, None).await.unwrap();
    let child_caller = s
        .bind_thread_execution(&c.history_id, "child-session", "child-execution", turn.id)
        .await
        .unwrap();
    assert!(
        s.mutate_scoped_thread(
            &child_caller,
            "ancestor-origin",
            &reference(ThreadRef::Id(c.thread_id), ThreadRef::default())
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn related_cancel_reset_and_stale_publication_are_fenced() {
    let (_dir, s, c) = fixture().await;
    let mutation = ThreadMutation::AddRelated {
        thread: ThreadRef::default(),
        target: RelatedTargetInput::Url {
            url: "https://example.com/repo".into(),
        },
        title: None,
    };
    let first = s.mutate_scoped_thread(&c, "add", &mutation).await.unwrap();
    let item = first["related_items"][0]["id"].as_u64().unwrap();
    let peer = s.test_running_caller().await;
    assert!(
        s.mutate_scoped_thread(
            &peer,
            "remove",
            &ThreadMutation::RemoveRelated {
                thread: ThreadRef::Id(c.thread_id),
                item_id: item
            }
        )
        .await
        .is_err()
    );
    s.mutate_scoped_thread(
        &c,
        "cancel",
        &ThreadMutation::Cancel {
            thread: ThreadRef::default(),
        },
    )
    .await
    .unwrap();
    assert!(s.mutate_scoped_thread(&c, "add", &mutation).await.is_err());
    s.reset().await.unwrap();
    let fresh = s.test_running_caller().await;
    assert!(
        s.add_thread_related(
            "stale",
            &c.history_id,
            fresh.thread_id,
            &url("https://example.com"),
            None
        )
        .await
        .is_err()
    );
    assert!(
        s.remove_thread_related("stale-remove", &c.history_id, fresh.thread_id, item)
            .await
            .is_err()
    );
    assert!(
        s.related_publication_snapshot(&c.history_id, fresh.thread_id)
            .await
            .is_err()
    );
    assert!(s.mutate_scoped_thread(&c, "add", &mutation).await.is_err());
    assert!(
        s.thread_context(&fresh)
            .await
            .unwrap()
            .related_items
            .is_empty()
    );
}
