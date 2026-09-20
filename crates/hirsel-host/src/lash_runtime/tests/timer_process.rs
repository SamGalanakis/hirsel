use super::*;

#[tokio::test]
async fn timer_triggered_typescript_process_calls_hirsel_tool_and_delivers_message() {
    timer_process_case("in_secs: 1", true).await;
}

#[tokio::test]
async fn directly_started_typescript_process_calls_hirsel_tool() {
    use lash_core::{LlmOutputPart, llm::types::LlmResponse};

    const DRAIN: &str = "direct-process-drain";
    let source = r#"<typescript>
const direct_shell_check = defineProcess({
  name: "direct_shell_check",
  signals: {},
  run: async () => {
    const output = await shell.run({ cmd: "printf direct-primitive-ok" });
    return output.stdout;
  }
});
const result = await start(direct_shell_check);
finish(result);
</typescript>"#;
    let provider = lash_core::testing::TestProvider::builder()
        .kind("hirsel-direct-process-e2e")
        .complete(move |_request| async move {
            Ok(LlmResponse {
                parts: vec![LlmOutputPart::Text {
                    text: source.to_string(),
                    response_meta: None,
                }],
                ..LlmResponse::default()
            })
        })
        .build()
        .into_handle();
    let (executor, storage, _log, _dir) = test_event_executor().await;
    let route = executor.anchors.lock().await.active.clone().unwrap();
    let session_id = storage
        .reconcile_agent_tool_surface(
            route.thread_id,
            "direct-process-surface",
            &["shell_run".to_string()],
        )
        .await
        .unwrap()
        .session_id;
    storage
        .bind_thread_execution(
            &storage.history_id().await.unwrap(),
            &session_id,
            DRAIN,
            route.thread_turn_id,
        )
        .await
        .unwrap();
    let protocol = lash_protocol_rlm::RlmProtocolPluginFactory::new(
        hirsel_rlm_config(),
        Arc::new(lash::persistence::InMemoryLashlangArtifactStore::new()),
    );
    let core = lash::LashCore::rlm_builder(lash::TurnBudget::Unbounded, protocol)
        .with_native_queued_work()
        .provider(provider)
        .model(provider_rebind_test_model("hirsel-direct-process-model"))
        .store_factory(Arc::new(
            lash_core::facade_support::InMemorySessionStoreFactory::new(),
        ))
        .effect_host(Arc::new(lash::durability::NativeEffectHost::default()))
        .attachment_store(Arc::new(lash::persistence::InMemoryAttachmentStore::new()))
        .process_env_store(Arc::new(
            lash::persistence::InMemoryProcessExecutionEnvStore::new(),
        ))
        .process_registry(Arc::new(lash_core::TestLocalProcessRegistry::default()))
        .trigger_store(Arc::new(
            lash_core::facade_support::InMemoryTriggerStore::default(),
        ))
        .tools(Arc::new(HirselToolProvider {
            executor,
            coding: Arc::new(NativeCodingBinding::new(
                std::env::current_dir().unwrap().canonicalize().unwrap(),
            )),
        }))
        .plugin(Arc::new(HirselPluginFactory))
        .commit_budget(lash::CommitBudget::bounded(1024 * 1024, 512))
        .queued_work_batching(lash::QueuedWorkBatchingConfig::new(1))
        .build(lash_core::testing::runtime_lease_owner())
        .unwrap();
    let session = core
        .session(&session_id)
        .plugin_option(
            RLM_PROTOCOL_PLUGIN_ID,
            RlmCreateExtras {
                dialect: Some(AGENT_RLM_DIALECT),
                ..RlmCreateExtras::default()
            },
        )
        .unwrap()
        .open()
        .await
        .unwrap();
    session
        .enqueue(lash::TurnInput::text("start the process directly"))
        .id("direct-process-input")
        .ingress(TurnInputIngress::next_turn())
        .send()
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        session.queued_turn().turn_id(DRAIN).run(),
    )
    .await
    .expect("direct process turn completes")
    .unwrap()
    .expect("direct process turn");

    assert_eq!(
        result.final_value(),
        Some(&json!("direct-primitive-ok")),
        "directly started process must execute its Hirsel tool: {result:#?}"
    );
}

#[tokio::test]
async fn absolute_timer_retires_after_delivery() {
    timer_process_case("at: \"2030-01-01T00:00:00Z\"", true).await;
}

#[tokio::test]
async fn recurring_timer_retains_subscription_after_delivery() {
    timer_process_case("every_secs: 60", false).await;
}

async fn timer_process_case(schedule: &str, retire: bool) {
    use lash_core::{LlmOutputPart, llm::types::LlmResponse};

    const DRAIN: &str = "process-e2e-drain";
    let source = r#"<typescript>
const timer_shell_check = defineProcess({
  name: "timer_shell_check",
  signals: {},
  run: async (_event: unknown) => {
    const output = await shell.run({ cmd: "printf primitive-ok" });
    return output.stdout;
  }
});
const source = timer.Schedule({ label: "e2e", in_secs: 1 });
await registerTrigger({
  source,
  target: timer_shell_check,
  inputs: { _event: trigger.event },
  name: "timer shell check"
});
finish("registered");
</typescript>"#
        .replace("in_secs: 1", schedule);
    let expected_trigger = hirsel_proto::TriggerLabel::Timer {
        label: "e2e".into(),
        in_secs: (schedule == "in_secs: 1").then_some(1),
        every_secs: (!retire).then_some(60),
        at: schedule
            .starts_with("at:")
            .then(|| "2030-01-01T00:00:00Z".into()),
    };
    let provider = lash_core::testing::TestProvider::builder()
        .kind("hirsel-process-e2e")
        .complete(move |_request| {
            let source = source.clone();
            async move {
                Ok(LlmResponse {
                    parts: vec![LlmOutputPart::Text {
                        text: source.to_string(),
                        response_meta: None,
                    }],
                    ..LlmResponse::default()
                })
            }
        })
        .build()
        .into_handle();
    let (executor, storage, _log, _dir) = test_event_executor().await;
    let route = executor.anchors.lock().await.active.clone().unwrap();
    let session_id = storage
        .reconcile_agent_tool_surface(
            route.thread_id,
            "process-e2e-surface",
            &["shell_run".to_string()],
        )
        .await
        .unwrap()
        .session_id;
    storage
        .bind_thread_execution(
            &storage.history_id().await.unwrap(),
            &session_id,
            DRAIN,
            route.thread_turn_id,
        )
        .await
        .unwrap();
    let trigger_store = Arc::new(lash_core::facade_support::InMemoryTriggerStore::default());
    let process_registry = Arc::new(lash_core::TestLocalProcessRegistry::default());
    let protocol = lash_protocol_rlm::RlmProtocolPluginFactory::new(
        hirsel_rlm_config(),
        Arc::new(lash::persistence::InMemoryLashlangArtifactStore::new()),
    );
    let core = lash::LashCore::rlm_builder(lash::TurnBudget::Unbounded, protocol)
        .with_native_queued_work()
        .provider(provider)
        .model(provider_rebind_test_model("hirsel-process-e2e-model"))
        .store_factory(Arc::new(
            lash_core::facade_support::InMemorySessionStoreFactory::new(),
        ))
        .effect_host(Arc::new(lash::durability::NativeEffectHost::default()))
        .attachment_store(Arc::new(lash::persistence::InMemoryAttachmentStore::new()))
        .process_env_store(Arc::new(
            lash::persistence::InMemoryProcessExecutionEnvStore::new(),
        ))
        .process_registry(process_registry)
        .trigger_store(trigger_store.clone())
        .tools(Arc::new(HirselToolProvider {
            executor: executor.clone(),
            coding: Arc::new(NativeCodingBinding::new(
                std::env::current_dir().unwrap().canonicalize().unwrap(),
            )),
        }))
        .plugin(Arc::new(HirselPluginFactory))
        .commit_budget(lash::CommitBudget::bounded(1024 * 1024, 512))
        .queued_work_batching(lash::QueuedWorkBatchingConfig::new(1))
        .build(lash_core::testing::runtime_lease_owner())
        .unwrap();
    let session = core
        .session(&session_id)
        .plugin_option(
            RLM_PROTOCOL_PLUGIN_ID,
            RlmCreateExtras {
                dialect: Some(AGENT_RLM_DIALECT),
                ..RlmCreateExtras::default()
            },
        )
        .unwrap()
        .open()
        .await
        .unwrap();
    session
        .enqueue(lash::TurnInput::text("register the process"))
        .id("process-e2e-input")
        .ingress(TurnInputIngress::next_turn())
        .send()
        .await
        .unwrap();
    let registration = session
        .queued_turn()
        .turn_id(DRAIN)
        .run()
        .await
        .unwrap()
        .expect("registration turn");
    assert_eq!(
        registration.final_value(),
        Some(&json!("registered")),
        "registration output: {registration:#?}"
    );

    let subscriptions = trigger_store
        .list_subscriptions(TriggerSubscriptionFilter::for_session(&session_id))
        .await
        .unwrap();
    let subscription = subscriptions.first().expect("registered timer trigger");
    let report = core
        .triggers()
        .emit(
            lash::triggers::TriggerOccurrenceRequest::new(
                TIMER_SOURCE_TYPE,
                subscription.source_key.clone(),
                json!({
                    "label": "e2e",
                    "fired_at": Utc::now().to_rfc3339(),
                    "scheduled_at": Utc::now().to_rfc3339(),
                    "source_key": subscription.source_key,
                    "subscription_key": subscription.subscription_key,
                }),
                "process-e2e-timer",
            )
            .with_source(subscription.source.clone()),
            inline_trigger_scope("process-e2e-timer"),
        )
        .await
        .unwrap();
    let process_id = report
        .started_process_ids()
        .first()
        .cloned()
        .expect("process start");
    let item = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = core
                .processes()
                .session_snapshot(&session_id)
                .await
                .unwrap();
            if let Some(item) = snapshot.items.into_iter().find(|item| {
                item.process.process_id == process_id
                    && !matches!(
                        item.process.lifecycle,
                        lash_core::ProcessStatus::Running | lash_core::ProcessStatus::Waiting
                    )
            }) {
                break item;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("process completes");
    assert_eq!(
        item.process.lifecycle,
        lash_core::ProcessStatus::Completed,
        "terminal process: {item:#?}"
    );
    // The production timer loop tombstones one-shot subscriptions immediately
    // after emission. Projection must use the retained delivery snapshot.
    // Failed/empty emissions must preserve the schedule for a later attempt.
    let mut failed = report.clone();
    failed.deliveries[0].outcome = lash_core::facade_support::TriggerDeliveryEmitOutcome::Failed {
        reason: "start failed".into(),
    };
    for receipt in [
        failed,
        lash_core::facade_support::TriggerEmitReport::empty(),
    ] {
        super::super::timers::retire_delivered_one_shot(
            trigger_store.as_ref(),
            subscription,
            &receipt,
        )
        .await
        .unwrap();
        assert_eq!(
            trigger_store
                .list_subscriptions(TriggerSubscriptionFilter::for_session(&session_id))
                .await
                .unwrap()
                .len(),
            1
        );
    }
    if retire {
        let mut stale = subscription.clone();
        stale.revision += 1;
        assert!(
            super::super::timers::retire_delivered_one_shot(
                trigger_store.as_ref(),
                &stale,
                &report
            )
            .await
            .is_err(),
            "revision rejection must propagate"
        );
    }
    super::super::timers::retire_delivered_one_shot(trigger_store.as_ref(), subscription, &report)
        .await
        .unwrap();
    // Same operation receipt is idempotent across a cleanup retry.
    super::super::timers::retire_delivered_one_shot(trigger_store.as_ref(), subscription, &report)
        .await
        .unwrap();
    let retained = trigger_store
        .list_subscriptions(TriggerSubscriptionFilter::for_session(&session_id))
        .await
        .unwrap();
    let reservations = trigger_store.list_deliveries().await.unwrap();
    super::super::process_projection::tests::assert_folded_process_rows(
        &item,
        subscription,
        &retained,
        &reservations,
        retire,
    );

    assert_eq!(retained.is_empty(), retire);
    let captured = super::super::process_bridge::trigger_registration(
        trigger_store.as_ref(),
        &session_id,
        &item.process.process_id,
        &subscription.subscription_id,
    )
    .await
    .unwrap()
    .expect("retained registration");
    assert_eq!(captured, *subscription);
    let (_, mut delivery) = terminal_process_delivery(route.thread_id, &item).unwrap();
    let hirsel_proto::MessageOrigin::Process {
        trigger,
        subscription_key,
        ..
    } = &mut delivery.origin;
    *trigger = super::super::process_bridge::structured_trigger(&captured);
    *subscription_key = Some(subscription.subscription_key.clone());
    storage.stage_process_delivery(&delivery).await.unwrap();
    let delivered = storage
        .deliver_process_message(&delivery.key)
        .await
        .unwrap();

    assert!(delivered.newly_appended);
    assert_eq!(delivered.message.body, "primitive-ok");
    assert_eq!(
        delivered.message.origin,
        Some(hirsel_proto::MessageOrigin::Process {
            process_id: item.process.process_id,
            name: "timer_shell_check".into(),
            trigger: expected_trigger,
            subscription_key: Some(subscription.subscription_key.clone()),
            outcome: hirsel_proto::ProcessOutcome::Completed,
            result: json!("primitive-ok"),
            error: None,
        })
    );
}

#[tokio::test]
async fn cron_subscription_never_takes_one_shot_retirement_path() {
    let mut record = timer_registration(json!({"expr":"*/5 * * * *"}), 1000);
    record.source_type = "cron.Schedule".into();
    let store = lash_core::facade_support::InMemoryTriggerStore::default();
    let report = lash_core::facade_support::TriggerEmitReport {
        occurrence_id: "cron-occurrence".into(),
        deliveries: vec![lash_core::facade_support::TriggerDeliveryEmitReceipt {
            occurrence_id: "cron-occurrence".into(),
            subscription_id: record.subscription_id.clone(),
            process_id: "cron-run".into(),
            outcome: lash_core::facade_support::TriggerDeliveryEmitOutcome::Started,
        }],
    };
    // The record is deliberately absent from this store: a Delete would fail.
    super::super::timers::retire_delivered_one_shot(&store, &record, &report)
        .await
        .unwrap();
    let rows = super::super::process_projection::process_rows(7, &[], &[record], &[]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, hirsel_proto::ProcessState::Waiting);
    assert!(rows[0].trigger_recurring);
    assert_eq!(rows[0].trigger_enabled, Some(true));
    assert_eq!(rows[0].trigger.as_deref(), Some("cron */5 * * * *"));
}
