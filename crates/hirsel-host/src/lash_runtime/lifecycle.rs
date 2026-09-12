use super::*;

pub(super) async fn reconcile_opened_session_provider(
    session: &lash::LashSession,
    provider: &ProviderHandle,
    model: &lash::ModelSpec,
) -> anyhow::Result<()> {
    let old_provider_id = session.policy_snapshot().recorded_provider_id().to_string();
    let new_provider_id = provider.kind();
    if old_provider_id == new_provider_id {
        return Ok(());
    }

    session
        .admin()
        .config()
        .update(lash::SessionConfigPatch {
            provider: Some(provider.clone()),
            model: Some(model.clone()),
            ..lash::SessionConfigPatch::default()
        })
        .await
        .context("rebind reopened main-agent Lash session to the booted provider")?;
    tracing::info!(
        session_id = %session.session_id(),
        old_provider = %old_provider_id,
        new_provider = %new_provider_id,
        "Lash Agent session provider rebound"
    );
    Ok(())
}

impl LashAgentRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn start(
        config: RuntimeConfig,
        model_selection: Option<ModelSelectionState>,
        tools: ToolSuite,
        broadcaster: broadcast::Sender<HostToClient>,
        broadcast_log: BroadcastLog,
        thread_id: u64,
        history_id: String,
        tasks: RuntimeTasks,
        capacity: Arc<tokio::sync::Semaphore>,
    ) -> anyhow::Result<LashStartup> {
        let provider = match build_provider(&config).await {
            Ok(provider) => provider,
            Err(ProviderUnavailable { message }) => {
                tracing::warn!(%message, "Lash Agent provider unavailable; using degraded runtime");
                return Ok(LashStartup::Unavailable(Arc::new(DegradedAgentRuntime {
                    reason: message,
                })));
            }
        };

        tokio::fs::create_dir_all(&config.data_dir).await?;
        let lash_dir = config
            .data_dir
            .join("thread-runtime")
            .join(&history_id)
            .join(thread_id.to_string());
        tokio::fs::create_dir_all(&lash_dir).await?;
        let store_factory = Arc::new(
            lash_sqlite_store::SqliteSessionStoreFactory::new_with_process_registry(
                lash_dir.join("sessions"),
                lash_dir.join("processes.db"),
            ),
        );
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
        let rlm_config = lash_protocol_rlm::RlmProtocolPluginConfig::builder()
            .instruction_limit(lash_protocol_rlm::InstructionBound::instructions(1_000_000))
            .wall_clock(lash_protocol_rlm::WallClockBound::secs(30))
            .memory_limit(lash_protocol_rlm::MemoryBound::mebibytes(64))
            .build()
            .with_lashlang_abilities(
                lash_protocol_rlm::RlmAbilities::default()
                    .with_processes()
                    .with_triggers(),
            );
        let rlm_factory =
            lash_protocol_rlm::RlmProtocolPluginFactory::new(rlm_config, artifact_store);
        let mut tool_definitions = hirsel_tool_definitions(&tools.subagent_model_snapshot());
        // Plugins are booted before the agent runtime, so the tools of every
        // enabled plugin are part of the first tool-surface fingerprint rather
        // than rotating the session immediately after startup.
        tool_definitions.extend(tools.plugin_tools().definitions());
        let tool_surface = agent_tool_surface(&tool_definitions)?;
        let session_bootstrap = tools
            .prepare_agent_session(
                thread_id,
                &tool_surface.fingerprint,
                &tool_surface.tool_names,
            )
            .await
            .context("prepare main-agent session generation")?;
        let session_guidance = agent_guidance_with_handoff(
            config.prompts.agent_guidance(),
            session_bootstrap.handoff_seed.as_deref(),
        );
        let executor = HirselToolExecutor {
            tools: tools.clone(),
            anchors: Arc::new(Mutex::new(TurnAnchorState::default())),
        };
        let anchors = executor.anchors.clone();
        let tool_provider = Arc::new(HirselToolProvider { executor });
        let notify = Arc::new(Notify::new());
        // ADR-0015's dispatcher cannot exist yet — it escalates into a runtime
        // that is built below, and the monitor engine that feeds it is
        // registered while the core is still under construction. The handle
        // closes that loop; it is installed at the end of this function.
        let fork_wake = crate::fork_wake::ForkWakeHandle::default();
        let queued_work_driver = NativeQueuedWork::new(Arc::new(HirselQueuedWorkNotifier {
            notify: Arc::clone(&notify),
        }));
        let core = lash::LashCore::rlm_builder(lash::TurnBudget::Unbounded, rlm_factory)
            .provider(provider.clone())
            .model(model_spec.clone())
            .store_factory(store_factory.clone())
            .attachment_store(Arc::new(lash::persistence::FileAttachmentStore::new(
                lash_dir.join("attachments"),
            )))
            .process_env_store(process_env_store)
            .effect_host(Arc::new(lash::durability::NativeEffectHost::default()))
            .process_registry(process_registry)
            .trigger_store(Arc::clone(&trigger_store))
            .tools(tool_provider)
            .plugin(Arc::new(HirselProcessPluginFactory {
                history_id: history_id.clone(),
                thread_id,
                tools: tools.clone(),
                fork_wake: fork_wake.clone(),
            }))
            .with_queued_work(Arc::new(queued_work_driver))
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
        let session = core
            .session(&session_bootstrap.session_id)
            // The agent is prompted and acts in the TypeScript RLM dialect. The
            // pin is durable from the session's first commit, so a session
            // recorded under another dialect is a typed refusal rather than a
            // silent reinterpretation — `agent_tool_surface` folds the dialect
            // into the rotation fingerprint so the switch lands on a fresh
            // session with a handoff seed.
            .plugin_option(
                RLM_PROTOCOL_PLUGIN_ID,
                RlmCreateExtras {
                    dialect: Some(AGENT_RLM_DIALECT),
                    ..RlmCreateExtras::default()
                },
            )?
            .prompt_contribution(lash::prompt::PromptContribution::guidance(
                "Hirsel Agent",
                session_guidance,
            ))
            .open()
            .await?;
        reconcile_opened_session_provider(&session, &provider, &model_spec).await?;

        let runtime = Arc::new(Self {
            thread_id,
            history_id,
            tasks,
            provider_id: config.boot_plan.label().into(),
            capacity,
            core: core.clone(),
            provider,
            session,
            session_id: session_bootstrap.session_id,
            tools: tools.clone(),
            broadcaster: broadcaster.clone(),
            broadcast_log,
            notify,
            pump_lock: Mutex::new(()),
            request_lock: Mutex::new(()),
            anchors,
            active_turn_id: Arc::new(Mutex::new(None)),
            timeline_commits: TimelineCommitBarrier::default(),
            drain_seq: AtomicU64::new(0),
            drain_boot_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            drain_retry_scheduled: AtomicBool::new(false),
            drain_retry_attempts: AtomicU64::new(0),
            model_selection,
            prompts: config.prompts.clone(),
            handoff_seed: session_bootstrap.handoff_seed,
            fork_wake: fork_wake.clone(),
        });
        fork_wake.install(runtime.build_fork_wake());
        runtime.reconcile_unowned_inputs().await?;
        runtime.spawn_observation_bridge();
        runtime.spawn_turn_pump();
        runtime.notify.notify_one();
        runtime.spawn_timer_trigger_source(trigger_store);
        runtime.notify_if_work_pending().await;
        tracing::info!(
            model = %runtime.session.policy_snapshot().model.id,
            variant = ?runtime.session.policy_snapshot().model.variant,
            provider = config.boot_plan.label(),
            env_mode = ?config.provider_mode,
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
            .admin()
            .config()
            .update(lash::SessionConfigPatch {
                model: Some(spec),
                ..lash::SessionConfigPatch::default()
            })
            .await
            .context("apply selected model to main-agent Lash session")
    }

    /// Reconcile the live session's prompt with the Owner's configuration.
    ///
    /// The system prompt is rebuilt from session policy at the start of every
    /// turn, so replacing the session prompt layer here takes effect on the
    /// next turn and never mid-turn. Called before each queued drain (so a hand
    /// edit of `hirsel.toml` lands without a restart) and right after a
    /// Settings edit (so the change is applied by the time the op returns).
    /// Idempotent: an unchanged layer is not written back.
    pub(super) async fn apply_agent_prompt(&self) -> anyhow::Result<()> {
        let guidance = agent_guidance_with_handoff(
            self.prompts.agent_guidance(),
            self.handoff_seed.as_deref(),
        );
        let prompt = lash::prompt::PromptLayer::new().with_contribution(
            lash::prompt::PromptContribution::guidance("Hirsel Agent", guidance),
        );
        if self.session.policy_snapshot().prompt == prompt {
            return Ok(());
        }
        self.session
            .admin()
            .config()
            .update(lash::SessionConfigPatch {
                prompt: Some(prompt),
                ..lash::SessionConfigPatch::default()
            })
            .await
            .context("apply the Owner's Agent prompt to the main-agent Lash session")
    }

    pub(super) async fn refresh_subagent_model_tools(
        &self,
        catalog: &SubagentModelCatalog,
    ) -> anyhow::Result<()> {
        let encoded = serde_json::to_vec(catalog).context("serialize delegation tool contract")?;
        let fingerprint = format!("{:x}", Sha256::digest(encoded));
        self.session
            .admin()
            .commands()
            .refresh_tool_catalog(
                "Delegation executor settings changed",
                format!("delegation-settings:{fingerprint}"),
            )
            .await
            .context("enqueue delegation tool-catalog refresh")?;
        self.notify.notify_one();
        Ok(())
    }

    /// Toggling a plugin adds or removes agent tools, which changes the tool
    /// surface. The refresh goes through the same catalog-refresh command the
    /// Sub-agent model settings use; the surface fingerprint rotates the agent
    /// session on the next generation check, which is accepted behaviour for a
    /// deliberate owner action.
    pub(super) async fn refresh_plugin_tools(&self, tool_names: &[String]) -> anyhow::Result<()> {
        let fingerprint = format!("{:x}", Sha256::digest(tool_names.join("\n").as_bytes()));
        self.session
            .admin()
            .commands()
            .refresh_tool_catalog(
                "Plugin enablement changed",
                format!("plugin-tools:{fingerprint}"),
            )
            .await
            .context("enqueue plugin tool-catalog refresh")?;
        self.notify.notify_one();
        Ok(())
    }

    pub(super) async fn enqueue_inner(&self, turn: OwnerTurn) -> anyhow::Result<()> {
        self.enqueue_thread_request(turn).await
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
        let requests = self
            .tools
            .storage()
            .pending_thread_requests()
            .await
            .map(|requests| {
                requests
                    .iter()
                    .any(|(_, p)| p["thread_id"].as_u64() == Some(self.thread_id))
            })
            .unwrap_or(true);
        pending_inputs || queued_work || requests
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
        self.tasks.spawn(async move {
            tokio::time::sleep(delay).await;
            runtime
                .drain_retry_scheduled
                .store(false, Ordering::Release);
            runtime.notify.notify_one();
        });
    }

    pub(super) fn spawn_turn_pump(self: &Arc<Self>) {
        let runtime = Arc::clone(self);
        self.tasks.spawn(async move {
            loop {
                runtime.notify.notified().await;
                let _guard = runtime.pump_lock.lock().await;
                loop {
                    let Ok(_capacity) = runtime.capacity.acquire().await else { return; };
                    if let Err(error) = runtime.apply_agent_prompt().await {
                        tracing::warn!(%error, "failed to reconcile the Agent prompt before queued turn");
                        runtime.schedule_drain_retry();
                        break;
                    }
                    let request_id = match runtime.admit_next_thread_request().await {
                        Ok(id) => id,
                        Err(error) => {
                            tracing::warn!(%error, "failed to admit Thread request");
                            runtime.schedule_drain_retry();
                            break;
                        }
                    };
                    if request_id.is_none() {
                        match runtime.activate_background_turn().await {
                            Ok(true) => {}
                            Ok(false) => break,
                            Err(error) => {
                                tracing::warn!(%error, "failed to establish background Thread ownership");
                                runtime.schedule_drain_retry();
                                break;
                            }
                        }
                    }
                    let drain_id = runtime
                        .active_turn_id
                        .lock()
                        .await
                        .clone()
                        .expect("every admitted drain has an execution identity");
                    let result = runtime.run_admitted_drain(&drain_id).await;

                    match result {
                        Ok(QueuedTurnDrain::Ran(output)) => {
                            runtime.drain_retry_attempts.store(0, Ordering::Release);
                            let delivery = if let Some(id) = &request_id {
                                runtime.finish_thread_request(id, Some(&output)).await
                            } else {
                                runtime.finish_active_thread(Some(&output)).await
                            };
                            if let Err(error) = delivery {
                                tracing::warn!(%error, "failed to persist Thread output");
                                if let Some(id) = &request_id {
                                    // Inference already consumed this request. Projection failure
                                    // must never resubmit it or re-run side effects.
                                    if let Err(error) =
                                        runtime.finish_thread_request(id, None).await
                                    {
                                        runtime.clear_active_turn_id(&drain_id).await;
                                        tracing::error!(%error,"cannot settle failed Thread projection; stopping pump until restart");
                                        return;
                                    }
                                } else if let Err(error) = runtime.finish_active_thread(None).await
                                {
                                    runtime.clear_active_turn_id(&drain_id).await;
                                    tracing::error!(%error, "cannot settle failed background Thread projection");
                                    return;
                                }
                            }
                            runtime.clear_active_turn_id(&drain_id).await;
                            runtime.clear_active_anchor().await;
                            continue;
                        }
                        Ok(QueuedTurnDrain::Empty(_)) => {
                            // An empty drain while durable work is still queued
                            // means another owner's session execution lease is
                            // blocking the claim (e.g. a stale lease after an
                            // unclean shutdown). Nothing else re-notifies the
                            // pump, so schedule a bounded delayed retry until
                            // the lease expires or is reclaimed.
                            if runtime.work_pending().await {
                                runtime.schedule_drain_retry();
                            }
                            runtime.clear_active_turn_id(&drain_id).await;
                            break;
                        }
                        Err(error) => {
                            if let Some(id) = &request_id
                                && let Err(delivery_error) =
                                    runtime.finish_thread_request(id, None).await
                            {
                                runtime.clear_active_turn_id(&drain_id).await;
                                tracing::error!(%delivery_error, "cannot settle failed Thread turn; stopping pump until restart");
                                return;
                            }
                            if request_id.is_none()
                                && let Err(delivery_error) =
                                    runtime.finish_active_thread(None).await
                            {
                                runtime.clear_active_turn_id(&drain_id).await;
                                tracing::error!(%delivery_error, "cannot settle failed background Thread turn");
                                return;
                            }
                            runtime.clear_active_turn_id(&drain_id).await;
                            runtime.handle_turn_error(error).await;
                            runtime.clear_active_anchor().await;
                            break;
                        }
                    }
                }
            }
        });
    }

    pub(super) fn next_drain_id(&self, route: &TurnAnchors) -> String {
        let seq = self.drain_seq.fetch_add(1, Ordering::Relaxed) + 1;
        // The boot epoch keeps drain replay keys unique across restarts:
        // a per-boot counter alone collides with drains already committed in
        // a persistent session store (store_commit_failed on first turn).
        format!(
            "host-queue-drain:{}:{seq}:thread:{}:turn:{}",
            self.drain_boot_ms,
            route.thread_id,
            route.thread_turn_id.expect("drain turn identity")
        )
    }

    pub(super) async fn set_active_turn_id(&self, id: Option<String>) {
        *self.active_turn_id.lock().await = id;
    }

    pub(super) async fn clear_active_turn_id(&self, id: &str) {
        let mut active = self.active_turn_id.lock().await;
        if active.as_deref() == Some(id) {
            *active = None;
            self.timeline_commits.clear(id).await;
        }
    }

    pub(super) async fn activate_background_turn(&self) -> anyhow::Result<bool> {
        let _request_guard = self.request_lock.lock().await;
        if let Some(route) = self.anchors.lock().await.active.clone() {
            let drain_id = self.next_drain_id(&route);
            self.tools
                .storage()
                .bind_thread_execution(
                    &self.history_id,
                    &self.session_id,
                    &drain_id,
                    route.thread_turn_id.expect("owned turn"),
                )
                .await?;
            self.set_active_turn_id(Some(drain_id)).await;
            return Ok(true);
        }
        if self.session.queued_work().await?.is_empty()
            && self.session.pending_turn_inputs().await?.is_empty()
        {
            return Ok(false);
        }
        let turn = self
            .tools
            .storage()
            .start_thread_turn(self.thread_id, None)
            .await?;
        let route = TurnAnchors {
            request_id: None,
            thread_id: self.thread_id,
            thread_turn_id: Some(turn.id),
        };
        let drain_id = self.next_drain_id(&route);
        self.tools
            .storage()
            .bind_thread_execution(&self.history_id, &self.session_id, &drain_id, turn.id)
            .await?;
        self.set_active_turn_id(Some(drain_id)).await;
        self.anchors.lock().await.active = Some(route);
        self.tools.publish_thread_turn(turn).await;
        Ok(true)
    }

    pub(super) async fn clear_active_anchor(&self) {
        self.anchors.lock().await.active = None;
    }

    pub(super) async fn cancel_turn(&self) -> anyhow::Result<()> {
        self.cancel_owned_turn(None).await
    }

    pub(super) async fn cancel_queued(
        &self,
        client_id: &str,
    ) -> anyhow::Result<CancelQueuedResult> {
        self.cancel_thread_request(client_id).await
    }

    pub(super) async fn start_monitor_process(&self, record: &MonitorRecord) -> anyhow::Result<()> {
        self.core
            .processes()
            .start(
                monitor_start_request(
                    record,
                    &self.session_id,
                    host_process_env_spec(self.session.policy_snapshot()),
                ),
                inline_trigger_scope(format!("monitor-debug-create:{}", record.id)),
            )
            .await?;
        self.notify.notify_one();
        Ok(())
    }

    /// Build the ADR-0015 dispatcher for this runtime.
    ///
    /// The sink holds a [`std::sync::Weak`] back to the runtime: the runtime
    /// owns the handle that owns the dispatcher that owns the sink, so a strong
    /// reference here would be a cycle that never drops.
    pub(super) fn build_fork_wake(self: &Arc<Self>) -> Arc<crate::fork_wake::ForkWake> {
        crate::fork_wake::ForkWake::new(
            self.thread_id,
            self.history_id.clone(),
            crate::fork_wake::LashForkRunner::new(
                self.core.clone(),
                self.provider.clone(),
                self.prompts.clone(),
                self.session_id.clone(),
            ),
            Arc::new(MainSessionBriefSink {
                runtime: Arc::downgrade(self),
            }),
            self.tools.clone(),
            self.tools.storage(),
        )
    }

    /// Put one distilled fork brief on the main Agent's queue.
    ///
    /// This is the *only* way a non-owner message reaches the main session
    /// after ADR-0015, and it deliberately reuses the Owner queued-turn path
    /// (`enqueue` → the pump's `queued_turn` drain) rather than inventing a
    /// second one. The brief is marked so both the main prompt and the client
    /// can tell a fork brief from something the Owner said: the text opens
    /// with [`FORK_BRIEF_MARKER`], and the enqueue's source key names the
    /// triggering message.
    pub(super) async fn enqueue_fork_brief(
        &self,
        message: &crate::fork_wake::WakeMessage,
        brief: &str,
    ) -> anyhow::Result<()> {
        let text = format!(
            "{FORK_BRIEF_MARKER} (triage fork, source: {})\n\n{brief}",
            message.source.label()
        );
        let client_id = format!("fork-brief:{}", message.key);
        anyhow::ensure!(
            message.thread_id == self.thread_id,
            "wake destination mismatch"
        );
        let request = OwnerTurn {
            history_id: self.history_id.clone(),
            turn_id: None,
            thread_id: self.thread_id,
            thread_action: None,
            message_id: None,
            report_triggered: false,
            client_id: client_id.clone(),
            body: text,
            anchor: None,
            attachments: Vec::new(),

            mode: SendMode::NextTurn,
        };
        let payload = serde_json::to_value(request)?;
        let turn = self
            .tools
            .storage()
            .queue_background_thread_request(&client_id, self.thread_id, &payload)
            .await?;
        self.publish_background_acceptance(&client_id, turn).await?;
        self.notify.notify_one();
        Ok(())
    }

    pub(super) async fn handle_turn_error(&self, error: lash::EmbedError) {
        tracing::warn!(%error, "Lash queued turn failed");
        let active = self.anchors.lock().await.active.clone();
        let thread_id = self.thread_id;
        let turn_id = active.as_ref().and_then(|a| a.thread_turn_id);
        match self
            .tools
            .storage()
            .append_thread_activity(
                thread_id,
                turn_id,
                "turn_error",
                &json!({"message":error.to_string()}),
            )
            .await
        {
            Ok(activity) => self.tools.publish_thread_activity(activity).await,
            Err(storage_error) => {
                tracing::warn!(%storage_error,"failed to record Thread turn error")
            }
        }
        if let Some(turn_id) = turn_id {
            publish(
                &self.broadcast_log,
                &self.broadcaster,
                HostToClient::AgentActivity {
                    turn_id,
                    thread_id,
                    state: AgentActivityState::Idle,
                    text: None,
                },
            );
        }
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
        Arc::new(lash::runtime::NativeRuntimeEffectController::default()),
        lash_core::ExecutionScope::runtime_operation(scope_id.into()),
    )
    .expect("inline timer trigger occurrence execution scope")
}

/// The Escalate exit, bound to the live main session.
///
/// This is the only edge from fork-land back into the resident Agent, which is
/// why it is a single small type rather than a general capability handed to the
/// fork's tools.
pub(super) struct MainSessionBriefSink {
    runtime: std::sync::Weak<LashAgentRuntime>,
}

#[async_trait]
impl crate::fork_wake::BriefSink for MainSessionBriefSink {
    async fn inject(
        &self,
        message: &crate::fork_wake::WakeMessage,
        brief: &str,
    ) -> anyhow::Result<()> {
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("the main-agent runtime is gone"))?;
        runtime.enqueue_fork_brief(message, brief).await
    }
}
