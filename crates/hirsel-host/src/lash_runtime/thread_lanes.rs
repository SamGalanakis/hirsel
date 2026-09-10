//! Lazy independent Thread sessions. A durable FIFO feeds each lane; capacity is shared.
use super::*;
use tokio::sync::{OnceCell, Semaphore};

pub(super) struct ThreadRuntimeRegistry {
    monitors_started: Mutex<HashSet<String>>,
    cli: Arc<Mutex<HashMap<u64, Arc<CliTurn>>>>,
    admission: Mutex<()>,
    epoch: std::sync::RwLock<(String, RuntimeTasks)>,
    config: RuntimeConfig,
    model_selection: Option<ModelSelectionState>,
    tools: ToolSuite,
    broadcaster: broadcast::Sender<HostToClient>,
    broadcast_log: BroadcastLog,
    lanes: Mutex<HashMap<u64, Arc<OnceCell<Arc<AgentBackend>>>>>,
    pub(super) capacity: Arc<Semaphore>,
}
impl ThreadRuntimeRegistry {
    pub(super) fn is_scripted(&self) -> bool {
        matches!(self.config.agent_mode, AgentMode::Scripted)
    }
    pub(super) fn start(
        history_id: String,
        config: RuntimeConfig,
        model_selection: Option<ModelSelectionState>,
        tools: ToolSuite,
        broadcaster: broadcast::Sender<HostToClient>,
        broadcast_log: BroadcastLog,
    ) -> Arc<Self> {
        Arc::new(Self {
            monitors_started: Mutex::new(HashSet::new()),
            cli: Arc::new(Mutex::new(HashMap::new())),
            admission: Mutex::new(()),
            epoch: std::sync::RwLock::new((history_id, RuntimeTasks::new())),
            config,
            model_selection,
            tools,
            broadcaster,
            broadcast_log,
            lanes: Mutex::new(HashMap::new()),
            capacity: Arc::new(Semaphore::new(4)),
        })
    }
    pub(super) fn spawn_poller(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            loop {
                tick.tick().await;
                let Some(registry) = weak.upgrade() else {
                    break;
                };
                if let Err(error) = registry.pump_pending().await {
                    tracing::warn!(%error,"Thread admission poll failed");
                }
            }
        });
    }
    pub(super) async fn refresh_execution_default(&self) -> anyhow::Result<()> {
        let model = match &self.model_selection {
            Some(selection) => selection.model_spec()?,
            None => lash::ModelSpec::builder(self.config.model.clone())
                .variant(ReasoningSelection::ProviderDefault)
                .context_window_tokens(200_000)
                .build()
                .map_err(|e| anyhow::anyhow!("invalid model metadata: {e}"))?,
        };
        self.tools
            .storage()
            .set_host_execution_default(&crate::storage::ThreadExecution::Host {
                provider_id: self.config.boot_plan.label().into(),
                model,
            })
            .await
    }
    pub(super) async fn lane(&self, id: u64) -> anyhow::Result<Arc<AgentBackend>> {
        anyhow::ensure!(
            self.tools.storage().thread(id).await?.is_some(),
            "Thread is unavailable"
        );
        let (history_id, tasks) = self.epoch.read().expect("runtime epoch poisoned").clone();
        let cell = self
            .lanes
            .lock()
            .await
            .entry(id)
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone();
        let lane = cell
            .get_or_try_init(|| async {
                let lane = match self.config.agent_mode {
                    AgentMode::Scripted => AgentBackend::Scripted(start_scripted_runtime(
                        self.config.clone(),
                        self.tools.clone(),
                        self.broadcaster.clone(),
                        self.broadcast_log.clone(),
                        id,
                        tasks.clone(),
                        self.capacity.clone(),
                    )),
                    AgentMode::Lash => match LashAgentRuntime::start(
                        self.config.clone(),
                        self.model_selection.clone(),
                        self.tools.clone(),
                        self.broadcaster.clone(),
                        self.broadcast_log.clone(),
                        id,
                        history_id,
                        tasks,
                        self.capacity.clone(),
                    )
                    .await?
                    {
                        LashStartup::Ready(runtime) => AgentBackend::Lash(runtime),
                        LashStartup::Unavailable(runtime) => AgentBackend::Degraded(runtime),
                    },
                };
                Ok::<_, anyhow::Error>(Arc::new(lane))
            })
            .await?;
        Ok(lane.clone())
    }
    async fn pump_pending(&self) -> anyhow::Result<()> {
        let _admission = self.admission.lock().await;
        for turn in self
            .tools
            .storage()
            .requested_thread_cancellations()
            .await?
        {
            if turn.state == hirsel_proto::ThreadTurnState::Queued {
                if let Some(work) = self.cli.lock().await.get(&turn.thread_id)
                    && work.turn_id == turn.id
                {
                    work.cancel.cancel();
                }
                let history = self.epoch.read().expect("runtime epoch poisoned").0.clone();
                let (turn, _) = self
                    .tools
                    .storage()
                    .complete_thread_turn(
                        &history,
                        turn.id,
                        hirsel_proto::ThreadTurnState::Cancelled,
                        None,
                    )
                    .await?;
                self.tools.publish_thread_turn(turn).await;
            } else if let Some(work) = self.cli.lock().await.get(&turn.thread_id) {
                work.cancel.cancel();
            } else {
                match self.lane(turn.thread_id).await?.as_ref() {
                    AgentBackend::Lash(runtime) => {
                        runtime.cancel_owned_turn(Some(turn.thread_id)).await?
                    }
                    AgentBackend::Scripted(runtime) => runtime.cancel_turn().await?,
                    _ => {}
                }
            }
        }
        for record in self.tools.active_monitors().await? {
            self.start_monitor_inner(&record).await?;
        }
        let requests = self.tools.storage().pending_thread_requests().await?;
        let mut seen = HashSet::new();
        for (_, payload) in requests {
            let request: OwnerTurn = serde_json::from_value(payload)?;
            let id = request.thread_id;
            if !seen.insert(id) || self.cli.lock().await.contains_key(&id) {
                continue;
            }
            let turn = request.stored_turn(&self.tools.storage()).await?;
            let execution = self.tools.storage().turn_execution(turn.id).await?;
            if let crate::storage::ThreadExecution::Cli { agent, .. } = &execution {
                if turn.state != hirsel_proto::ThreadTurnState::Queued {
                    continue;
                }
                let work = CliTurn::new(turn.id, self.tools.driver_for(*agent));
                self.cli.lock().await.insert(id, work.clone());
                let active = self.cli.clone();
                let tools = self.tools.clone();
                let capacity = self.capacity.clone();
                self.epoch.read().expect("runtime epoch poisoned").1.spawn(async move {
                    if let Err(error)=work.run(&tools,request,execution,capacity).await {tracing::error!(thread_id=id,%error,"CLI Thread execution could not be projected");}
                    active.lock().await.remove(&id);
                });
                continue;
            }
            match self.lane(id).await {
                Ok(lane) => match lane.as_ref() {
                    AgentBackend::Lash(r) => r.notify.notify_one(),
                    AgentBackend::Scripted(r) => {
                        r.recover_pending().await?;
                        r.notify.notify_one();
                    }
                    AgentBackend::Degraded(_) => {
                        let (turn, _) = self
                            .tools
                            .storage()
                            .complete_thread_turn(
                                &request.history_id,
                                turn.id,
                                hirsel_proto::ThreadTurnState::Failed,
                                None,
                            )
                            .await?;
                        self.tools.publish_thread_turn(turn).await;
                    }
                    AgentBackend::Threaded(_) => unreachable!(),
                },
                Err(error) => {
                    tracing::warn!(thread_id=id,%error,"Thread lane failed to initialize");
                    let (turn, _) = self
                        .tools
                        .storage()
                        .complete_thread_turn(
                            &request.history_id,
                            turn.id,
                            hirsel_proto::ThreadTurnState::Failed,
                            None,
                        )
                        .await?;
                    self.tools.publish_thread_turn(turn).await;
                }
            }
        }
        Ok(())
    }
    pub(super) async fn reset_history(&self) -> anyhow::Result<()> {
        let _admission = self.admission.lock().await;
        let tasks = self.epoch.read().expect("runtime epoch poisoned").1.clone();
        for lane in self.opened().await {
            if let AgentBackend::Lash(runtime) = lane.as_ref() {
                runtime.fork_wake.stop().await;
            }
        }
        for work in self.cli.lock().await.values() {
            work.stop().await;
        }
        tasks.stop().await;
        self.cli.lock().await.clear();
        // Quiesce provider execution too; its callbacks are already detached from
        // all host observation/pump tasks and the old binding is revoked below.
        for lane in self.opened().await {
            match lane.as_ref() {
                AgentBackend::Lash(runtime) => {
                    let _ = runtime.cancel_owned_turn(Some(runtime.thread_id)).await;
                }
                AgentBackend::Scripted(runtime) => {
                    runtime.cancel_turn().await?;
                }
                _ => {}
            }
        }
        self.tools.storage().reset().await?;
        self.tools.reset_runtime_projections().await;
        self.lanes.lock().await.clear();
        self.monitors_started.lock().await.clear();
        let history_id = self.tools.storage().history_id().await?;
        *self.epoch.write().expect("runtime epoch poisoned") = (history_id, RuntimeTasks::new());
        self.refresh_execution_default().await?;
        Ok(())
    }
    pub(super) async fn start_monitor(&self, record: &MonitorRecord) -> anyhow::Result<()> {
        let _admission = self.admission.lock().await;
        self.start_monitor_inner(record).await
    }
    async fn start_monitor_inner(&self, record: &MonitorRecord) -> anyhow::Result<()> {
        if self.monitors_started.lock().await.contains(&record.id) {
            return Ok(());
        }
        match self.lane(record.thread_id).await?.as_ref() {
            AgentBackend::Lash(runtime) => runtime.start_monitor_process(record).await?,
            AgentBackend::Scripted(runtime) => runtime.spawn_standalone_monitor(record.id.clone()),
            _ => return Ok(()),
        }
        self.monitors_started.lock().await.insert(record.id.clone());
        Ok(())
    }
    pub(super) async fn dispatch_fork_wake(
        &self,
        message: crate::fork_wake::WakeMessage,
    ) -> anyhow::Result<bool> {
        let _admission = self.admission.lock().await;
        match self.lane(message.thread_id).await?.as_ref() {
            AgentBackend::Lash(runtime) => Ok(runtime.fork_wake.dispatch(message)),
            _ => Ok(false),
        }
    }
    pub(super) async fn opened(&self) -> Vec<Arc<AgentBackend>> {
        self.lanes
            .lock()
            .await
            .values()
            .filter_map(|c| c.get().cloned())
            .collect()
    }
    pub(super) async fn enqueue(&self, turn: OwnerTurn) -> anyhow::Result<()> {
        let _admission = self.admission.lock().await;
        #[cfg(test)]
        if self.is_scripted() && turn.body == "__hirsel_test_enqueue_error__" {
            anyhow::bail!("scripted enqueue failed for test");
        }
        anyhow::ensure!(
            turn.history_id == self.epoch.read().expect("runtime epoch poisoned").0,
            "request belongs to an old history"
        );
        turn.stored_turn(&self.tools.storage()).await?;
        // SQL acceptance already committed the FIFO input. Polling covers both
        // provider kinds and is also the recovery/wakeup path for child reports.
        Ok(())
    }
    pub(super) async fn cancel(&self, id: u64) -> anyhow::Result<()> {
        let _admission = self.admission.lock().await;
        let history = self.epoch.read().expect("runtime epoch poisoned").0.clone();
        self.tools
            .storage()
            .request_thread_cancellation(&history, id)
            .await?;
        if let Some(work) = self.cli.lock().await.get(&id) {
            work.cancel.cancel();
            return Ok(());
        }
        let lane = self.lane(id).await?;
        match lane.as_ref() {
            AgentBackend::Lash(r) => r.cancel_owned_turn(Some(id)).await,
            AgentBackend::Scripted(r) => r.cancel_turn().await,
            AgentBackend::Degraded(_) => anyhow::bail!("Thread has no running turn"),
            AgentBackend::Threaded(_) => unreachable!(),
        }
    }
    pub(super) async fn cancel_queued(
        &self,
        client_id: &str,
    ) -> anyhow::Result<CancelQueuedResult> {
        let _admission = self.admission.lock().await;
        let Some(payload) = self.tools.storage().thread_request(client_id).await? else {
            return Ok(CancelQueuedResult::AlreadyClaimed);
        };
        let request: OwnerTurn = serde_json::from_value(payload)?;
        let id = request.thread_id;
        let turn = request.stored_turn(&self.tools.storage()).await?;
        if turn.state != hirsel_proto::ThreadTurnState::Queued {
            return Ok(CancelQueuedResult::AlreadyClaimed);
        }
        if let Some(work) = self.cli.lock().await.get(&id)
            && work.turn_id == turn.id
        {
            work.cancel.cancel();
        }
        // CLI and not-yet-opened scripted lanes have no admitted provider input.
        if self.is_scripted()
            || matches!(
                self.tools.storage().turn_execution(turn.id).await?,
                crate::storage::ThreadExecution::Cli { .. }
            )
        {
            for lane in self.opened().await {
                if let AgentBackend::Scripted(runtime) = lane.as_ref() {
                    runtime
                        .state
                        .lock()
                        .await
                        .queue
                        .retain(|queued| queued.client_id != client_id);
                }
            }
            let (turn, _) = self
                .tools
                .storage()
                .complete_thread_turn(
                    &request.history_id,
                    turn.id,
                    hirsel_proto::ThreadTurnState::Cancelled,
                    None,
                )
                .await?;
            self.tools.publish_thread_turn(turn).await;
            return Ok(CancelQueuedResult::Cancelled);
        }
        match self.lane(id).await?.as_ref() {
            AgentBackend::Lash(r) => r.cancel_queued(client_id).await,
            AgentBackend::Scripted(r) => r.cancel_queued(client_id).await,
            AgentBackend::Degraded(r) => r.cancel_queued(client_id).await,
            AgentBackend::Threaded(_) => unreachable!(),
        }
    }
}
