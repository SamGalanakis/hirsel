use super::*;

impl LashAgentRuntime {
    pub(super) async fn start(
        config: RuntimeConfig,
        model_selection: Option<ModelSelectionState>,
        tools: ToolSuite,
        broadcaster: broadcast::Sender<HostToClient>,
        broadcast_log: BroadcastLog,
    ) -> anyhow::Result<LashStartup> {
        let provider = match build_provider(&config).await {
            Ok(provider) => provider,
            Err(ProviderUnavailable { message }) => {
                tracing::warn!(%message, "Lash Agent provider unavailable; using degraded runtime");
                return Ok(LashStartup::Unavailable(Arc::new(DegradedAgentRuntime {
                    reason: message,
                    tools,
                    broadcaster,
                    broadcast_log,
                })));
            }
        };

        tokio::fs::create_dir_all(&config.data_dir).await?;
        let lash_dir = config.data_dir.join("lash");
        tokio::fs::create_dir_all(&lash_dir).await?;
        let store_factory = Arc::new(lash_sqlite_store::SqliteSessionStoreFactory::new(
            lash_dir.join("sessions"),
        ));
        let artifact_store =
            Arc::new(lash_sqlite_store::Store::open(&lash_dir.join("artifacts.db")).await?)
                as Arc<dyn lash::persistence::LashlangArtifactStore>;
        let process_env_store =
            Arc::new(lash_sqlite_store::Store::open(&lash_dir.join("process-env.db")).await?);
        let trigger_store = Arc::new(
            lash_sqlite_store::SqliteTriggerStore::open(&lash_dir.join("triggers.db")).await?,
        ) as Arc<dyn TriggerStore>;
        let process_registry = Arc::new(
            lash_sqlite_store::SqliteProcessRegistry::open(
                &lash_dir.join("processes.db"),
                lash_dir.join("sessions"),
            )
            .await?,
        ) as Arc<dyn lash::process::ProcessRegistry>;
        let model_spec = match config.provider_mode {
            ProviderMode::Codex | ProviderMode::OpenRouter => model_selection
                .as_ref()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Lash main-agent runtime requires a selectable model for {:?} mode",
                        config.provider_mode
                    )
                })?
                .model_spec()?,
            ProviderMode::Anthropic => lash::ModelSpec::builder(config.model.clone())
                .variant(ReasoningSelection::ProviderDefault)
                .context_window_tokens(200_000)
                .build()
                .map_err(|error| anyhow::anyhow!("invalid HIRSEL_MODEL metadata: {error}"))?,
        };
        // Execution bounds have no defaults on the plugin config: the host names
        // every one. These match the reference-host budgets — a cell may run a
        // million instructions, for thirty seconds, inside 64 MiB.
        let rlm_config = lash_protocol_rlm::RlmProtocolPluginConfig::new(
            lash_protocol_rlm::ExecutionBound::instructions(1_000_000),
            lash_protocol_rlm::ExecutionBound::secs(30),
            lash_protocol_rlm::ExecutionBound::instructions(64 * 1024 * 1024),
        )
        .with_lashlang_abilities(
            lash_protocol_rlm::LashlangAbilities::default()
                .with_processes()
                .with_triggers(),
        );
        let rlm_factory =
            lash_protocol_rlm::RlmProtocolPluginFactory::new(rlm_config, artifact_store);
        let tool_definitions = hirsel_tool_definitions(&tools.subagent_model_snapshot());
        let tool_surface = agent_tool_surface(&tool_definitions)?;
        let session_bootstrap = tools
            .prepare_agent_session(&tool_surface.fingerprint, &tool_surface.tool_names)
            .await
            .context("prepare main-agent session generation")?;
        let session_guidance = agent_guidance_with_handoff(
            config.agent_guidance.clone(),
            session_bootstrap.handoff_seed.as_deref(),
        );
        let executor = HirselToolExecutor {
            tools: tools.clone(),
            anchors: Arc::new(Mutex::new(TurnAnchorState::default())),
            process_registry: Arc::clone(&process_registry),
        };
        let anchors = executor.anchors.clone();
        let tool_provider = Arc::new(HirselToolProvider { executor });
        let notify = Arc::new(Notify::new());
        let queued_work_driver = QueuedWorkDriver::new(Arc::new(HirselQueuedWorkNotifier {
            notify: Arc::clone(&notify),
        }));
        let core = lash::LashCore::rlm_builder(lash::TurnBudget::Unbounded, rlm_factory)
            .provider(provider)
            .model(model_spec)
            .store_factory(store_factory.clone())
            .attachment_store(Arc::new(lash::persistence::FileAttachmentStore::new(
                lash_dir.join("attachments"),
            )))
            .process_env_store(process_env_store)
            .effect_host(Arc::new(lash::durability::InlineEffectHost::default()))
            .process_registry(process_registry)
            .trigger_store(Arc::clone(&trigger_store))
            .tools(tool_provider)
            .plugin(Arc::new(HirselProcessPluginFactory {
                tools: tools.clone(),
                notify: Arc::clone(&notify),
            }))
            .queued_work_driver(queued_work_driver)
            // lash's documented recommended starting point (1 MiB / 512 nodes),
            // matching its reference hosts; tune if SQLite commit latency drifts.
            .commit_budget(lash::CommitBudget::bounded(1024 * 1024, 512))
            .queued_work_batching(lash::QueuedWorkBatchingConfig::new(1024))
            // A liveness-aware lease identity lets a rebooted host reclaim the
            // session execution lease immediately when the previous holder was
            // a now-dead process on this same host+boot (e.g. after SIGKILL),
            // instead of waiting out the lease TTL. The owner is stable per
            // host; the incarnation is minted once per boot.
            .build(lash_core::LeaseOwnerIdentity::opaque(
                format!("hirsel-host:agent:{}", local_host_id()),
                Uuid::new_v4().to_string(),
            ))?;
        let process_registry = core
            .process_registry()
            .ok_or_else(|| anyhow::anyhow!("Lash process registry was not configured"))?;
        let session = core
            .session(&session_bootstrap.session_id)
            // The agent is prompted and acts in the TypeScript RLM dialect. The
            // pin is durable from the session's first commit, so a session
            // recorded under another dialect is a typed refusal rather than a
            // silent reinterpretation — `agent_tool_surface` folds the dialect
            // into the rotation fingerprint so the switch lands on a fresh
            // session with a handoff seed.
            .rlm_dialect(AGENT_RLM_DIALECT)?
            .prompt_contribution(lash::prompt::PromptContribution::guidance(
                "Hirsel Agent",
                session_guidance,
            ))
            .open()
            .await?;

        let runtime = Arc::new(Self {
            core: core.clone(),
            session,
            session_id: session_bootstrap.session_id,
            tools: tools.clone(),
            broadcaster: broadcaster.clone(),
            broadcast_log,
            notify,
            pump_lock: Mutex::new(()),
            anchors,
            active_turn_id: Arc::new(Mutex::new(None)),
            drain_seq: AtomicU64::new(0),
            drain_boot_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            drain_retry_scheduled: AtomicBool::new(false),
            drain_retry_attempts: AtomicU64::new(0),
            model_selection,
            agent_guidance: Arc::from(config.agent_guidance),
        });
        runtime.spawn_observation_bridge();
        runtime.spawn_turn_pump();
        runtime.spawn_process_terminal_bridge(process_registry.clone(), store_factory.clone());
        runtime.spawn_subagent_control_bridge(process_registry.clone());
        runtime.spawn_timer_trigger_source(trigger_store);
        runtime
            .restore_subagent_processes_after_restart(
                process_registry.clone(),
                store_factory.clone(),
            )
            .await;
        runtime
            .abandon_recovered_subagent_runtime_processes(process_registry, store_factory)
            .await;
        runtime.resume_active_monitors().await;
        runtime.notify_if_work_pending().await;
        tracing::info!(
            model = %runtime.session.policy_snapshot().model.id,
            variant = ?runtime.session.policy_snapshot().model.variant,
            provider = ?config.provider_mode,
            data_dir = %config.data_dir.display(),
            session_id = %runtime.session_id,
            "Lash Agent runtime opened session"
        );
        Ok(LashStartup::Ready(runtime))
    }

    pub(super) async fn apply_selected_model(&self) -> anyhow::Result<()> {
        let Some(selection) = &self.model_selection else {
            return Ok(());
        };
        let spec = selection.model_spec()?;
        if self.session.policy_snapshot().model == spec {
            return Ok(());
        }
        self.session
            .configure(lash::SessionConfigPatch {
                model: Some(spec),
                ..lash::SessionConfigPatch::default()
            })
            .await
            .context("apply selected model to main-agent Lash session")
    }

    pub(super) async fn refresh_subagent_model_tools(
        &self,
        catalog: &SubagentModelCatalog,
    ) -> anyhow::Result<()> {
        let encoded =
            serde_json::to_vec(catalog).context("serialize Sub-agent model tool contract")?;
        let fingerprint = format!("{:x}", Sha256::digest(encoded));
        self.session
            .commands()
            .refresh_tool_catalog(
                "Sub-agent model settings changed",
                format!("subagent-model-settings:{fingerprint}"),
            )
            .await
            .context("enqueue Sub-agent model tool-catalog refresh")?;
        self.notify.notify_one();
        Ok(())
    }

    pub(super) async fn enqueue_inner(&self, turn: OwnerTurn) -> anyhow::Result<()> {
        let source_key = owner_turn_source_key(&turn.client_id);
        {
            let mut anchors = self.anchors.lock().await;
            anchors.pending_by_source_key.insert(
                source_key.clone(),
                TurnAnchors {
                    owner_message_id: turn.message_id,
                    task_action_event_id: turn.task_action.as_ref().map(|context| context.event.id),
                },
            );
        }
        let ingress = self.ingress_for_mode(turn.mode).await;
        let input = match owner_turn_input(&turn).await {
            Ok(input) => input,
            Err(error) => {
                self.anchors
                    .lock()
                    .await
                    .pending_by_source_key
                    .remove(&source_key);
                return Err(error);
            }
        };
        if let Err(error) = self
            .session
            .enqueue(input)
            .id(turn.client_id)
            .ingress(ingress)
            .send()
            .await
        {
            self.anchors
                .lock()
                .await
                .pending_by_source_key
                .remove(&source_key);
            return Err(error.into());
        }
        self.notify.notify_one();
        Ok(())
    }

    pub(super) async fn ingress_for_mode(&self, mode: SendMode) -> TurnInputIngress {
        match mode {
            SendMode::NextTurn => TurnInputIngress::next_turn(),
            SendMode::Send => {
                let active_turn_id = self.active_turn_id.lock().await.clone();
                match active_turn_id {
                    Some(turn_id) => TurnInputIngress::active_turn(
                        turn_id,
                        TurnInputCheckpointBoundary::AfterWork,
                    ),
                    None => TurnInputIngress::next_turn(),
                }
            }
        }
    }

    pub(super) async fn notify_if_work_pending(&self) {
        if self.work_pending().await {
            self.notify.notify_one();
        }
    }

    pub(super) async fn work_pending(&self) -> bool {
        let pending_inputs = match self.session.pending_turn_inputs().await {
            Ok(inputs) => !inputs.is_empty(),
            Err(error) => {
                tracing::warn!(%error, "failed to inspect pending Lash turn inputs");
                false
            }
        };
        let queued_work = match self.session.queued_work().await {
            Ok(work) => !work.is_empty(),
            Err(error) => {
                tracing::warn!(%error, "failed to inspect pending Lash queued work");
                false
            }
        };
        pending_inputs || queued_work
    }

    /// Schedule a single delayed pump re-notify with exponential backoff
    /// (2s doubling to a 30s cap). Used when a queued-work drain came back
    /// empty while work is still pending: the session execution lease is held
    /// elsewhere, and without a retry the pending work would sit unclaimed
    /// forever (no other code path re-notifies the pump).
    pub(super) fn schedule_drain_retry(self: &Arc<Self>) {
        if self.drain_retry_scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        let attempt = self.drain_retry_attempts.fetch_add(1, Ordering::AcqRel);
        let delay = Duration::from_secs((2u64 << attempt.min(4)).min(30));
        tracing::info!(
            attempt = attempt + 1,
            delay_secs = delay.as_secs(),
            "queued work is pending but the drain claimed nothing (session \
             execution lease busy); scheduling a delayed drain retry"
        );
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            runtime
                .drain_retry_scheduled
                .store(false, Ordering::Release);
            runtime.notify.notify_one();
        });
    }

    pub(super) async fn restore_subagent_processes_after_restart(
        &self,
        process_registry: Arc<dyn lash::process::ProcessRegistry>,
        store_factory: Arc<dyn lash::persistence::SessionStoreFactory>,
    ) {
        let abandoned = match self.tools.restore_subagent_processes_after_restart().await {
            Ok(abandoned) => abandoned,
            Err(error) => {
                tracing::warn!(%error, "failed to restore Sub-agent process metadata at boot");
                return;
            }
        };
        for process_id in abandoned {
            if self
                .append_subagent_abandoned_event(
                    process_registry.as_ref(),
                    store_factory.as_ref(),
                    &process_id,
                )
                .await
            {
                self.notify.notify_one();
            }
        }
    }

    pub(super) async fn abandon_recovered_subagent_runtime_processes(
        &self,
        process_registry: Arc<dyn lash::process::ProcessRegistry>,
        store_factory: Arc<dyn lash::persistence::SessionStoreFactory>,
    ) {
        // The registry only exposes non-terminal rows as a cursor-paged scan;
        // boot recovery walks every page rather than assuming one fits.
        let mut continuation = None;
        loop {
            let page = match process_registry
                .list_non_terminal_page(NON_TERMINAL_SCAN_PAGE, continuation.take())
                .await
            {
                Ok(page) => page,
                Err(error) => {
                    tracing::warn!(%error, "failed to list non-terminal Lash processes at boot");
                    return;
                }
            };
            let more = page.continuation.clone();
            for record in page.records {
                if !is_hirsel_subagent_process_record(&record) {
                    continue;
                }
                if self
                    .append_subagent_abandoned_event(
                        process_registry.as_ref(),
                        store_factory.as_ref(),
                        &record.id,
                    )
                    .await
                {
                    self.notify.notify_one();
                }
            }
            match more {
                Some(cursor) => continuation = Some(cursor),
                None => break,
            }
        }
    }

    pub(super) async fn append_subagent_abandoned_event(
        &self,
        process_registry: &dyn lash::process::ProcessRegistry,
        store_factory: &dyn lash::persistence::SessionStoreFactory,
        process_id: &str,
    ) -> bool {
        let request =
            ProcessEventAppendRequest::new(SUBAGENT_ABANDONED, subagent_abandoned_payload())
                .with_replay_key(format!("hirsel-subagent:{process_id}:{SUBAGENT_ABANDONED}"));
        match process_registry.append_event(process_id, request).await {
            Ok(result) => match enqueue_process_wake(store_factory, result.wake_delivery).await {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        process_id = %process_id,
                        "failed to enqueue abandoned Sub-agent process wake"
                    );
                    false
                }
            },
            Err(error) => {
                tracing::warn!(
                    %error,
                    process_id = %process_id,
                    "failed to append abandoned Sub-agent process event"
                );
                match process_registry
                    .complete_process(
                        process_id,
                        subagent_abandoned_output(),
                        ProcessCompletionAuthority::ReconciledAbandon,
                    )
                    .await
                {
                    Ok(_) => true,
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            process_id = %process_id,
                            "failed to complete recovered Sub-agent process as abandoned"
                        );
                        false
                    }
                }
            }
        }
    }

    pub(super) async fn resume_active_monitors(&self) {
        let monitors = match self.tools.active_monitors().await {
            Ok(monitors) => monitors,
            Err(error) => {
                tracing::warn!(%error, "failed to list active monitors at boot");
                return;
            }
        };
        for monitor in monitors {
            let existing = match self.core.processes().get(&monitor.id).await {
                Ok(existing) => existing,
                Err(error) => {
                    tracing::warn!(%error, monitor_id = %monitor.id, "failed to inspect monitor process at boot");
                    continue;
                }
            };
            if existing.as_ref().is_some_and(|process| !process.terminal) {
                continue;
            }
            let scope = inline_trigger_scope(format!("monitor-resume:{}", monitor.id));
            if let Err(error) = self
                .core
                .processes()
                .start(monitor_start_request(&monitor, &self.session_id), scope)
                .await
            {
                tracing::warn!(%error, monitor_id = %monitor.id, "failed to resume monitor process");
            } else {
                self.tools.broadcast_monitor_upsert(&monitor);
            }
        }
        match self.core.durable_process_worker_config() {
            Ok(config) => {
                if let Err(error) = lash::durability::DurableProcessWorker::new(config)
                    .drive_pending_processes()
                    .await
                {
                    tracing::warn!(%error, "failed to drive recovered monitor processes at boot");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to build monitor recovery worker");
            }
        }
    }

    pub(super) fn spawn_turn_pump(self: &Arc<Self>) {
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                runtime.notify.notified().await;
                let _guard = runtime.pump_lock.lock().await;
                loop {
                    if let Err(error) = runtime.apply_selected_model().await {
                        tracing::warn!(%error, "failed to reconcile main-agent model before queued turn");
                        break;
                    }
                    let drain_id = runtime.next_drain_id();
                    runtime.activate_anchor_for_next_drain().await;
                    runtime.set_active_turn_id(Some(drain_id.clone())).await;
                    let result = runtime
                        .session
                        .queued_turn()
                        .drain_id(drain_id.clone())
                        .run()
                        .await;
                    runtime.clear_active_turn_id(&drain_id).await;
                    runtime.clear_active_anchor_and_prune().await;
                    match result {
                        Ok(Some(output)) => {
                            runtime.drain_retry_attempts.store(0, Ordering::Release);
                            if let Err(error) = materialize_turn_chat(&runtime.tools, &output).await
                            {
                                tracing::warn!(%error, "failed to deliver Agent turn output to Chat");
                            }
                            continue;
                        }
                        Ok(None) => {
                            // An empty drain while durable work is still queued
                            // means another owner's session execution lease is
                            // blocking the claim (e.g. a stale lease after an
                            // unclean shutdown). Nothing else re-notifies the
                            // pump, so schedule a bounded delayed retry until
                            // the lease expires or is reclaimed.
                            if runtime.work_pending().await {
                                runtime.schedule_drain_retry();
                            }
                            break;
                        }
                        Err(error) => {
                            runtime.handle_turn_error(error).await;
                            break;
                        }
                    }
                }
            }
        });
    }

    pub(super) fn next_drain_id(&self) -> String {
        let seq = self.drain_seq.fetch_add(1, Ordering::Relaxed) + 1;
        // The boot epoch keeps drain replay keys unique across restarts:
        // a per-boot counter alone collides with drains already committed in
        // a persistent session store (store_commit_failed on first turn).
        format!("host-queue-drain:{}:{seq}", self.drain_boot_ms)
    }

    pub(super) async fn set_active_turn_id(&self, id: Option<String>) {
        *self.active_turn_id.lock().await = id;
    }

    pub(super) async fn clear_active_turn_id(&self, id: &str) {
        let mut active = self.active_turn_id.lock().await;
        if active.as_deref() == Some(id) {
            *active = None;
        }
    }

    pub(super) async fn activate_anchor_for_next_drain(&self) {
        let pending = match self.session.pending_turn_inputs().await {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(%error, "failed to inspect pending turn inputs for Ping anchor");
                self.anchors.lock().await.active = None;
                return;
            }
        };
        let mut anchors = self.anchors.lock().await;
        anchors.active = pending
            .iter()
            .filter_map(|input| input.source_key.as_ref())
            .find_map(|source_key| anchors.pending_by_source_key.get(source_key).cloned());
    }

    pub(super) async fn clear_active_anchor_and_prune(&self) {
        let live_source_keys = match self.session.pending_turn_inputs().await {
            Ok(pending) => Some(
                pending
                    .into_iter()
                    .filter_map(|input| input.source_key)
                    .collect::<HashSet<_>>(),
            ),
            Err(error) => {
                tracing::warn!(%error, "failed to prune pending turn anchors");
                None
            }
        };
        let mut anchors = self.anchors.lock().await;
        anchors.active = None;
        if let Some(live_source_keys) = live_source_keys {
            anchors
                .pending_by_source_key
                .retain(|source_key, _| live_source_keys.contains(source_key));
        }
    }

    pub(super) async fn cancel_turn(&self) -> anyhow::Result<()> {
        self.session.cancel_running_turns();
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                state: AgentActivityState::Idle,
                text: None,
                sc: None,
            },
        );
        Ok(())
    }

    pub(super) async fn cancel_queued(
        &self,
        client_id: &str,
    ) -> anyhow::Result<CancelQueuedResult> {
        let target =
            lash::PendingTurnInputCancelTarget::source_key(owner_turn_source_key(client_id));
        let mut results = self.session.cancel_pending_turn_inputs([target]).await?;
        let outcome = results
            .pop()
            .map(|result| result.outcome)
            .unwrap_or(lash::PendingTurnInputCancelOutcome::NotFound);
        match outcome {
            lash::PendingTurnInputCancelOutcome::Cancelled(_) => Ok(CancelQueuedResult::Cancelled),
            lash::PendingTurnInputCancelOutcome::AlreadyClaimed { .. }
            | lash::PendingTurnInputCancelOutcome::AlreadyCompleted(_)
            | lash::PendingTurnInputCancelOutcome::AlreadyCancelled(_)
            | lash::PendingTurnInputCancelOutcome::NotFound => {
                Ok(CancelQueuedResult::AlreadyClaimed)
            }
        }
    }

    pub(super) async fn start_monitor_process(&self, record: &MonitorRecord) -> anyhow::Result<()> {
        self.core
            .processes()
            .start(
                monitor_start_request(record, &self.session_id),
                inline_trigger_scope(format!("monitor-debug-create:{}", record.id)),
            )
            .await?;
        self.notify.notify_one();
        Ok(())
    }

    pub(super) async fn cancel_monitor_process(&self, monitor_id: &str) -> anyhow::Result<()> {
        match self
            .core
            .processes()
            .cancel(
                monitor_id,
                inline_trigger_scope(format!("monitor-debug-cancel:{monitor_id}")),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(error) => {
                tracing::debug!(%error, monitor_id = %monitor_id, "monitor process cancel returned an error");
                Ok(())
            }
        }
    }

    pub(super) async fn enqueue_monitor_wake(&self, text: String) -> anyhow::Result<()> {
        self.session
            .enqueue(TurnInput::text(text))
            .id(format!("monitor-wake-{}", Uuid::new_v4()))
            .ingress(TurnInputIngress::next_turn())
            .send()
            .await?;
        self.notify.notify_one();
        Ok(())
    }

    pub(super) async fn handle_turn_error(&self, error: lash::EmbedError) {
        tracing::warn!(%error, "Lash queued turn failed");
        // No ref: an error right under the Owner's message renders as a noisy
        // self-quote in the client.
        match self
            .tools
            .chat_send(format!("Agent turn failed: {error}"), None)
            .await
        {
            Ok(_) => {}
            Err(chat_error) => {
                tracing::warn!(%chat_error, "failed to write Agent turn error to Chat");
            }
        }
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                state: AgentActivityState::Idle,
                text: None,
                sc: None,
            },
        );
    }
}

pub(super) fn scheduled_digest_label(label: &str) -> Option<&str> {
    label
        .strip_prefix("digest:")
        .map(str::trim)
        .filter(|label| !label.is_empty())
}

pub(super) struct HirselQueuedWorkNotifier {
    pub(super) notify: Arc<Notify>,
}

#[async_trait]
impl QueuedWorkRunHandle for HirselQueuedWorkNotifier {
    async fn run_queued_work(
        &self,
        _request: QueuedWorkRunRequest,
    ) -> Result<(), lash::runtime::QueuedWorkRunError> {
        self.notify.notify_one();
        Ok(())
    }
}

/// A stable per-machine id for lease owner liveness. Lease reclaim only
/// compares it between processes that already share the same session store
/// (a local sqlite file), so the hostname is plenty; the boot id and pid
/// carried alongside it do the real liveness discrimination.
pub(super) fn local_host_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "local".to_string())
}

pub(super) fn inline_trigger_scope(
    scope_id: impl Into<String>,
) -> lash_core::ScopedEffectController<'static> {
    lash_core::ScopedEffectController::shared(
        Arc::new(lash::runtime::InlineRuntimeEffectController::default()),
        lash_core::ExecutionScope::runtime_operation(scope_id.into()),
    )
    .expect("inline timer trigger occurrence execution scope")
}
