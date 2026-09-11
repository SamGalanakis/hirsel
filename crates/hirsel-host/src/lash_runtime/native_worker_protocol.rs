//! Standard protocol configuration for native workers with no extra tools.

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use lash::{
    plugins::{
        PluginError, PluginFactory, PluginRegistrar, PluginSessionContext, ProtocolBuildInput,
        ProtocolDriverPlugin, ProtocolSessionContext, ProtocolSessionPlugin, SessionPlugin,
        TurnDriverConfig, TurnDriverPreamble,
    },
    provider::ProviderHandle,
    tools::ToolProvider,
};

use crate::native_coding_tools::NativeCodingTools;

const NATIVE_WORKER_TURN_BUDGET: usize = 32;
const NATIVE_STANDARD_PROTOCOL_PLUGIN_ID: &str = "hirsel_native_standard_protocol";
const NATIVE_STANDARD_EXECUTION_SECTION: &str = r#"Use direct tool calls.

- Use only the tools exposed in this session.
- Serialize calls when later arguments depend on earlier results.
- For direct conversational requests that need no tools, respond in prose only."#;

#[derive(Default)]
struct NativeStandardProtocolPluginFactory;

impl PluginFactory for NativeStandardProtocolPluginFactory {
    fn id(&self) -> &'static str {
        NATIVE_STANDARD_PROTOCOL_PLUGIN_ID
    }

    fn build(&self, _ctx: &PluginSessionContext) -> Result<Arc<dyn SessionPlugin>, PluginError> {
        Ok(Arc::new(NativeStandardProtocolPlugin))
    }
}

struct NativeStandardProtocolPlugin;

impl SessionPlugin for NativeStandardProtocolPlugin {
    fn id(&self) -> &'static str {
        NATIVE_STANDARD_PROTOCOL_PLUGIN_ID
    }

    fn register(&self, reg: &mut PluginRegistrar) -> Result<(), PluginError> {
        reg.protocol()
            .session(Arc::new(NativeStandardProtocolSession))?;
        reg.protocol()
            .protocol_driver(Arc::new(NativeStandardProtocolDriver))?;
        Ok(())
    }
}

struct NativeStandardProtocolSession;

#[async_trait]
impl ProtocolSessionPlugin for NativeStandardProtocolSession {
    async fn initialize_session(
        &self,
        _ctx: ProtocolSessionContext<'_>,
    ) -> Result<(), lash::SessionError> {
        Ok(())
    }
}

struct NativeStandardProtocolDriver;

impl ProtocolDriverPlugin for NativeStandardProtocolDriver {
    fn build_preamble(&self, input: ProtocolBuildInput) -> TurnDriverPreamble {
        let tool_names = input.tool_catalog.tool_names();
        let tool_names_fingerprint = input.tool_catalog.tool_names_fingerprint();
        TurnDriverPreamble {
            config: TurnDriverConfig::chat(
                Arc::new(lash_protocol_standard::StandardDriver),
                true,
                Arc::new(native_turn_limit_exhausted_message),
            ),
            tool_specs: input.tool_catalog.model_tool_specs(),
            tool_names,
            tool_names_fingerprint,
            execution_prompt: Arc::from(NATIVE_STANDARD_EXECUTION_SECTION),
            prompt_contributions: input.extra_prompt_contributions,
        }
    }
}

fn native_turn_limit_exhausted_message(
    message_id: String,
    max_turns: usize,
) -> lash::messages::Message {
    lash::messages::Message {
        id: message_id.clone(),
        role: lash::messages::MessageRole::System,
        parts: lash_core::facade_support::shared_parts(vec![lash::messages::Part::error(
            format!("{message_id}.p0"),
            format!("Turn limit reached ({max_turns}) before a final assistant response."),
        )]),
        origin: None,
    }
}

pub(super) async fn build_native_worker_core(
    lash_dir: &Path,
    provider: ProviderHandle,
    model: lash::ModelSpec,
    coding_tools: Arc<NativeCodingTools>,
    lease_owner: lash_core::LeaseOwnerIdentity,
) -> anyhow::Result<lash::LashCore> {
    tokio::fs::create_dir_all(lash_dir).await?;
    let store_factory = Arc::new(lash_sqlite_store::SqliteSessionStoreFactory::new(
        lash_dir.join("sessions"),
    ));
    let process_env_store =
        Arc::new(lash_sqlite_store::Store::open(&lash_dir.join("process-env.db")).await?);
    let core = lash::LashCore::builder(lash::TurnBudget::bounded(NATIVE_WORKER_TURN_BUDGET))
        .protocol_plugin(Arc::new(NativeStandardProtocolPluginFactory))
        .plugins(lash::plugins::runtime_plugin_stack())
        .provider(provider)
        .model(model)
        .store_factory(store_factory)
        .attachment_store(Arc::new(lash::persistence::FileAttachmentStore::new(
            lash_dir.join("attachments"),
        )))
        .process_env_store(process_env_store)
        .effect_host(Arc::new(lash::durability::NativeEffectHost::default()))
        .tools(coding_tools as Arc<dyn ToolProvider>)
        .without_queued_work()
        .commit_budget(lash::CommitBudget::bounded(1024 * 1024, 512))
        .queued_work_batching(lash::QueuedWorkBatchingConfig::new(1))
        .build(lease_owner)?;
    Ok(core)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    use lash::{TurnInput, provider::ReasoningSelection, tools::ToolStateFacadeOps};
    use lash_core::{LlmOutputPart, llm::types::LlmResponse};

    use super::*;
    use crate::lash_runtime::native_worker::ensure_native_tool_surface;

    #[tokio::test]
    async fn opened_native_worker_session_has_exact_tools_and_drives_standard_read() {
        let dir = tempfile::tempdir().unwrap();
        let fixture_dir = dir.path().join("fixture");
        std::fs::create_dir(&fixture_dir).unwrap();
        std::fs::write(fixture_dir.join("probe.txt"), "opened-session-probe\n").unwrap();
        let calls = Arc::new(AtomicU64::new(0));
        let provider = lash_core::testing::TestProvider::builder()
            .kind("hirsel-native-opened-session")
            .complete({
                let calls = Arc::clone(&calls);
                move |request| {
                    let ordinal = calls.fetch_add(1, Ordering::SeqCst);
                    let names = request
                        .tools
                        .iter()
                        .map(|tool| tool.name.clone())
                        .collect::<Vec<_>>();
                    ensure_native_tool_surface(&names).unwrap();
                    let request_json = serde_json::to_string(&request).unwrap();
                    assert!(
                        !request_json.contains("batch"),
                        "native standard prompt or catalog exposed batch: {request_json}"
                    );
                    async move {
                        Ok(if ordinal == 0 {
                            LlmResponse {
                                parts: vec![LlmOutputPart::ToolCall {
                                    call_id: "read-probe".into(),
                                    tool_name: "read".into(),
                                    input_json: r#"{"path":"probe.txt"}"#.into(),
                                    replay: None,
                                }],
                                ..LlmResponse::default()
                            }
                        } else {
                            assert!(request_json.contains("opened-session-probe"));
                            LlmResponse {
                                parts: vec![LlmOutputPart::Text {
                                    text: "native read completed".into(),
                                    response_meta: None,
                                }],
                                ..LlmResponse::default()
                            }
                        })
                    }
                }
            })
            .build()
            .into_handle();
        let model = lash::ModelSpec::builder("native-opened-session-model")
            .variant(ReasoningSelection::ProviderDefault)
            .context_window_tokens(200_000)
            .build()
            .unwrap();
        let coding_tools = Arc::new(NativeCodingTools::new(fixture_dir).unwrap());
        let core = build_native_worker_core(
            &dir.path().join("runtime"),
            provider,
            model,
            Arc::clone(&coding_tools),
            lash_core::LeaseOwnerIdentity::opaque("hirsel-native-opened-session-test", "one"),
        )
        .await
        .unwrap();
        let session = core.session("native-opened-session").open().await.unwrap();

        let names = session
            .observe()
            .active_tool_manifests()
            .into_iter()
            .map(|manifest| manifest.name)
            .collect::<Vec<_>>();
        ensure_native_tool_surface(&names).unwrap();
        let tool_state = session.admin().tools().state().await.unwrap();
        assert!(
            !tool_state.contains(&lash_core::ToolId::from("tool:batch")),
            "batch remained invocable despite being absent from the model catalog"
        );

        let output = session
            .turn(TurnInput::text("Read probe.txt, then report completion."))
            .run()
            .await
            .unwrap();
        assert_eq!(output.assistant_message(), Some("native read completed"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        coding_tools.shutdown().await;
    }
}
