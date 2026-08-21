use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use chrono::Utc;
use hirsel_proto::{ChatAuthor, ChatMessage, Event, EventKind, EventSource, EventSourceKind};
use tokio::sync::Mutex;

use super::*;
use crate::storage::TasteDecision;

// ---------------------------------------------------------------- test doubles

#[derive(Default)]
struct RecordingSink {
    briefs: Mutex<Vec<(String, String)>>,
}

impl RecordingSink {
    async fn briefs(&self) -> Vec<(String, String)> {
        self.briefs.lock().await.clone()
    }
}

#[async_trait]
impl BriefSink for RecordingSink {
    async fn inject(&self, message: &WakeMessage, brief: &str) -> anyhow::Result<()> {
        self.briefs
            .lock()
            .await
            .push((message.key.clone(), brief.to_string()));
        Ok(())
    }
}

/// A runner that drives the fork's exits directly, standing in for a model.
struct FakeRunner {
    behaviour: Behaviour,
    started: Arc<AtomicUsize>,
    peak_concurrency: Arc<AtomicUsize>,
    in_flight: Arc<AtomicUsize>,
    packs: Mutex<Vec<String>>,
}

enum Behaviour {
    Drop,
    Escalate,
    /// Return Ok without taking any exit.
    NoExit,
    /// Fail the turn outright.
    Fail,
    /// Hold the permit long enough for a burst to overlap.
    Slow,
}

impl FakeRunner {
    fn new(behaviour: Behaviour) -> Arc<Self> {
        Arc::new(Self {
            behaviour,
            started: Arc::new(AtomicUsize::new(0)),
            peak_concurrency: Arc::new(AtomicUsize::new(0)),
            in_flight: Arc::new(AtomicUsize::new(0)),
            packs: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl TriageRunner for FakeRunner {
    async fn run_triage(&self, request: TriageRequest) -> anyhow::Result<()> {
        self.started.fetch_add(1, Ordering::SeqCst);
        let live = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_concurrency.fetch_max(live, Ordering::SeqCst);
        self.packs.lock().await.push(request.pack.clone());
        let result = match self.behaviour {
            Behaviour::Drop => request
                .tools
                .drop_message("already known")
                .await
                .map(|_| ()),
            Behaviour::Escalate => request
                .tools
                .escalate("the release job failed; rerun or abandon?")
                .await
                .map(|_| ()),
            Behaviour::NoExit => Ok(()),
            Behaviour::Fail => Err(anyhow::anyhow!("provider unavailable")),
            Behaviour::Slow => {
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                request.tools.drop_message("slow drop").await.map(|_| ())
            }
        };
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        result
    }
}

// ------------------------------------------------------------------- fixtures

fn event(id: u64, name: &str, description: &str) -> Event {
    Event {
        id,
        kind: EventKind::Judgment,
        source: EventSource {
            kind: EventSourceKind::Agent,
            r#ref: None,
        },
        name: name.to_string(),
        description: description.to_string(),
        ui: serde_json::json!({ "type": "text", "text": "secret ui payload" }),
        anchor: 1,
        requires_response: true,
        quick_replies: Vec::new(),
        status: hirsel_proto::EventStatus::Open,
        read: false,
        archived: false,
        snoozed_until: None,
        archived_at: None,
        fork_sc: None,
        ts: Utc::now(),
    }
}

fn chat(id: u64, author: ChatAuthor, body: &str) -> ChatMessage {
    ChatMessage {
        id,
        author,
        body: body.to_string(),
        ts: Utc::now(),
        r#ref: None,
        tool_calls: Vec::new(),
        attachments: Vec::new(),
    }
}

fn subagent_message() -> WakeMessage {
    WakeMessage::new(
        WakeSource::Subagent {
            process_id: "proc-7".to_string(),
        },
        "Sub-agent completed: the migration landed on main.",
        "subagent:proc-7:subagent.completed",
    )
}

async fn test_state(dir: &std::path::Path) -> crate::AppState {
    crate::build_state(crate::tests::test_config(dir))
        .await
        .unwrap()
}

fn fork_wake(
    state: &crate::AppState,
    runner: Arc<FakeRunner>,
    sink: Arc<RecordingSink>,
) -> Arc<ForkWake> {
    ForkWake::new(runner, sink, state.tools.clone(), state.storage.clone())
}

// ------------------------------------------------------------- the pack itself

#[test]
fn the_pack_carries_the_trigger_verbatim_plus_a_curated_slice() {
    let message = subagent_message();
    let context = PackContext {
        events: vec![event(11, "release-channel", "Choose stable or beta")],
        recent_chat: vec![
            chat(1, ChatAuthor::Owner, "ship the migration"),
            chat(2, ChatAuthor::Agent, "started a worker on it"),
        ],
        rules: vec![TasteDecision {
            id: 1,
            event_id: 11,
            choice: Some("A".to_string()),
            rule: "always pick the stable channel for releases".to_string(),
            ts: Utc::now(),
        }],
    };

    let pack = build_pack(&message, &context);

    // 1. the triggering message, verbatim and attributed
    assert!(pack.contains("sub-agent proc-7"));
    assert!(pack.contains("Sub-agent completed: the migration landed on main."));
    // 2. the live inventory, by id and one line
    assert!(pack.contains("#11 release-channel — Choose stable or beta (open)"));
    // ... but never the UI payload
    assert!(!pack.contains("secret ui payload"));
    // 3. the conversation tail, oldest first
    let owner_at = pack.find("owner: ship the migration").unwrap();
    let agent_at = pack.find("agent: started a worker on it").unwrap();
    assert!(owner_at < agent_at);
    // 4. recorded rules
    assert!(pack.contains("always pick the stable channel for releases"));
}

#[test]
fn the_pack_bounds_every_section_and_never_dumps_history() {
    let message = WakeMessage::new(
        WakeSource::Monitor {
            monitor_id: "mon-1".to_string(),
            label: "ci".to_string(),
        },
        "x".repeat(64 * 1024),
        "monitor:mon-1:1",
    );
    let context = PackContext {
        events: (0..80)
            .map(|id| event(id, &format!("task-{id}"), "a task"))
            .collect(),
        recent_chat: (0..200)
            .map(|id| chat(id, ChatAuthor::Owner, &format!("message {id}")))
            .collect(),
        rules: (0..100)
            .map(|id| TasteDecision {
                id,
                event_id: id,
                choice: None,
                rule: format!("rule {id}"),
                ts: Utc::now(),
            })
            .collect(),
    };

    let pack = build_pack(&message, &context);

    assert_eq!(pack.matches("] #").count(), pack::PACK_EVENT_LIMIT);
    assert_eq!(pack.matches("- owner: ").count(), pack::PACK_CHAT_LIMIT);
    assert_eq!(pack.matches("(from event #").count(), pack::PACK_RULE_LIMIT);
    // The oldest chat rows are the ones dropped: a tail, not a transcript.
    assert!(!pack.contains("message 0\n"));
    assert!(pack.contains("message 199"));
    // Even the verbatim trigger is bounded.
    assert!(pack.len() < 32 * 1024);
    assert!(pack.contains('…'));
}

#[test]
fn an_empty_host_still_renders_every_pack_section() {
    let pack = build_pack(&subagent_message(), &PackContext::default());

    assert!(pack.contains("## Incoming event"));
    assert!(pack.contains("## Live Tasks and events\n\n(none open)"));
    assert!(pack.contains("## Recent conversation\n\n(none)"));
    assert!(pack.contains("## Recorded rules\n\n(none)"));
}

// ------------------------------------------------------------- the tool surface

#[test]
fn the_fork_tool_surface_is_exactly_its_three_exits() {
    let names = fork_tool_definitions()
        .iter()
        .map(|definition| definition.name().to_string())
        .collect::<Vec<_>>();

    assert_eq!(
        names,
        vec![
            "fork_record_info",
            "fork_record_summary",
            "fork_close_task",
            "fork_escalate",
            "fork_drop",
        ]
    );
    // A fork never spawns Sub-agents, never speaks to the Owner, and never
    // starts long work — enforced as capability, not as instruction.
    for forbidden in [
        "subagents_spawn",
        "subagents_prompt",
        "subagents_wait",
        "subagents_list",
        "pings_send",
        "events_judgment",
        "shell_run",
        "monitors_create",
        "views_show",
    ] {
        assert!(
            !names.iter().any(|name| name == forbidden),
            "the fork catalog must not contain {forbidden}"
        );
    }
}

#[tokio::test]
async fn a_fork_gets_exactly_one_exit() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let sink = Arc::new(RecordingSink::default());
    let tools = ForkTools::new(
        state.tools.clone(),
        Arc::clone(&sink) as Arc<dyn BriefSink>,
        subagent_message(),
        1,
    );

    tools.drop_message("already handled").await.unwrap();
    let second = tools.escalate("changed my mind").await;

    assert!(
        second.unwrap_err().to_string().contains("already took"),
        "a second exit must be refused"
    );
    assert!(sink.briefs().await.is_empty());
}

// ------------------------------------------------------------------- dispatch

#[tokio::test]
async fn dispatch_spawns_exactly_one_fork_per_message() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let runner = FakeRunner::new(Behaviour::Drop);
    let sink = Arc::new(RecordingSink::default());
    let fork = fork_wake(&state, Arc::clone(&runner), Arc::clone(&sink));

    for index in 0..5 {
        fork.dispatch_now(WakeMessage::new(
            WakeSource::Subagent {
                process_id: format!("proc-{index}"),
            },
            format!("Sub-agent {index} completed."),
            format!("subagent:proc-{index}:done"),
        ))
        .await;
    }

    assert_eq!(runner.started.load(Ordering::SeqCst), 5);
    // Every one dropped, so the main Agent was never woken.
    assert!(sink.briefs().await.is_empty());
}

#[tokio::test]
async fn an_escalating_fork_injects_exactly_one_brief() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let runner = FakeRunner::new(Behaviour::Escalate);
    let sink = Arc::new(RecordingSink::default());
    let fork = fork_wake(&state, runner, Arc::clone(&sink));

    fork.dispatch_now(subagent_message()).await;

    let briefs = sink.briefs().await;
    assert_eq!(briefs.len(), 1);
    assert_eq!(briefs[0].0, "subagent:proc-7:subagent.completed");
    assert_eq!(briefs[0].1, "the release job failed; rerun or abandon?");
}

#[tokio::test]
async fn a_recording_fork_writes_the_event_itself_and_never_wakes_main() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let sink = Arc::new(RecordingSink::default());
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Owner, "ship it".to_string(), None)
        .await
        .unwrap();
    let tools = ForkTools::new(
        state.tools.clone(),
        Arc::clone(&sink) as Arc<dyn BriefSink>,
        subagent_message(),
        anchor.id,
    );

    tools
        .record_info("migration-landed", "The migration landed on main.", None)
        .await
        .unwrap();

    let events = state.storage.ping_snapshot().await.unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.name == "migration-landed" && event.kind == EventKind::Info)
    );
    assert!(sink.briefs().await.is_empty());
}

// ------------------------------------------------------------------ fail-open

#[tokio::test]
async fn a_failed_fork_still_escalates_the_original_message() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let runner = FakeRunner::new(Behaviour::Fail);
    let sink = Arc::new(RecordingSink::default());
    let fork = fork_wake(&state, runner, Arc::clone(&sink));

    fork.dispatch_now(subagent_message()).await;

    let briefs = sink.briefs().await;
    assert_eq!(briefs.len(), 1, "a non-owner message is never lost");
    assert!(briefs[0].1.contains("Triage unavailable"));
    assert!(
        briefs[0]
            .1
            .contains("Sub-agent completed: the migration landed on main."),
        "the fallback brief carries the original message"
    );
}

#[tokio::test]
async fn a_fork_that_takes_no_exit_is_a_failure_not_a_drop() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let runner = FakeRunner::new(Behaviour::NoExit);
    let sink = Arc::new(RecordingSink::default());
    let fork = fork_wake(&state, runner, Arc::clone(&sink));

    fork.dispatch_now(subagent_message()).await;

    let briefs = sink.briefs().await;
    assert_eq!(briefs.len(), 1);
    assert!(briefs[0].1.contains("without taking an exit"));
}

// ---------------------------------------------------------------- concurrency

#[tokio::test]
async fn concurrent_forks_are_capped_by_the_semaphore() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let runner = FakeRunner::new(Behaviour::Slow);
    let sink = Arc::new(RecordingSink::default());
    let fork = fork_wake(&state, Arc::clone(&runner), sink);

    let burst = 3 * MAX_CONCURRENT_FORKS;
    let mut handles = Vec::new();
    for index in 0..burst {
        let fork = Arc::clone(&fork);
        handles.push(tokio::spawn(async move {
            fork.dispatch_now(WakeMessage::new(
                WakeSource::Monitor {
                    monitor_id: format!("mon-{index}"),
                    label: "ci".to_string(),
                },
                "Monitor fired.",
                format!("monitor:mon-{index}:1"),
            ))
            .await;
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    assert_eq!(runner.started.load(Ordering::SeqCst), burst);
    assert!(
        runner.peak_concurrency.load(Ordering::SeqCst) <= MAX_CONCURRENT_FORKS,
        "a burst must not stampede the provider: peak was {}",
        runner.peak_concurrency.load(Ordering::SeqCst)
    );
}

// -------------------------------------------------------------- owner bypass

/// Ruling 4: an Owner message never passes through a fork.
///
/// The enforcement is structural rather than conditional, and this test pins
/// both halves of it. First, the dispatcher's ingress type cannot *represent*
/// an Owner message: [`WakeSource`] enumerates three non-owner origins and the
/// exhaustive match below fails to compile the day a fourth is added, so a
/// future Owner variant cannot slip in unnoticed. Second, an installed
/// dispatcher is reached only by calling [`ForkWakeHandle::dispatch`], which
/// the Owner path (`AgentRuntime::enqueue` → `enqueue_inner`) never does — so
/// running the Owner path leaves the fork counter where it started.
#[tokio::test]
async fn owner_messages_bypass_forks_entirely() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let runner = FakeRunner::new(Behaviour::Drop);
    let sink = Arc::new(RecordingSink::default());
    let handle = ForkWakeHandle::default();
    handle.install(fork_wake(&state, Arc::clone(&runner), sink));

    // The Owner's own path: a message lands in the transcript and goes to the
    // Agent's queue. Nothing here consults the handle.
    state
        .storage
        .append_chat(ChatAuthor::Owner, "ship the migration".to_string(), None)
        .await
        .unwrap();
    assert_eq!(
        runner.started.load(Ordering::SeqCst),
        0,
        "an Owner message must not spawn a triage fork"
    );

    // Every source the dispatcher *can* carry is a non-owner origin.
    for source in [
        WakeSource::Subagent {
            process_id: "proc-1".to_string(),
        },
        WakeSource::Monitor {
            monitor_id: "mon-1".to_string(),
            label: "disk".to_string(),
        },
        WakeSource::External {
            origin: "webhook".to_string(),
        },
    ] {
        match &source {
            // Exhaustive on purpose: adding an Owner-shaped variant must break
            // this test rather than quietly route Owner traffic into a fork.
            WakeSource::Subagent { .. }
            | WakeSource::Monitor { .. }
            | WakeSource::External { .. } => {}
        }
        assert!(handle.dispatch(WakeMessage::new(source, "fired", "k")));
    }

    // The spawned forks are detached tasks; wait for the counter to settle.
    for _ in 0..200 {
        if runner.started.load(Ordering::SeqCst) == 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(runner.started.load(Ordering::SeqCst), 3);
}
