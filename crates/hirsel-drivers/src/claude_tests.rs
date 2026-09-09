use super::*;
use futures_util::StreamExt;

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
    std::fs::write(&path, format!("import sys,json,os,time,subprocess\ndef send(v):\n print(json.dumps(v),flush=True)\ndef ack(v):\n send(dict(v,isReplay=True,session_id='fixture'))\n{source}\n")).unwrap();
    let mut command = Command::new("python3");
    command.arg(path);
    let task = SpawnSpec {
        agent: AgentKind::Claude,
        model: None,
        variant: None,
        prompt: "initial".into(),
        cwd: dir.path().into(),
        fake_fixture: None,
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
source="import time,json; time.sleep(0.08); print(json.dumps({'type':'result','is_error':False,'result':'late final'}),flush=True)"
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
