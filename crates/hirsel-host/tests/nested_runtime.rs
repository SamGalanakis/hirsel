//! Real Lash -> real CLI driver -> real host MCP, with loopback-only provider peers.
use axum::{Json, Router, extract::State, routing::post};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Clone)]
struct ProviderFixture {
    requests: Arc<Mutex<Vec<Value>>>,
    cwd: String,
}
async fn complete(
    State(f): State<ProviderFixture>,
    Json(request): Json<Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut requests = f.requests.lock().await;
    let n = requests.len();
    requests.push(request);
    let source = if n == 0 {
        format!(
            r#"<typescript>
await artifacts.edit({{artifact_id:44,edits:[{{old_string:"Original notes",new_string:"Edited through explicit human reference"}}]}});
const child=await threads.delegate({{title:"Offline CLI child",brief:"Read your own scoped context. Shared notes are explicitly supplied.",artifact_ids:[44],agent:"claude",model:"claude-opus-5",variant:"high",cwd:{}}});
finish("Delegated CLI child " + child.thread_id);
</typescript>"#,
            serde_json::to_string(&f.cwd).unwrap()
        )
    } else {
        "<typescript>finish(\"Observed completed CLI child report.\");</typescript>".to_string()
    };
    let parts = [
        json!({"id":format!("proof-{n}"),"model":"proof-model","choices":[{"index":0,"delta":{"role":"assistant","content":source}}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":10,"total_tokens":20}}),
    ];
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        parts[0], parts[1]
    );
    ([("Content-Type", "text/event-stream")], body).into_response()
}
async fn next_frame(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    kind: &str,
) -> Value {
    tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let frame = ws.next().await.unwrap().unwrap();
            if let Message::Text(text) = frame {
                let v: Value = serde_json::from_str(&text).unwrap();
                assert_ne!(v["type"], "error", "{v}");
                if v["type"] == kind {
                    return v;
                }
            }
        }
    })
    .await
    .expect("protocol response deadline")
}
async fn send(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    value: Value,
) {
    ws.send(Message::Text(value.to_string())).await.unwrap();
}

#[tokio::test]
async fn lash_parent_delegates_real_cli_and_receives_durable_report_with_human_refs() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    // Offline fixture state: artifact44 is initially referenced only in unrelated B.
    let storage = hirsel_host::storage::Storage::open(&data).await.unwrap();
    let peer = storage
        .create_thread(
            "peer",
            "Unrelated B",
            "",
            &json!({}),
            hirsel_proto::ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap()
        .0;
    let message = storage
        .append_thread_chat(
            peer.id,
            hirsel_proto::ChatAuthor::Agent,
            "PEER_ONLY_SECRET",
            None,
            vec![],
        )
        .await
        .unwrap();
    drop(storage);
    {
        let c = rusqlite::Connection::open(data.join("hirsel.sqlite")).unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        c.execute("INSERT INTO artifacts(id,title,kind,mime,filename,content,created_at,updated_at) VALUES(44,'Shared notes','\"file\"','text/plain','notes.txt','Original notes',?1,?1)",[now]).unwrap();
        c.execute(
            "INSERT INTO message_artifacts(message_id,artifact_id) VALUES(?1,44)",
            [message.id],
        )
        .unwrap();
    }
    let fixture = ProviderFixture {
        requests: Default::default(),
        cwd: dir.path().to_string_lossy().into_owned(),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_addr = listener.local_addr().unwrap();
    let provider_state = fixture.clone();
    let provider_task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/chat/completions", post(complete))
                .with_state(provider_state),
        )
        .await
        .unwrap();
    });
    let config = dir.path().join("hirsel.toml");
    std::fs::write(
        &config,
        format!(
            r#"[providers.probe]
kind = "openai_compatible"
label = "Offline proof"
base_url = "http://{provider_addr}/v1"
api_key = "offline-no-credential"
default_model = "proof-model"
[model]
provider = "probe"
id = "proof-model"
variant = "default"
[fork]
provider = "probe"
model = "proof-model"
variant = "default"
"#
        ),
    )
    .unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let peer_source =
        std::fs::read_to_string(repo.join("crates/hirsel-drivers/src/claude_fixture.py")).unwrap();
    let peer_source=peer_source.replace("    text = json.dumps(results)","    if os.path.exists('hold'):\n        with open('held', 'w') as f: json.dump({'pid': os.getpid()}, f)\n        while os.path.exists('hold'): time.sleep(0.01)\n    text = json.dumps(results)");
    std::fs::write(bin.join("claude"), peer_source).unwrap();
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let host_addr = reserve.local_addr().unwrap();
    drop(reserve);
    let log_path = dir.path().join("host.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut host = tokio::process::Command::new(env!("CARGO_BIN_EXE_hirsel-host"))
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("LANG", "C.UTF-8")
        .env("HIRSEL_AGENT", "lash")
        .env("HIRSEL_DRIVER", "real")
        .env("HIRSEL_PROVIDER", "openrouter")
        .env("HIRSEL_MODEL", "google/gemini-3.7-flash")
        .env("HIRSEL_TOKEN", "offline-proof")
        .env("HIRSEL_IROH", "0")
        .env("HIRSEL_DEBUG", "1")
        .env("HIRSEL_CONFIG", config)
        .env("HIRSEL_DATA_DIR", &data)
        .env("HIRSEL_LISTEN", host_addr.to_string())
        .env("HIRSEL_TEMPLATES_DIR", repo.join("templates"))
        .env("HIRSEL_DOCS", repo.join("docs/hirsel-config.md"))
        .current_dir(dir.path())
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let http = reqwest::Client::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if http
                .get(format!("http://{host_addr}/livez"))
                .send()
                .await
                .is_ok()
            {
                break;
            }
            assert!(
                host.try_wait().unwrap().is_none(),
                "host exited: {}",
                std::fs::read_to_string(&log_path).unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let (mut ws, _) = connect_async(format!("ws://{host_addr}/ws")).await.unwrap();
    send(
        &mut ws,
        json!({"type":"hello","auth":{"static_token":"offline-proof"}}),
    )
    .await;
    let hello = next_frame(&mut ws, "hello_ok").await;
    assert_eq!(hello["threads"].as_array().unwrap().len(), 1);
    let history_id = hello["history_id"].as_str().unwrap();
    send(&mut ws,json!({"type":"create_thread","kind":"task","history_id":history_id,"client_id":"parent","title":"Real Lash parent","parent_thread_id":null})).await;
    let created = next_frame(&mut ws, "thread_created").await;
    let parent = created["thread"]["id"].as_u64().unwrap();
    send(&mut ws,json!({"type":"send_thread_message","history_id":history_id,"client_id":"owner-proof","thread_id":parent,"body":"Edit the explicitly attached notes and delegate the focused child.","attachments":[],"mentions":[],"artifact_ids":[44],"mode":"send"})).await;
    let db = rusqlite::Connection::open_with_flags(
        data.join("hirsel.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let turns: u64 = db
                .query_row(
                    "SELECT COUNT(*) FROM thread_turns WHERE state='completed'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let pending: u64 = db
                .query_row("SELECT COUNT(*) FROM thread_requests", [], |r| r.get(0))
                .unwrap();
            if turns >= 3 && pending == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await;
    let captured = fixture.requests.lock().await.clone();
    assert!(
        completed.is_ok(),
        "runtime did not complete: {}\nprovider requests: {}",
        std::fs::read_to_string(&log_path).unwrap(),
        serde_json::to_string(&captured).unwrap()
    );
    assert_eq!(
        captured.len(),
        2,
        "unexpected repair or retry: {captured:?}"
    );
    for request in &captured {
        assert_eq!(request["model"], "proof-model");
        assert!(!request.to_string().contains("PEER_ONLY_SECRET"));
    }
    assert!(
        captured[0]
            .to_string()
            .contains("Current accepted message artifact references")
    );
    assert!(captured[1].to_string().contains("report (completed)"));
    let child: u64 = db
        .query_row(
            "SELECT id FROM threads WHERE parent_thread_id=?1",
            [parent],
            |r| r.get(0),
        )
        .unwrap();
    let child_turn: (u64, u64, u64) = db
        .query_row(
            "SELECT id,requester_thread_id,requester_turn_id FROM thread_turns WHERE thread_id=?1",
            [child],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(child_turn.1, parent);
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM thread_reports WHERE child_turn_id=?1",
            [child_turn.0],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM thread_turns WHERE state!='completed'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT content FROM artifacts WHERE id=44", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "Edited through explicit human reference"
    );
    assert_eq!(db.query_row("SELECT COUNT(*) FROM message_artifacts r JOIN client_messages c ON c.msg_id=r.message_id WHERE c.client_id='owner-proof' AND r.artifact_id=44",[],|r|r.get::<_,u64>(0)).unwrap(),1);
    let child_body:String=db.query_row("SELECT body FROM chat_messages WHERE thread_id=?1 AND author='agent' ORDER BY id DESC LIMIT 1",[child],|r|r.get(0)).unwrap();
    let results: Value = serde_json::from_str(&child_body).unwrap();
    assert_eq!(results[0]["isError"], false);
    let context: Value =
        serde_json::from_str(results[0]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(context["self"]["id"], child);
    assert_eq!(context["brief"]["artifact_ids"], json!([44]));
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM chat_messages WHERE thread_id=?1",
            [peer.id],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        1
    );
    let summaries:String=db.query_row("SELECT tool_calls FROM chat_messages WHERE thread_id=?1 AND author='agent' ORDER BY id DESC LIMIT 1",[child],|r|r.get(0)).unwrap();
    let summaries = serde_json::from_str::<Value>(&summaries).unwrap();
    assert_eq!(summaries[0]["name"], "threads_context");
    assert_eq!(summaries[0]["ok"], true);
    assert!(summaries[0]["id"].as_str().is_some_and(|id| !id.is_empty()));

    // Retain an actual running CLI callback across debug reset, then recreate IDs.
    std::fs::write(dir.path().join("hold"), b"hold this owned fixture").unwrap();
    send(&mut ws,json!({"type":"send_thread_message","history_id":history_id,"client_id":"child-held","thread_id":child,"body":"Hold this callback for reset proof.","attachments":[],"mentions":[],"artifact_ids":[],"mode":"send"})).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !dir.path().join("held").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let held: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("held")).unwrap()).unwrap();
    let held_pid = held["pid"].as_u64().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM thread_turns WHERE state='running' AND thread_id=?1",
            [child],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        1
    );
    let old_history: String = db
        .query_row("SELECT value FROM meta WHERE key='history_id'", [], |r| {
            r.get(0)
        })
        .unwrap();
    http.post(format!("http://{host_addr}/debug/reset"))
        .bearer_auth("offline-proof")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let new_history: String = db
        .query_row("SELECT value FROM meta WHERE key='history_id'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_ne!(old_history, new_history);
    assert!(
        !Path::new(&format!("/proc/{held_pid}")).exists(),
        "reset must await retirement of its actual CLI child"
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM thread_requests", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM chat_messages", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    std::fs::remove_file(dir.path().join("hold")).unwrap();
    for (client, title) in [("new-peer", "New peer"), ("new-parent", "Fresh parent")] {
        send(&mut ws,json!({"type":"create_thread","kind":"task","history_id":new_history,"client_id":client,"title":title,"parent_thread_id":null})).await;
        let fresh = next_frame(&mut ws, "thread_created").await;
        if client == "new-parent" {
            assert_eq!(fresh["thread"]["id"], parent);
        }
    }
    send(&mut ws,json!({"type":"send_thread_message","history_id":new_history,"client_id":"fresh-input","thread_id":parent,"body":"NEW_HISTORY_ONLY","attachments":[],"mentions":[],"artifact_ids":[],"mode":"send"})).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if db
                .query_row(
                    "SELECT COUNT(*) FROM thread_turns WHERE state='completed'",
                    [],
                    |r| r.get::<_, u64>(0),
                )
                .unwrap()
                == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let requests = fixture.requests.lock().await;
    assert_eq!(requests.len(), 3);
    let fresh = requests[2].to_string();
    assert!(fresh.contains("NEW_HISTORY_ONLY"));
    assert!(!fresh.contains("Edit the explicitly attached notes"));
    assert!(!fresh.contains("PEER_ONLY_SECRET"));
    assert!(!fresh.contains("Hold this callback"));
    assert!(!fresh.contains("report (completed)"));
    drop(requests);
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    ws.close(None).await.unwrap();
    // Only this test's freshly spawned host is stopped; no process discovery/signals.
    host.kill().await.unwrap();
    host.wait().await.unwrap();
    provider_task.abort();
    let _ = provider_task.await;
}
