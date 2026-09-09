use super::*;
use crate::tools::{ProcessTerminal, TerminalEventBus};
use hirsel_drivers::SessionHandle;
use lash::persistence::{SessionStoreCreateRequest, SessionStoreFactory};
use std::sync::atomic::AtomicUsize;

async fn runtime_fixture() -> (crate::AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = crate::tests::test_config(dir.path());
    config.agent = AgentMode::Lash;
    config.anthropic_api_key = Some("test-key-no-inference".into());
    (crate::build_state(config).await.unwrap(), dir)
}

async fn start_process(runtime: &LashAgentRuntime, id: &str, wake_target: Option<&str>) {
    let mut terminal = terminal_event_type(SUBAGENT_COMPLETED, ProcessStatus::Completed);
    let originator = match wake_target {
        Some(target) => ProcessOriginator::session(SessionScope::new(target)),
        None => {
            terminal.semantics.wake = None;
            ProcessOriginator::host()
        }
    };
    runtime
        .core
        .processes()
        .start(
            ProcessStartRequest::external(id, originator, json!({}))
                .with_event_types(vec![terminal])
                .with_wake_session_id(wake_target.map(str::to_string))
                .with_observers(wake_target),
            inline_trigger_scope(format!("delivery-test:{id}")),
        )
        .await
        .unwrap();
}

fn terminal(id: &str) -> ProcessTerminal {
    ProcessTerminal {
        process_id: id.into(),
        handle: SessionHandle {
            id: format!("session:{id}"),
            agent: AgentKind::Codex,
        },
        outcome: TerminalOutcome::Done {
            summary: format!("completed {id}"),
        },
    }
}

async fn notified(notify: &Notify) {
    tokio::time::timeout(Duration::from_secs(5), notify.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn terminal_receiver_suppresses_duplicates_only_after_handling_ack() {
    let bus = TerminalEventBus::new(4);
    let mut receiver = bus.subscribe();
    bus.publish(terminal("receipt-is-not-ack"));
    let event = receiver.recv().await.unwrap();
    bus.publish(event.clone());
    assert_eq!(receiver.recv().await.unwrap().process_id, event.process_id);
    receiver.acknowledge(&event.process_id);
    bus.publish(event);
    bus.publish(terminal("next-delivery"));
    assert_eq!(receiver.recv().await.unwrap().process_id, "next-delivery");
    receiver.acknowledge("next-delivery");
}

#[tokio::test]
async fn terminal_delivery_retries_failure_and_lost_receipt_without_duplicate_append() {
    let (state, _dir) = runtime_fixture().await;
    let AgentBackend::Lash(runtime) = state.agent.backend.as_ref() else {
        panic!("Lash runtime")
    };
    start_process(runtime, "lost-receipt", None).await;
    let registry = runtime.core.process_registry().unwrap();
    let bus = TerminalEventBus::new(2);
    let attempts = Arc::new(AtomicUsize::new(0));
    let first_failure = Arc::new(Notify::new());
    let allow_retry = Arc::new(Notify::new());
    let notify = Arc::new(Notify::new());
    let bridge = tokio::spawn(run_process_terminal_bridge(
        bus.subscribe(),
        {
            let registry = registry.clone();
            let attempts = attempts.clone();
            let first_failure = first_failure.clone();
            let allow_retry = allow_retry.clone();
            move |id: String, request| {
                let registry = registry.clone();
                let attempts = attempts.clone();
                let first_failure = first_failure.clone();
                let allow_retry = allow_retry.clone();
                async move {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        first_failure.notify_one();
                        return Err(PluginError::Session("injected append failure".into()));
                    }
                    if attempt == 1 {
                        allow_retry.notified().await;
                    }
                    let receipt = registry.append_event(&id, request).await?;
                    if attempt == 1 {
                        Err(PluginError::Session("injected lost append receipt".into()))
                    } else {
                        Ok(receipt)
                    }
                }
            }
        },
        Arc::new(lash_core::facade_support::InMemorySessionStoreFactory::new()),
        crate::fork_wake::ForkWakeHandle::default(),
        notify.clone(),
    ));
    bus.publish(terminal("lost-receipt"));
    notified(&first_failure).await;
    assert!(
        !registry
            .get_process("lost-receipt")
            .await
            .unwrap()
            .unwrap()
            .is_terminal()
    );
    bus.publish(terminal("lost-receipt")); // Duplicate while delivery is in flight.
    allow_retry.notify_one();
    notified(&notify).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    let log = registry.events_after("lost-receipt", 0).await.unwrap();
    assert_eq!(
        log.iter()
            .filter(|e| e.event_type == SUBAGENT_COMPLETED)
            .count(),
        1
    );
    let output = tokio::time::timeout(
        Duration::from_secs(2),
        runtime.core.processes().await_output("lost-receipt"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        subagents_wait_result("lost-receipt", &output).unwrap()["outcome"]["value"]["summary"],
        "completed lost-receipt"
    );
    bridge.abort();
    assert!(bridge.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn terminal_delivery_does_not_block_other_processes_and_replays_after_abort() {
    let (state, _dir) = runtime_fixture().await;
    let AgentBackend::Lash(runtime) = state.agent.backend.as_ref() else {
        panic!("Lash runtime")
    };
    for id in ["blocked-delivery", "healthy-delivery"] {
        start_process(runtime, id, None).await;
    }
    let registry = runtime.core.process_registry().unwrap();
    let bus = TerminalEventBus::new(2);
    let failed = Arc::new(Notify::new());
    let notify = Arc::new(Notify::new());
    let bridge = tokio::spawn(run_process_terminal_bridge(
        bus.subscribe(),
        {
            let registry = registry.clone();
            let failed = failed.clone();
            move |id: String, request| {
                let registry = registry.clone();
                let failed = failed.clone();
                async move {
                    if id == "blocked-delivery" {
                        failed.notify_one();
                        Err(PluginError::Session("injected append outage".into()))
                    } else {
                        registry.append_event(&id, request).await
                    }
                }
            }
        },
        Arc::new(lash_core::facade_support::InMemorySessionStoreFactory::new()),
        crate::fork_wake::ForkWakeHandle::default(),
        notify.clone(),
    ));
    bus.publish(terminal("blocked-delivery"));
    notified(&failed).await;
    bus.publish(terminal("healthy-delivery"));
    notified(&notify).await;
    assert!(
        !registry
            .get_process("blocked-delivery")
            .await
            .unwrap()
            .unwrap()
            .is_terminal()
    );
    assert!(
        registry
            .get_process("healthy-delivery")
            .await
            .unwrap()
            .unwrap()
            .is_terminal()
    );
    bridge.abort();
    assert!(bridge.await.unwrap_err().is_cancelled());
    // The failed delivery remains retained and a replacement bridge handles
    // it without restarting delegated work. Replayed successful append also
    // uses the same key, so this cannot duplicate the durable terminal.
    let recovered = Arc::new(Notify::new());
    let next = tokio::spawn(run_process_terminal_bridge(
        bus.subscribe(),
        {
            let registry = registry.clone();
            move |id: String, request| {
                let registry = registry.clone();
                async move { registry.append_event(&id, request).await }
            }
        },
        Arc::new(lash_core::facade_support::InMemorySessionStoreFactory::new()),
        crate::fork_wake::ForkWakeHandle::default(),
        recovered,
    ));
    tokio::time::timeout(
        Duration::from_secs(2),
        runtime.core.processes().await_output("blocked-delivery"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        registry
            .events_after("healthy-delivery", 0)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.event_type == SUBAGENT_COMPLETED)
            .count(),
        1
    );
    drop(bus);
    tokio::time::timeout(Duration::from_secs(2), next)
        .await
        .unwrap()
        .unwrap();
}

struct FailOnceFactory {
    inner: lash_core::facade_support::InMemorySessionStoreFactory,
    opens: AtomicUsize,
}

#[async_trait]
impl lash_core::AttachmentRootSet for FailOnceFactory {
    async fn live_attachment_refs(
        &self,
        cutoff: u64,
    ) -> Result<std::collections::BTreeSet<lash_core::AttachmentId>, lash_core::StoreError> {
        self.inner.live_attachment_refs(cutoff).await
    }

    async fn has_live_attachment_ref(
        &self,
        id: &lash_core::AttachmentId,
        cutoff: u64,
    ) -> Result<bool, lash_core::StoreError> {
        self.inner.has_live_attachment_ref(id, cutoff).await
    }
}

#[async_trait]
impl SessionStoreFactory for FailOnceFactory {
    async fn session_was_deleted(&self, session_id: &str) -> Result<bool, String> {
        self.inner.session_was_deleted(session_id).await
    }

    async fn delete_session(
        &self,
        session_id: &str,
    ) -> lash_core::store::MaintenanceResult<lash_core::store::SessionBlobReclaimReport> {
        self.inner.delete_session(session_id).await
    }

    async fn create_store(
        &self,
        request: &SessionStoreCreateRequest,
    ) -> Result<Arc<dyn lash_core::store::RuntimePersistence>, lash_core::StoreError> {
        self.inner.create_store(request).await
    }
    async fn open_existing_store(
        &self,
        request: &SessionStoreCreateRequest,
    ) -> Result<Option<Arc<dyn lash_core::store::RuntimePersistence>>, String> {
        if self.opens.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err("injected wake-store failure".into());
        }
        self.inner.open_existing_store(request).await
    }
}

#[tokio::test]
async fn terminal_delivery_retries_wake_without_reappending_terminal() {
    let (state, _dir) = runtime_fixture().await;
    let AgentBackend::Lash(runtime) = state.agent.backend.as_ref() else {
        panic!("Lash runtime")
    };
    let target = "terminal-delivery-wake-target";
    let factory = Arc::new(FailOnceFactory {
        inner: lash_core::facade_support::InMemorySessionStoreFactory::new(),
        opens: AtomicUsize::new(0),
    });
    let store = factory
        .create_store(&SessionStoreCreateRequest {
            session_id: target.into(),
            relation: Default::default(),
            policy: SessionPolicy::new(lash::TurnBudget::Unbounded),
            pending_observer_intents: vec![],
        })
        .await
        .unwrap();
    start_process(runtime, "wake-retry", Some(target)).await;
    let registry = runtime.core.process_registry().unwrap();
    let bus = TerminalEventBus::new(2);
    let appends = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(Notify::new());
    let bridge = tokio::spawn(run_process_terminal_bridge(
        bus.subscribe(),
        {
            let registry = registry.clone();
            let appends = appends.clone();
            move |id: String, request| {
                let registry = registry.clone();
                let appends = appends.clone();
                async move {
                    appends.fetch_add(1, Ordering::SeqCst);
                    let result = registry.append_event(&id, request).await?;
                    assert!(result.wake_delivery.is_some());
                    Ok(result)
                }
            }
        },
        factory.clone(),
        crate::fork_wake::ForkWakeHandle::default(),
        notify.clone(),
    ));
    bus.publish(terminal("wake-retry"));
    bus.publish(terminal("wake-retry"));
    notified(&notify).await;
    assert_eq!(appends.load(Ordering::SeqCst), 1);
    assert_eq!(factory.opens.load(Ordering::SeqCst), 2);
    assert_eq!(store.list_queued_work(target).await.unwrap().len(), 1);
    assert_eq!(
        registry
            .events_after("wake-retry", 0)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.event_type == SUBAGENT_COMPLETED)
            .count(),
        1
    );
    bridge.abort();
    assert!(bridge.await.unwrap_err().is_cancelled());
}
