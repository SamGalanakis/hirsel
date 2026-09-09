use super::*;
use futures_util::StreamExt;
use tempfile::TempDir;

const PEER: &str = r#"
import json, os, subprocess, sys, time
mode, directory = sys.argv[1:]
with open(directory + '/pid', 'w') as f:
    f.write(str(os.getpid()))

def send(value):
    print(json.dumps(value), flush=True)

def reply(request, result):
    send({'id': request['id'], 'result': result})

def reject(request):
    send({'id': request['id'], 'error': {'code': -32000, 'message': 'fixture rejected ' + request['method']}})

def event(method, thread='root', **params):
    send({'method': method, 'params': {'threadId': thread, **params}})

def complete(thread='root', turn='turn-a'):
    event('turn/completed', thread, turn={'id': turn, 'status': 'completed'})

initialized = False
pending = []
for line in sys.stdin:
    request = json.loads(line)
    with open(directory + '/requests', 'a') as f:
        f.write(json.dumps(request) + '\n')
    if 'method' not in request:
        assert request['error']['code'] == -32601
        assert request['id'] == 4
        reply(pending.pop(), {'turnId': 'turn-a'})
        continue
    method = request['method']
    if method == 'initialize':
        if mode == 'malformed':
            print('{broken-json', flush=True)
            time.sleep(60)
        if mode == 'hang-startup':
            time.sleep(60)
        if mode == 'reject-initialize':
            reject(request)
            continue
        if mode == 'stderr':
            os.write(2, b'x' * 262144)
        reply(request, {'userAgent': 'hermetic-codex'})
    elif method == 'initialized':
        initialized = True
    elif method == 'thread/start':
        assert initialized, 'thread/start before initialized'
        if mode == 'reject-thread':
            reject(request)
            continue
        send({'method': 'thread/started', 'params': {'thread': {'id': 'child'}}})
        reply(request, {'thread': {'id': 'root'}})
    elif method == 'turn/start':
        assert request['params']['threadId'] == 'root'
        if mode == 'reject-turn':
            reject(request)
            continue
        reply(request, {'turn': {'id': 'turn-a'}})
        event('turn/started', turn={'id': 'turn-a'})
        if mode == 'child':
            event('turn/started', 'child', turn={'id': 'child-turn'})
            event('item/completed', 'child', item={'type': 'agentMessage', 'text': 'CHILD RESULT'})
            complete('child', 'child-turn')
            complete('root', 'stale-root-turn')
            send({'method': 'turn/completed', 'params': {'turn': {'id': 'turn-a', 'status': 'completed'}}})
            event('item/completed', item={'type': 'agentMessage', 'text': 'root barrier'})
        if mode == 'closed-stdin-final':
            os.close(0)
            send({'id': 'closing-native-request', 'method': 'item/tool/requestUserInput', 'params': {'threadId': 'root'}})
            time.sleep(0.05)
            event('item/completed', item={'type': 'agentMessage', 'text': 'last root result'})
            complete()
            sys.exit(0)
        if mode == 'reply-backpressure':
            descendant = subprocess.Popen([sys.executable, '-c', 'import time;time.sleep(60)'])
            with open(directory + '/descendant', 'w') as f:
                f.write(str(descendant.pid))
            while not os.path.exists(directory + '/send-native-request'):
                time.sleep(0.001)
            send({'id': 'native-request', 'method': 'item/tool/requestUserInput', 'params': {'threadId': 'root'}})
            while not os.path.exists(directory + '/exit-now'):
                time.sleep(0.001)
            sys.exit(0)
        if mode in ['exit-zero', 'inherited-pipes']:
            if mode == 'inherited-pipes':
                descendant = subprocess.Popen([sys.executable, '-c', 'import time;time.sleep(60)'])
                with open(directory + '/descendant', 'w') as f:
                    f.write(str(descendant.pid))
            sys.exit(0)
        if mode == 'done':
            event('item/completed', item={'type': 'agentMessage', 'text': 'root result'})
            complete()
            sys.exit(0)
    elif method == 'turn/steer':
        assert request['params']['threadId'] == 'root'
        assert request['params']['expectedTurnId'] == 'turn-a'
        if mode == 'hang-control':
            continue
        if mode == 'close-control':
            sys.exit(0)
        if mode == 'control-errors':
            reject(request)
        elif mode == 'out-of-order':
            pending.append(request)
        elif mode == 'server-request':
            pending.append(request)
            send({'id': request['id'], 'method': 'item/tool/requestUserInput', 'params': {'threadId': 'root'}})
        elif mode == 'wrong-steer-turn':
            reply(request, {'turnId': 'unexpected-turn'})
        else:
            reply(request, {'turnId': 'turn-a'})
    elif method == 'turn/interrupt':
        assert request['params']['threadId'] == 'root'
        assert request['params']['turnId'] == 'turn-a'
        if mode == 'control-errors':
            reject(request)
        elif mode == 'out-of-order':
            pending.append(request)
        else:
            reply(request, {})
            event('item/completed', item={'type': 'agentMessage', 'text': 'ROOT RESULT'})
            complete()
    if len(pending) == 2:
        for waiting in reversed(pending):
            if waiting['method'] == 'turn/steer':
                reject(waiting)
            else:
                reply(waiting, {})
        pending.clear()
"#;

struct Peer {
    directory: TempDir,
}

impl Peer {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("peer.py"), PEER).unwrap();
        Self { directory }
    }

    async fn spawn(&self, driver: &CodexDriver, mode: &str) -> DriverResult<SessionHandle> {
        self.spawn_with_timeout(driver, mode, Duration::from_secs(3))
            .await
    }

    async fn spawn_with_timeout(
        &self,
        driver: &CodexDriver,
        mode: &str,
        control_timeout: Duration,
    ) -> DriverResult<SessionHandle> {
        let mut command = Command::new("python3");
        command
            .arg(self.directory.path().join("peer.py"))
            .arg(mode)
            .arg(self.directory.path());
        driver
            .spawn_command(
                SpawnSpec {
                    agent: AgentKind::Codex,
                    model: None,
                    variant: None,
                    prompt: "initial task".into(),
                    cwd: self.directory.path().to_path_buf(),
                    fake_fixture: None,
                },
                command,
                Vec::new(),
                control_timeout,
            )
            .await
    }

    fn requests(&self) -> Vec<Value> {
        std::fs::read_to_string(self.directory.path().join("requests"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    async fn wait_pid(&self) -> u32 {
        timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(self.directory.path().join("pid"))
                    && let Ok(pid) = text.parse()
                {
                    break pid;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
}

async fn wait_dead(pid: u32) {
    timeout(Duration::from_secs(3), async {
        loop {
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Ok(stat)
                    if stat
                        .rsplit_once(") ")
                        .is_some_and(|(_, tail)| tail.starts_with("Z ")) =>
                {
                    break;
                }
                _ => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .expect("owned fixture process survived cleanup");
}

async fn next_terminal(events: &mut EventStream) -> TerminalOutcome {
    timeout(Duration::from_secs(3), async {
        while let Some(event) = events.next().await {
            if let SubagentEvent::Terminal { outcome } = event {
                return outcome;
            }
        }
        panic!("stream closed without terminal");
    })
    .await
    .expect("terminal never arrived")
}

#[tokio::test]
async fn native_handshake_is_ordered_and_stderr_is_drained_before_initialization() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let handle = peer.spawn(&driver, "stderr").await.unwrap();
    let requests = peer.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| request["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["initialize", "initialized", "thread/start", "turn/start"]
    );
    assert_eq!(requests[2]["params"]["approvalPolicy"], "never");
    assert_eq!(requests[2]["params"]["sandbox"], "danger-full-access");
    driver.retire(&handle).await.unwrap();
    wait_dead(peer.wait_pid().await).await;
}

#[tokio::test]
async fn every_startup_rejection_and_malformed_output_roll_back_the_owned_process() {
    for mode in [
        "reject-initialize",
        "reject-thread",
        "reject-turn",
        "malformed",
    ] {
        let peer = Peer::new();
        let driver = CodexDriver::default();
        let error = peer.spawn(&driver, mode).await.unwrap_err();
        assert!(
            error.to_string().contains(if mode == "malformed" {
                "protocol error"
            } else {
                "fixture rejected"
            }),
            "{mode}: {error}"
        );
        wait_dead(peer.wait_pid().await).await;
    }
}

#[tokio::test]
async fn startup_timeout_and_cancellation_kill_the_owned_process() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let error = peer
        .spawn_with_timeout(&driver, "hang-startup", Duration::from_secs(2))
        .await
        .unwrap_err();
    assert!(matches!(error, DriverError::RequestTimeout(_)));
    wait_dead(peer.wait_pid().await).await;

    let peer = Arc::new(Peer::new());
    let spawn_peer = peer.clone();
    let task = tokio::spawn(async move {
        spawn_peer
            .spawn(&CodexDriver::default(), "hang-startup")
            .await
    });
    let pid = peer.wait_pid().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    wait_dead(pid).await;
}

#[tokio::test]
async fn child_traffic_cannot_change_the_root_turn_result_or_steering_target() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let handle = peer.spawn(&driver, "child").await.unwrap();
    let mut events = driver.events(&handle).unwrap();
    timeout(Duration::from_secs(3), async {
        loop {
            match events.next().await.unwrap() {
                SubagentEvent::Started { external_id } => assert_eq!(external_id, "root"),
                SubagentEvent::Progress { summary } if summary == "root barrier" => break,
                event => panic!("child traffic leaked: {event:?}"),
            }
        }
    })
    .await
    .unwrap();
    driver
        .prompt(&handle, "steer the same run".into())
        .await
        .unwrap();
    driver.interrupt(&handle).await.unwrap();
    assert_eq!(
        next_terminal(&mut events).await,
        TerminalOutcome::Done {
            summary: "ROOT RESULT".into()
        }
    );
    let requests = peer.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "turn/start")
            .count(),
        1
    );
    assert_eq!(requests[4]["method"], "turn/steer");
    assert_eq!(requests[4]["params"]["expectedTurnId"], "turn-a");
    assert!(matches!(
        driver.prompt(&handle, "after completion".into()).await,
        Err(DriverError::NoActiveTurn)
    ));
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn native_control_errors_are_returned_to_the_matching_caller() {
    for mode in ["control-errors", "out-of-order"] {
        let peer = Peer::new();
        let driver = CodexDriver::default();
        let handle = peer.spawn(&driver, mode).await.unwrap();
        let (steer, interrupt) = tokio::join!(
            driver.prompt(&handle, "follow-up".into()),
            driver.interrupt(&handle)
        );
        assert!(
            steer
                .unwrap_err()
                .to_string()
                .contains("fixture rejected turn/steer")
        );
        if mode == "control-errors" {
            assert!(
                interrupt
                    .unwrap_err()
                    .to_string()
                    .contains("fixture rejected turn/interrupt")
            );
        } else {
            interrupt.unwrap();
        }
        driver.retire(&handle).await.unwrap();
    }
}

#[tokio::test]
async fn control_deadline_and_transport_closure_fail_pending_requests_and_settle() {
    for mode in ["hang-control", "close-control"] {
        let peer = Peer::new();
        let driver = CodexDriver::default();
        let handle = peer
            .spawn_with_timeout(&driver, mode, Duration::from_secs(2))
            .await
            .unwrap();
        let mut events = driver.events(&handle).unwrap();
        let error = driver
            .prompt(&handle, "follow-up".into())
            .await
            .unwrap_err();
        if mode == "hang-control" {
            assert!(matches!(error, DriverError::RequestTimeout(_)));
        }
        assert!(matches!(
            next_terminal(&mut events).await,
            TerminalOutcome::Failed { .. }
        ));
        wait_dead(peer.wait_pid().await).await;
        driver.retire(&handle).await.unwrap();
    }
}

#[tokio::test]
async fn clean_exit_and_inherited_pipes_cannot_leave_a_run_unsettled() {
    for mode in ["exit-zero", "inherited-pipes"] {
        let peer = Peer::new();
        let driver = CodexDriver::default();
        let handle = peer.spawn(&driver, mode).await.unwrap();
        let mut events = driver.events(&handle).unwrap();
        assert!(matches!(
            next_terminal(&mut events).await,
            TerminalOutcome::Failed { .. }
        ));
        if mode == "inherited-pipes" {
            let pid = std::fs::read_to_string(peer.directory.path().join("descendant"))
                .unwrap()
                .parse()
                .unwrap();
            wait_dead(pid).await;
        }
        driver.retire(&handle).await.unwrap();
    }
}

#[tokio::test]
async fn a_result_before_eof_remains_the_single_terminal_outcome() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let handle = peer.spawn(&driver, "done").await.unwrap();
    let mut events = driver.events(&handle).unwrap();
    assert_eq!(
        next_terminal(&mut events).await,
        TerminalOutcome::Done {
            summary: "root result".into()
        }
    );
    assert!(events.next().await.is_none());
    wait_dead(peer.wait_pid().await).await;
    driver.retire(&handle).await.unwrap();
    let session_events = driver.events(&handle);
    assert!(matches!(
        session_events,
        Err(DriverError::SessionNotFound(_))
    ));
}

#[tokio::test]
async fn steering_acknowledgement_must_name_the_same_active_turn() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let handle = peer.spawn(&driver, "wrong-steer-turn").await.unwrap();
    let error = driver.prompt(&handle, "same run".into()).await.unwrap_err();
    assert!(error.to_string().contains("different or missing turn id"));
    let mut events = driver.events(&handle).unwrap();
    assert!(matches!(
        next_terminal(&mut events).await,
        TerminalOutcome::Failed { .. }
    ));
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn unsupported_native_requests_are_rejected_without_stealing_a_client_response() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let handle = peer.spawn(&driver, "server-request").await.unwrap();
    driver.prompt(&handle, "same run".into()).await.unwrap();
    let requests = peer.requests();
    let response = requests
        .iter()
        .find(|value| value.get("error").is_some())
        .unwrap();
    assert_eq!(response["id"], 4);
    assert_eq!(response["error"]["code"], -32601);
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn native_reply_backpressure_does_not_block_direct_child_exit_supervision() {
    let peer = Peer::new();
    let driver = Arc::new(CodexDriver::default());
    let handle = peer.spawn(&driver, "reply-backpressure").await.unwrap();
    let mut events = driver.events(&handle).unwrap();
    let writer_driver = Arc::clone(&driver);
    let writer_handle = handle.clone();
    let writer = tokio::spawn(async move {
        writer_driver
            .prompt(&writer_handle, "x".repeat(2 * 1024 * 1024))
            .await
    });
    let session = driver.sessions.get(&handle).unwrap();
    timeout(Duration::from_secs(3), async {
        while session.stdin.try_lock().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixture writer never blocked");
    std::fs::write(peer.directory.path().join("send-native-request"), "").unwrap();
    timeout(Duration::from_secs(3), async {
        loop {
            match events.next().await.unwrap() {
                SubagentEvent::Progress { summary }
                    if summary.starts_with("unsupported codex server request:") =>
                {
                    break;
                }
                SubagentEvent::Terminal { outcome } => panic!("fixture ended early: {outcome:?}"),
                _ => {}
            }
        }
    })
    .await
    .expect("supervisor did not receive native request");
    std::fs::write(peer.directory.path().join("exit-now"), "").unwrap();
    timeout(Duration::from_secs(2), async {
        assert!(matches!(
            next_terminal(&mut events).await,
            TerminalOutcome::Failed { .. }
        ));
        assert!(writer.await.unwrap().is_err());
        driver.retire(&handle).await.unwrap();
    })
    .await
    .expect("blocked response delayed exit supervision");
    wait_dead(peer.wait_pid().await).await;
    let descendant = std::fs::read_to_string(peer.directory.path().join("descendant"))
        .unwrap()
        .parse()
        .unwrap();
    wait_dead(descendant).await;
}

#[tokio::test]
async fn failed_native_reply_still_drains_a_valid_final_result() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let handle = peer.spawn(&driver, "closed-stdin-final").await.unwrap();
    let mut events = driver.events(&handle).unwrap();
    assert_eq!(
        next_terminal(&mut events).await,
        TerminalOutcome::Done {
            summary: "last root result".into()
        }
    );
    assert!(events.next().await.is_none());
    driver.retire(&handle).await.unwrap();
}
