//! Private per-execution MCP transport. This never enters the human owner protocol.
use crate::{lash_runtime::ScopedThreadTools, tools::ToolSuite};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use subtle::ConstantTimeEq;
use tokio::{
    net::{UnixListener, UnixStream},
    sync::Mutex,
};
use tokio_util::{
    codec::{Framed, LinesCodec},
    sync::CancellationToken,
};

const MAX_FRAME: usize = 2 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
struct Invocation {
    capability: String,
    bridge_instance: String,
    invocation_id: String,
    request: Value,
}

pub(crate) struct ThreadToolBridge {
    pub(crate) caller: crate::storage::ThreadCaller,
    pub(crate) socket_path: PathBuf,
    pub(crate) capability_file: PathBuf,
    pub(crate) expected_tools: Vec<String>,
    pub(crate) invalidated: CancellationToken,
    telemetry: Arc<Mutex<ToolTelemetry>>,
    server: Option<tokio::task::JoinHandle<()>>,
    cancel: CancellationToken,
    _directory: tempfile::TempDir,
}
impl Drop for ThreadToolBridge {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl ThreadToolBridge {
    pub(crate) async fn tool_calls(&self) -> Vec<hirsel_proto::ToolCallSummary> {
        self.telemetry.lock().await.completed.clone()
    }
    /// Stop accepting/abort in-flight callbacks, then pair every published
    /// start before the owner persists the terminal assistant projection.
    pub(crate) async fn finish(&mut self) {
        self.cancel.cancel();
        if let Some(server) = self.server.take() {
            let _ = server.await;
        }
    }
    pub(crate) async fn start(
        tools: ToolSuite,
        history_id: &str,
        turn_id: u64,
    ) -> anyhow::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::Builder::new()
            .prefix("hirsel-thread-tools-")
            .tempdir()?;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        let socket_path = directory.path().join("tools.sock");
        let capability_file = directory.path().join("capability");
        let capability = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        std::fs::write(&capability_file, &capability)?;
        std::fs::set_permissions(&capability_file, std::fs::Permissions::from_mode(0o600))?;
        let listener = UnixListener::bind(&socket_path)?;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
        let launch = uuid::Uuid::new_v4().to_string();
        let caller = tools
            .storage()
            .bind_thread_execution(history_id, &launch, &launch, turn_id)
            .await?;
        let catalog = crate::lash_runtime::scoped_mcp_catalog(&tools);
        let expected_tools = catalog
            .iter()
            .filter_map(|v| v["name"].as_str().map(str::to_owned))
            .collect();
        let invalidated = CancellationToken::new();
        let telemetry = Arc::new(Mutex::new(ToolTelemetry::default()));
        let state = Arc::new(BridgeState {
            telemetry: telemetry.clone(),
            invalidated: invalidated.clone(),
            tools,
            caller: caller.clone(),
            launch,
            capability,
            catalog,
            first_invoker: Mutex::new(None),
            receipts: Mutex::new(HashMap::new()),
        });
        let cancel = CancellationToken::new();
        let shutdown = cancel.clone();
        let server = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _=shutdown.cancelled()=>break,
                    Some(_)=connections.join_next()=>{},
                    accepted=listener.accept()=>{
                        let Ok((stream,_))=accepted else{break};let state=state.clone();
                        connections.spawn(async move { let _ = serve_connection(stream,state).await; });
                    }
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
            state.finish_pending().await;
            let _ = state
                .tools
                .storage()
                .revoke_thread_execution(&state.caller)
                .await;
        });
        Ok(Self {
            caller,
            socket_path,
            capability_file,
            expected_tools,
            invalidated,
            telemetry,
            server: Some(server),
            cancel,
            _directory: directory,
        })
    }
}
mod telemetry;
use telemetry::ToolTelemetry;

struct BridgeState {
    telemetry: Arc<Mutex<ToolTelemetry>>,
    invalidated: CancellationToken,
    tools: ToolSuite,
    caller: crate::storage::ThreadCaller,
    launch: String,
    capability: String,
    catalog: Vec<Value>,
    first_invoker: Mutex<Option<String>>,
    receipts: Mutex<HashMap<(String, String), (Value, Value)>>,
}
async fn serve_connection(stream: UnixStream, state: Arc<BridgeState>) -> anyhow::Result<()> {
    let mut frames = Framed::new(stream, LinesCodec::new_with_max_length(MAX_FRAME));
    while let Some(frame) = frames.next().await {
        let invocation: Invocation = serde_json::from_str(&frame?)?;
        anyhow::ensure!(
            bool::from(
                state
                    .capability
                    .as_bytes()
                    .ct_eq(invocation.capability.as_bytes())
            ),
            "invalid execution capability"
        );
        let caller = state
            .tools
            .storage()
            .execution_caller(&state.launch, &state.launch)
            .await?;
        let request = &invocation.request;
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let result = match request["method"].as_str().unwrap_or("") {
            "initialize" => Ok(
                json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"hirsel-thread","version":"1"}}),
            ),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools":state.catalog})),
            "tools/call" => {
                let mut first = state.first_invoker.lock().await;
                if first
                    .as_ref()
                    .is_some_and(|old| old != &invocation.bridge_instance)
                {
                    state
                        .tools
                        .storage()
                        .revoke_thread_execution(&state.caller)
                        .await?;
                    state.invalidated.cancel();
                    anyhow::bail!(
                        "bridge restarted after invocation; execution must be interrupted"
                    );
                }
                *first = Some(invocation.bridge_instance.clone());
                drop(first);
                let key = (
                    invocation.bridge_instance.clone(),
                    invocation.invocation_id.clone(),
                );
                let mut receipts = state.receipts.lock().await;
                if let Some((old, result)) = receipts.get(&key) {
                    let storage = state.tools.storage();
                    let _guard = storage.execution_guard(&caller).await?;
                    anyhow::ensure!(old == request, "invocation payload changed");
                    Ok(result.clone())
                } else {
                    let params = &request["params"];
                    let name = params["name"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("tool name missing"))?;
                    anyhow::ensure!(
                        state.catalog.iter().any(|t| t["name"] == name),
                        "tool not in scoped catalog"
                    );
                    let args = params
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    let event_id = format!("{}:{}", key.0, key.1);
                    state.start_tool(&event_id, name).await?;
                    let facade = ScopedThreadTools {
                        tools: state.tools.clone(),
                        caller: caller.clone(),
                        operation_id: format!("cli:{}:{}:{}", state.launch, key.0, key.1),
                    };
                    let result = match facade.execute(name, &args).await {
                        Ok(value) => {
                            json!({"content":[{"type":"text","text":serde_json::to_string(&value)?}],"isError":false})
                        }
                        Err(error) => {
                            json!({"content":[{"type":"text","text":error}],"isError":true})
                        }
                    };
                    let ok = result["isError"] == false;
                    state.finish_tool(&event_id, ok, None).await;
                    receipts.insert(key, (request.clone(), result.clone()));
                    Ok(result)
                }
            }
            method if method.starts_with("notifications/") => continue,
            _ => Err("unknown MCP method"),
        };
        let response = match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(message) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":message}})
            }
        };
        frames.send(serde_json::to_string(&response)?).await?;
    }
    Ok(())
}

/// Selected by main before auth/config/tracing startup. stdout is protocol only.
pub async fn run_stdio(socket: &Path, cap_file: &Path) -> anyhow::Result<()> {
    let capability = tokio::fs::read_to_string(cap_file).await?;
    let bridge_instance = uuid::Uuid::new_v4().to_string();
    let mut input = tokio_util::codec::FramedRead::new(
        tokio::io::stdin(),
        LinesCodec::new_with_max_length(MAX_FRAME),
    );
    let mut output = tokio_util::codec::FramedWrite::new(
        tokio::io::stdout(),
        LinesCodec::new_with_max_length(MAX_FRAME),
    );
    let mut invocations = HashMap::<String, (Value, String)>::new();
    while let Some(line) = input.next().await {
        let request: Value = serde_json::from_str(&line?)?;
        if request.get("id").is_none() {
            continue;
        }
        let key = serde_json::to_string(&request["id"])?;
        let (old, invocation_id) = invocations
            .entry(key)
            .or_insert_with(|| (request.clone(), uuid::Uuid::new_v4().to_string()));
        anyhow::ensure!(
            *old == request,
            "MCP request ID reused with changed payload"
        );
        let invocation = Invocation {
            capability: capability.clone(),
            bridge_instance: bridge_instance.clone(),
            invocation_id: invocation_id.clone(),
            request,
        };
        let encoded = serde_json::to_string(&invocation)?;
        // A lost IPC reply retries the same host invocation, never a new tool effect.
        let mut response = None;
        for _ in 0..2 {
            let attempt = async {
                let stream = UnixStream::connect(socket).await?;
                let mut frames = Framed::new(stream, LinesCodec::new_with_max_length(MAX_FRAME));
                frames.send(encoded.clone()).await?;
                frames
                    .next()
                    .await
                    .transpose()?
                    .ok_or_else(|| anyhow::anyhow!("Thread bridge closed"))
            };
            if let Ok(Ok(value)) =
                tokio::time::timeout(std::time::Duration::from_secs(180), attempt).await
            {
                response = Some(value);
                break;
            }
        }
        output
            .send(response.ok_or_else(|| anyhow::anyhow!("Thread bridge unavailable"))?)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
