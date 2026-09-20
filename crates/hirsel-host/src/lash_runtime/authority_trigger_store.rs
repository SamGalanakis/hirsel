use super::*;

#[derive(Clone)]
pub(super) struct AuthorityTriggerStore {
    inner: Arc<dyn TriggerStore>,
    storage: crate::storage::Storage,
    session_id: String,
    anchors: Arc<Mutex<TurnAnchorState>>,
}

enum AuthorityMutation {
    Origin { actor_key: String },
    Carry { previous_revision: u64 },
    None,
}

impl AuthorityTriggerStore {
    pub(super) fn new(
        inner: Arc<dyn TriggerStore>,
        storage: crate::storage::Storage,
        session_id: String,
        anchors: Arc<Mutex<TurnAnchorState>>,
    ) -> Self {
        Self {
            inner,
            storage,
            session_id,
            anchors,
        }
    }

    fn mutation(command: &lash_core::TriggerCommand) -> AuthorityMutation {
        use lash_core::TriggerCommand;
        match command {
            TriggerCommand::Register { actor, .. }
            | TriggerCommand::Update { actor, .. }
            | TriggerCommand::Revive { actor, .. } => AuthorityMutation::Origin {
                actor_key: serde_json::to_string(actor)
                    .expect("ProcessOriginator serialization is infallible"),
            },
            TriggerCommand::Enable {
                expected_revision, ..
            }
            | TriggerCommand::Disable {
                expected_revision, ..
            }
            | TriggerCommand::Delete {
                expected_revision, ..
            } => AuthorityMutation::Carry {
                previous_revision: *expected_revision,
            },
            TriggerCommand::List { .. } | TriggerCommand::Prune { .. } => AuthorityMutation::None,
        }
    }

    fn store_error(error: anyhow::Error) -> lash::plugins::PluginError {
        lash::plugins::PluginError::Session(error.to_string())
    }
}

#[async_trait]
impl TriggerStore for AuthorityTriggerStore {
    async fn execute_command(
        &self,
        operation_id: &str,
        command: lash_core::TriggerCommand,
    ) -> Result<lash_core::TriggerEffectResult, lash::plugins::PluginError> {
        let mutation = Self::mutation(&command);
        if let AuthorityMutation::Origin { actor_key } = &mutation
            && let Some(turn_id) = self
                .anchors
                .lock()
                .await
                .active
                .as_ref()
                .map(|anchor| anchor.thread_turn_id)
        {
            // This marker is committed before Lash can finish the effect and
            // admit a later turn. A process-created trigger reuses the actor
            // stamped on its originating registration, so INSERT OR IGNORE
            // preserves the original turn instead of the current wake turn.
            self.storage
                .bind_trigger_operation_authority(&self.session_id, actor_key, turn_id)
                .await
                .map_err(Self::store_error)?;
        }

        let outcome = self.inner.execute_command(operation_id, command).await?;
        if let Ok(lash_core::TriggerCommandOutcome::Mutation { receipt }) = &outcome {
            match (&mutation, receipt.disposition) {
                (
                    AuthorityMutation::Origin { actor_key },
                    lash_core::TriggerMutationOutcome::Created
                    | lash_core::TriggerMutationOutcome::Updated
                    | lash_core::TriggerMutationOutcome::Revived,
                ) => {
                    self.storage
                        .bind_trigger_authority(
                            &self.session_id,
                            &receipt.subscription_id,
                            &receipt.incarnation,
                            receipt.revision,
                            actor_key,
                        )
                        .await
                        .map_err(Self::store_error)?;
                }
                (AuthorityMutation::Carry { previous_revision }, _) => {
                    self.storage
                        .carry_trigger_authority(
                            &self.session_id,
                            &receipt.subscription_id,
                            &receipt.incarnation,
                            *previous_revision,
                            receipt.revision,
                        )
                        .await
                        .map_err(Self::store_error)?;
                }
                _ => {}
            }
        }
        Ok(outcome)
    }

    async fn list_subscriptions(
        &self,
        filter: lash_core::TriggerSubscriptionFilter,
    ) -> Result<Vec<lash_core::TriggerSubscriptionRecord>, lash::plugins::PluginError> {
        self.inner.list_subscriptions(filter).await
    }

    async fn delete_session_subscriptions(
        &self,
        session_id: &str,
    ) -> Result<usize, lash::plugins::PluginError> {
        self.inner.delete_session_subscriptions(session_id).await
    }

    async fn ingest_occurrence(
        &self,
        request: lash_core::TriggerOccurrenceRequest,
    ) -> Result<lash_core::TriggerIngressReceipt, lash::plugins::PluginError> {
        self.inner.ingest_occurrence(request).await
    }

    async fn list_occurrences(
        &self,
        filter: lash_core::TriggerOccurrenceFilter,
    ) -> Result<Vec<lash_core::TriggerOccurrenceRecord>, lash::plugins::PluginError> {
        self.inner.list_occurrences(filter).await
    }

    async fn list_deliveries_by_occurrence_id(
        &self,
        occurrence_id: &str,
    ) -> Result<Vec<lash_core::TriggerDeliveryReservation>, lash::plugins::PluginError> {
        self.inner
            .list_deliveries_by_occurrence_id(occurrence_id)
            .await
    }

    async fn list_deliveries_by_subscription_id(
        &self,
        subscription_id: &str,
    ) -> Result<Vec<lash_core::TriggerDeliveryReservation>, lash::plugins::PluginError> {
        self.inner
            .list_deliveries_by_subscription_id(subscription_id)
            .await
    }

    async fn list_deliveries_by_process_id(
        &self,
        process_id: &str,
    ) -> Result<Vec<lash_core::TriggerDeliveryReservation>, lash::plugins::PluginError> {
        self.inner.list_deliveries_by_process_id(process_id).await
    }

    async fn list_deliveries(
        &self,
    ) -> Result<Vec<lash_core::TriggerDeliveryReservation>, lash::plugins::PluginError> {
        self.inner.list_deliveries().await
    }

    async fn list_delivery_process_ids(&self) -> Result<Vec<String>, lash::plugins::PluginError> {
        self.inner.list_delivery_process_ids().await
    }

    async fn list_delivery_retention_candidates(
        &self,
    ) -> Result<Vec<lash_core::TriggerDeliveryRetentionCandidate>, lash::plugins::PluginError> {
        self.inner.list_delivery_retention_candidates().await
    }

    async fn list_session_owner_ids_for_retention(
        &self,
    ) -> Result<Vec<String>, lash::plugins::PluginError> {
        self.inner.list_session_owner_ids_for_retention().await
    }

    async fn reconcile_trigger_retention(
        &self,
        candidates: &[lash_core::TriggerDeliveryRetentionCandidate],
        deleted_session_ids: &[String],
    ) -> Result<lash_core::TriggerRetentionReconciliationReport, lash::plugins::PluginError> {
        self.inner
            .reconcile_trigger_retention(candidates, deleted_session_ids)
            .await
    }

    async fn delete_delivery_retention_candidates(
        &self,
        candidates: &[lash_core::TriggerDeliveryRetentionCandidate],
    ) -> Result<usize, lash::plugins::PluginError> {
        self.inner
            .delete_delivery_retention_candidates(candidates)
            .await
    }

    async fn reclaim_trigger_occurrences(
        &self,
        cutoff_epoch_ms: u64,
    ) -> lash_core::TriggerOccurrenceReclamationResult {
        self.inner
            .reclaim_trigger_occurrences(cutoff_epoch_ms)
            .await
    }

    async fn prune_mutation_receipts(
        &self,
        cutoff_epoch_ms: u64,
    ) -> Result<usize, lash::plugins::PluginError> {
        self.inner.prune_mutation_receipts(cutoff_epoch_ms).await
    }
}
