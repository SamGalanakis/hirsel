//! Dedicated in-process RLM runtime for native workers' narrow coding tools.

use std::{path::Path, sync::Arc};

use lash::{provider::ProviderHandle, tools::ToolProvider};
use lash_core::TriggerStore;

use super::{HirselRlmSession, hirsel_rlm_config};
use crate::native_coding_tools::NativeCodingTools;

const NATIVE_WORKER_TURN_BUDGET: usize = 32;

pub(super) async fn build_native_worker_core(
    lash_dir: &Path,
    provider: ProviderHandle,
    model: lash::ModelSpec,
    coding_tools: Arc<NativeCodingTools>,
    lease_owner: lash_core::LeaseOwnerIdentity,
) -> anyhow::Result<lash::LashCore> {
    tokio::fs::create_dir_all(lash_dir).await?;
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
    let trigger_store =
        Arc::new(lash_sqlite_store::SqliteTriggerStore::open(&lash_dir.join("triggers.db")).await?)
            as Arc<dyn TriggerStore>;
    let process_registry = Arc::new(
        lash_sqlite_store::SqliteProcessRegistry::open(
            &lash_dir.join("processes.db"),
            lash_dir.join("sessions"),
        )
        .await?,
    ) as Arc<dyn lash::process::ProcessRegistry>;
    let protocol = lash_protocol_rlm::RlmProtocolPluginFactory::new(
        hirsel_rlm_config(HirselRlmSession::NativeWorker),
        artifact_store,
    );
    let core = lash::LashCore::rlm_builder(
        lash::TurnBudget::bounded(NATIVE_WORKER_TURN_BUDGET),
        protocol,
    )
    .provider(provider)
    .model(model)
    .store_factory(store_factory)
    .attachment_store(Arc::new(lash::persistence::FileAttachmentStore::new(
        lash_dir.join("attachments"),
    )))
    .process_env_store(process_env_store)
    .effect_host(Arc::new(lash::durability::NativeEffectHost::default()))
    .process_registry(process_registry)
    .trigger_store(trigger_store)
    .tools(coding_tools as Arc<dyn ToolProvider>)
    .without_queued_work()
    .commit_budget(lash::CommitBudget::bounded(1024 * 1024, 512))
    .queued_work_batching(lash::QueuedWorkBatchingConfig::new(1))
    .build(lease_owner)?;
    Ok(core)
}
