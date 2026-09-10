use super::*;
use futures_util::StreamExt;

#[tokio::test]
async fn fable_model_and_effort_reach_the_cli_unchanged() {
    let (_dir, mut task, command) = fixture(
        r#"
assert option('--model') == 'claude-fable-5-1'
assert option('--effort') == 'max'
first=json.loads(sys.stdin.readline()); ack(first)
send({'type':'result','is_error':False,'result':'fable configured'})
"#,
    );
    task.model = Some("claude-fable-5-1".into());
    task.variant = Some("max".into());
    let driver = ClaudeCodeDriver::default();
    let handle = driver
        .spawn_command(task, command, Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(
        outcome(&driver, &handle).await,
        TerminalOutcome::Done {
            summary: "fable configured".into()
        }
    );
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn immediate_duplicate_results_replay_one_terminal_to_late_subscriber() {
    let (_dir, task, command) = fixture(
        r#"
first=json.loads(sys.stdin.readline()); ack(first)
send({'type':'result','is_error':False,'result':'first final'})
send({'type':'result','is_error':False,'result':'duplicate final'})
sys.exit(7)
"#,
    );
    let driver = ClaudeCodeDriver::default();
    let handle = driver
        .spawn_command(task, command, Duration::from_secs(2))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        outcome(&driver, &handle).await,
        TerminalOutcome::Done {
            summary: "first final".into()
        }
    );
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn retirement_kills_before_waiting_for_a_blocked_stdin_writer() {
    let (_dir, task, command) = fixture(
        r#"
first=json.loads(sys.stdin.readline()); ack(first)
time.sleep(30)
"#,
    );
    let driver = Arc::new(ClaudeCodeDriver::default());
    let handle = driver
        .spawn_command(task, command, Duration::from_secs(2))
        .await
        .unwrap();
    let writer_driver = driver.clone();
    let writer_handle = handle.clone();
    let writer = tokio::spawn(async move {
        writer_driver
            .prompt(&writer_handle, "x".repeat(2 * 1024 * 1024))
            .await
    });
    let session = driver.sessions.get(&handle).unwrap();
    timeout(Duration::from_secs(2), async {
        loop {
            if session.stdin.try_lock().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    timeout(Duration::from_secs(2), driver.retire(&handle))
        .await
        .unwrap()
        .unwrap();
    assert!(writer.await.unwrap().is_err());
}

fn fixture(source: &str) -> (tempfile::TempDir, SpawnSpec, Command) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peer.py");
    let bootstrap = include_str!("claude_fixture.py");
    let bootstrap = bootstrap
        .split("if __name__ == \"__main__\":")
        .next()
        .unwrap();
    std::fs::write(
        &path,
        format!("{bootstrap}\nbridge,rpc=initialize()\n{source}\n"),
    )
    .unwrap();
    let scoped_mcp = crate::test_support::scoped_mcp_fixture(
        dir.path(),
        &["threads_context", "threads_delegate"],
    );
    let mut command = Command::new("python3");
    command.arg(path);
    let task = SpawnSpec {
        agent: AgentKind::Claude,
        model: None,
        variant: None,
        prompt: "initial".into(),
        cwd: dir.path().into(),
        fake_fixture: None,
        scoped_mcp,
    };
    (dir, task, command)
}

async fn outcome(driver: &ClaudeCodeDriver, handle: &SessionHandle) -> TerminalOutcome {
    let mut events = driver.events(handle).unwrap();
    loop {
        let event = timeout(Duration::from_secs(3), events.next())
            .await
            .unwrap()
            .unwrap();
        if let SubagentEvent::Terminal { outcome } = event {
            assert!(events.next().await.is_none(), "terminal closes stream");
            return outcome;
        }
    }
}

async fn assert_stopped(path: &std::path::Path) {
    let pid = std::fs::read_to_string(path).unwrap();
    for _ in 0..100 {
        let running =
            std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())).is_ok_and(|s| {
                !s.split(')')
                    .nth(1)
                    .unwrap_or("")
                    .trim_start()
                    .starts_with('Z')
            });
        if !running {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("fixture child still running: {pid}");
}

#[tokio::test]
async fn echo_receipt_and_correlated_interrupt_rejection() {
    let (_dir, task, command) = fixture(
        r#"
first=json.loads(sys.stdin.readline()); ack(first)
follow=json.loads(sys.stdin.readline())
send(dict(follow,uuid='unrelated',isReplay=True))
time.sleep(0.06); ack(follow)
request=json.loads(sys.stdin.readline())
send({'type':'control_response','response':{'request_id':'unrelated','subtype':'success'}})
send({'type':'control_response','response':{'request_id':request['request_id'],'subtype':'error','error':'cannot interrupt'}})
time.sleep(5)
"#,
    );
    let driver = ClaudeCodeDriver::default();
    let handle = driver
        .spawn_command(task, command, Duration::from_secs(2))
        .await
        .unwrap();
    let start = Instant::now();
    driver.prompt(&handle, "followup".into()).await.unwrap();
    assert!(start.elapsed() >= Duration::from_millis(50));
    assert!(
        matches!(driver.interrupt(&handle).await,Err(DriverError::Protocol(message)) if message == "cannot interrupt")
    );
    assert!(matches!(
        outcome(&driver, &handle).await,
        TerminalOutcome::Interrupted
    ));
    assert!(matches!(
        driver.prompt(&handle, "closed".into()).await,
        Err(DriverError::SessionClosed)
    ));
    assert!(matches!(
        driver.interrupt(&handle).await,
        Err(DriverError::SessionClosed)
    ));
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn clean_exit_without_result_fails_and_held_pipe_is_bounded() {
    let (dir, task, command) = fixture(
        r#"
first=json.loads(sys.stdin.readline()); ack(first)
child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'])
open('descendant.pid','w').write(str(child.pid))
sys.exit(0)
"#,
    );
    let driver = ClaudeCodeDriver::default();
    let handle = driver
        .spawn_command(task, command, Duration::from_secs(2))
        .await
        .unwrap();
    assert!(
        matches!(outcome(&driver,&handle).await,TerminalOutcome::Failed {reason} if reason.contains("without terminal result"))
    );
    assert_stopped(&dir.path().join("descendant.pid")).await;
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn late_result_is_drained_once_and_followup_after_result_is_rejected() {
    let (_dir, task, command) = fixture(
        r#"
first=json.loads(sys.stdin.readline()); ack(first)
source="import time,json; time.sleep(0.08); print(json.dumps({'type':'result','is_error':False,'result':'late final','session_id':'%s'}),flush=True)" % SESSION
subprocess.Popen([sys.executable,'-c',source])
sys.exit(7)
"#,
    );
    let driver = ClaudeCodeDriver::default();
    let handle = driver
        .spawn_command(task, command, Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(
        outcome(&driver, &handle).await,
        TerminalOutcome::Done {
            summary: "late final".into()
        }
    );
    assert!(matches!(
        driver.prompt(&handle, "too late".into()).await,
        Err(DriverError::SessionClosed)
    ));
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn startup_timeout_and_cancellation_kill_the_owned_group() {
    for cancel in [false, true] {
        let (dir, task, command) = fixture(
            r#"
open('child.pid','w').write(str(os.getpid()))
child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'])
open('descendant.pid','w').write(str(child.pid))
time.sleep(30)
"#,
        );
        let driver = ClaudeCodeDriver::default();
        if cancel {
            let startup = tokio::spawn(async move {
                driver
                    .spawn_command(task, command, Duration::from_secs(30))
                    .await
            });
            timeout(Duration::from_secs(3), async {
                loop {
                    if std::fs::read_to_string(dir.path().join("descendant.pid"))
                        .is_ok_and(|pid| pid.parse::<u32>().is_ok())
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            startup.abort();
            assert!(startup.await.unwrap_err().is_cancelled());
        } else {
            assert!(matches!(
                driver
                    .spawn_command(task, command, Duration::from_secs(1))
                    .await,
                Err(DriverError::RequestTimeout(_))
            ));
        }
        assert_stopped(&dir.path().join("child.pid")).await;
        assert_stopped(&dir.path().join("descendant.pid")).await;
    }
}

#[tokio::test]
async fn terminal_before_input_echo_rejects_followup() {
    let (_dir, task, command) = fixture(
        r#"
first=json.loads(sys.stdin.readline()); ack(first)
follow=json.loads(sys.stdin.readline())
send({'type':'result','is_error':False,'result':'already finished'})
ack(follow)
time.sleep(5)
"#,
    );
    let driver = ClaudeCodeDriver::default();
    let handle = driver
        .spawn_command(task, command, Duration::from_secs(2))
        .await
        .unwrap();
    assert!(matches!(
        driver.prompt(&handle, "too late".into()).await,
        Err(DriverError::SessionClosed)
    ));
    assert!(matches!(
        outcome(&driver, &handle).await,
        TerminalOutcome::Done { .. }
    ));
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn control_timeout_removes_waiter_and_stderr_drains_during_startup() {
    let (_dir, task, command) = fixture(
        r#"
sys.stderr.write('x'*200000); sys.stderr.flush()
first=json.loads(sys.stdin.readline()); ack(first)
request=json.loads(sys.stdin.readline()); time.sleep(5)
"#,
    );
    let driver = ClaudeCodeDriver::default();
    let handle = driver
        .spawn_command(task, command, Duration::from_secs(2))
        .await
        .unwrap();
    let session = driver.sessions.get(&handle).unwrap();
    let id = Uuid::new_v4().to_string();
    let result = session
        .request(
            json!({"type":"control_request","request_id":id,"request":{"subtype":"interrupt"}}),
            id,
            false,
            Duration::from_millis(100),
        )
        .await;
    assert!(matches!(result, Err(DriverError::RequestTimeout(_))));
    assert!(lock(&session.pending).unwrap().is_empty());
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn scoped_bridge_runs_recursive_tools_and_preserves_full_output_and_launch() {
    let (dir, mut task, command) = fixture(
        r#"
assert option('--model') == 'claude-opus-5'
assert option('--effort') == 'high'
assert os.path.isfile(os.path.join(os.getcwd(),'scoped_mcp.json'))
first=json.loads(sys.stdin.readline()); ack(first)
assert first['message']['content'][0]['text'] == 'accepted own Thread brief'
context=rpc('tools/call', {'name':'threads_context','arguments':{}})
assert json.loads(context['content'][0]['text'])['thread_id'] == 17
child=rpc('tools/call', {'name':'threads_delegate','arguments':{'title':'child','brief':'focused work','artifact_ids':[]}})
assert json.loads(child['content'][0]['text'])['thread_id'] == 18
send({'type':'stream_event','event':{'type':'content_block_delta','delta':{'type':'text_delta','text':'early progress'}}})
text='é' * 30000 + ' exact final ending'
send({'type':'assistant','message':{'stop_reason':'end_turn','content':[{'type':'text','text':text}]}})
send({'type':'result','subtype':'success','is_error':False,'result':text})
send({'type':'result','subtype':'success','is_error':False,'result':'duplicate'})
"#,
    );
    task.model = Some("claude-opus-5".into());
    task.variant = Some("high".into());
    task.prompt = "accepted own Thread brief".into();
    let config_path = dir.path().join("scoped_mcp.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["page_size"] = json!(1);
    config["log"] = json!(dir.path().join("bridge.log"));
    config["responses"] =
        json!({"threads_context":{"thread_id":17},"threads_delegate":{"thread_id":18}});
    std::fs::write(config_path, config.to_string()).unwrap();
    let driver = ClaudeCodeDriver::default();
    let handle = driver
        .spawn_command(task, command, Duration::from_secs(2))
        .await
        .unwrap();
    let events: Vec<_> = driver.events(&handle).unwrap().collect().await;
    let outputs: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            SubagentEvent::AssistantOutput { text } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(
        outputs,
        vec![&format!("{} exact final ending", "é".repeat(30000))]
    );
    assert!(matches!(
        events.last(),
        Some(SubagentEvent::Terminal {
            outcome: TerminalOutcome::Done { .. }
        })
    ));
    assert!(matches!(
        &events[events.len() - 2],
        SubagentEvent::AssistantOutput { .. }
    ));
    let requests: Vec<Value> = std::fs::read_to_string(dir.path().join("bridge.log"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "initialize")
            .count(),
        2
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "tools/list")
            .count(),
        4
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "tools/call")
            .count(),
        2
    );
    assert!(matches!(
        driver.prompt(&handle, "late".into()).await,
        Err(DriverError::SessionClosed)
    ));
    assert!(matches!(
        driver.interrupt(&handle).await,
        Err(DriverError::SessionClosed)
    ));
    driver.retire(&handle).await.unwrap();
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn preflight_rejects_foreign_catalog_without_starting_claude() {
    let (dir, mut task, command) = fixture("open('started','w').write('wrong')");
    task.scoped_mcp.expected_tools = vec!["threads_context".into()];
    assert!(matches!(
        ClaudeCodeDriver::default()
            .spawn_command(task, command, Duration::from_secs(2))
            .await,
        Err(DriverError::Protocol(_))
    ));
    assert!(!dir.path().join("started").exists());
}

#[tokio::test]
async fn init_foreign_connector_is_rejected_even_after_preflight() {
    let (dir, task, command) = fixture("first=json.loads(sys.stdin.readline()); ack(first)");
    let path = dir.path().join("peer.py");
    let source = std::fs::read_to_string(&path).unwrap().replace(
        r#"["Read", "Bash", *names]"#,
        r#"["Read", "Bash", "mcp__owner__threads_list", *names]"#,
    );
    std::fs::write(path, source).unwrap();
    assert!(
        ClaudeCodeDriver::default()
            .spawn_command(task, command, Duration::from_secs(2))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn malformed_and_error_results_never_invent_final_messages() {
    for result in [
        json!({"type":"result"}),
        json!({"type":"result","is_error":false}),
        json!({"type":"result","subtype":"error_during_execution","is_error":true,"errors":["provider rejected request"]}),
    ] {
        let source = format!(
            "first=json.loads(sys.stdin.readline()); ack(first)\nsend({})",
            result
                .to_string()
                .replace("false", "False")
                .replace("true", "True")
        );
        let (_dir, task, command) = fixture(&source);
        let driver = ClaudeCodeDriver::default();
        let handle = driver
            .spawn_command(task, command, Duration::from_secs(2))
            .await
            .unwrap();
        let events: Vec<_> = driver.events(&handle).unwrap().collect().await;
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SubagentEvent::AssistantOutput { .. }))
        );
        assert!(matches!(
            events.last(),
            Some(SubagentEvent::Terminal {
                outcome: TerminalOutcome::Failed { .. }
            })
        ));
        driver.retire(&handle).await.unwrap();
    }
}

#[tokio::test]
async fn failure_retains_only_actual_final_assistant_text() {
    for (stop, expected) in [("tool_use", false), ("end_turn", true)] {
        let source = format!(
            r#"
first=json.loads(sys.stdin.readline()); ack(first)
send({{'type':'assistant','message':{{'stop_reason':'{stop}','content':[{{'type':'text','text':'actual text'}}]}}}})
send({{'type':'result','subtype':'error_during_execution','is_error':True,'errors':['provider failed']}})
"#
        );
        let (_dir, task, command) = fixture(&source);
        let driver = ClaudeCodeDriver::default();
        let handle = driver
            .spawn_command(task, command, Duration::from_secs(2))
            .await
            .unwrap();
        let events: Vec<_> = driver.events(&handle).unwrap().collect().await;
        assert_eq!(
            events
                .iter()
                .any(|e| matches!(e,SubagentEvent::AssistantOutput{text} if text=="actual text")),
            expected
        );
        driver.retire(&handle).await.unwrap();
    }
}

#[tokio::test]
async fn eof_retains_only_completed_final_output_and_replays_failure() {
    for (name, frame, expected) in [
        ("absent", "", false),
        (
            "partial delta",
            "send({'type':'stream_event','event':{'type':'content_block_delta','delta':{'type':'text_delta','text':text}}})",
            false,
        ),
        (
            "unfinished assistant",
            "send({'type':'assistant','message':{'stop_reason':None,'content':[{'type':'text','text':text}]}})",
            false,
        ),
        (
            "tool-use commentary",
            "send({'type':'assistant','message':{'stop_reason':'tool_use','content':[{'type':'text','text':text},{'type':'tool_use','id':'tool-1','name':'Read','input':{'file_path':'example'}}]}})",
            false,
        ),
        (
            "empty completed assistant",
            "send({'type':'assistant','message':{'stop_reason':'end_turn','content':[]}})",
            false,
        ),
        (
            "completed assistant",
            "send({'type':'assistant','message':{'stop_reason':'end_turn','content':[{'type':'text','text':text}]}})",
            true,
        ),
    ] {
        let source = format!(
            "first=json.loads(sys.stdin.readline()); ack(first)\ntext='é' * 30000 + ' exact final ending'\n{frame}\nsys.exit(7)"
        );
        let (_dir, task, command) = fixture(&source);
        let driver = ClaudeCodeDriver::default();
        let handle = driver
            .spawn_command(task, command, Duration::from_secs(2))
            .await
            .unwrap();
        let events: Vec<_> = timeout(
            Duration::from_secs(3),
            driver.events(&handle).unwrap().collect(),
        )
        .await
        .unwrap();
        let outputs: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                SubagentEvent::AssistantOutput { text } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(outputs.len(), usize::from(expected), "{name}");
        if expected {
            assert_eq!(
                outputs[0],
                &format!("{} exact final ending", "é".repeat(30000))
            );
            assert!(matches!(
                events.get(events.len() - 2),
                Some(SubagentEvent::AssistantOutput { .. })
            ));
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, SubagentEvent::Terminal { .. }))
                .count(),
            1,
            "{name}"
        );
        assert!(matches!(
            events.last(),
            Some(SubagentEvent::Terminal { outcome: TerminalOutcome::Failed { reason } })
                if reason.contains("without terminal result")
        ));
        let replay: Vec<_> = driver.events(&handle).unwrap().collect().await;
        assert_eq!(replay, events, "late subscriber: {name}");
        driver.retire(&handle).await.unwrap();
    }
}

#[tokio::test]
async fn interrupt_ack_waits_for_terminal_and_timeout_cleans_up() {
    for complete in [false, true] {
        let source = format!(
            r#"
first=json.loads(sys.stdin.readline()); ack(first)
open('child.pid','w').write(str(os.getpid()))
request=json.loads(sys.stdin.readline())
send({{'type':'control_response','response':{{'request_id':request['request_id'],'subtype':'success'}}}})
time.sleep(0.08)
{}
time.sleep(30)
"#,
            if complete {
                "send({'type':'result','is_error':True,'terminal_reason':'aborted_streaming'})"
            } else {
                ""
            }
        );
        let (dir, task, command) = fixture(&source);
        let driver = ClaudeCodeDriver::default();
        let handle = driver
            .spawn_command(task, command, Duration::from_secs(2))
            .await
            .unwrap();
        let session = driver.sessions.get(&handle).unwrap();
        let started = Instant::now();
        let result = session
            .interrupt_with_timeout(Uuid::new_v4().to_string(), Duration::from_millis(200))
            .await;
        assert_eq!(result.is_ok(), complete);
        assert!(started.elapsed() >= Duration::from_millis(70));
        assert!(matches!(
            outcome(&driver, &handle).await,
            TerminalOutcome::Interrupted
        ));
        assert_stopped(&dir.path().join("child.pid")).await;
        driver.retire(&handle).await.unwrap();
    }
}

#[tokio::test]
async fn empty_success_and_cancel_without_final_emit_no_assistant_output() {
    for result in [
        "{'type':'result','is_error':False,'result':''}",
        "{'type':'result','is_error':True,'terminal_reason':'aborted_streaming'}",
    ] {
        let (_dir, task, command) = fixture(&format!(
            "first=json.loads(sys.stdin.readline()); ack(first)\nsend({result})"
        ));
        let driver = ClaudeCodeDriver::default();
        let handle = driver
            .spawn_command(task, command, Duration::from_secs(2))
            .await
            .unwrap();
        let events: Vec<_> = driver.events(&handle).unwrap().collect().await;
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, SubagentEvent::AssistantOutput { .. }))
        );
        assert!(matches!(
            events.last(),
            Some(SubagentEvent::Terminal { .. })
        ));
        driver.retire(&handle).await.unwrap();
    }
}
