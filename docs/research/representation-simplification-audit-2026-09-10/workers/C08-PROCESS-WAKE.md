# C08-PROCESS-WAKE audit report

Snapshot audited: 3ee0621a603659ab0168f565b99012b642415419
Expected tree: a4aac830c45398a66591f2c44b707aaf3cef281b
Scope: the exact C08 owned files and the named shared table definition, with
protocol, store, Lash runtime, and storage consumers read only. No source
files, tests, builds, application code, live data/configuration, providers,
sessions, or processes were changed or executed. The only intended write is
this report.

## Verdict

Two materially useful findings are confirmed. The first is a high-confidence
cross-layer contract defect: the browser advertises delegated sub-agent
processes while the Rust wire type and every host production producer support
only monitors, and the flat DTO permits kind-specific fields in the wrong
combination. The second is a high-confidence monitor projection defect: every
completed probe advances last_event_ts, so the UI reports a non-waking poll as
the monitor having fired.

| ID | Priority | Finding | Reachability |
| --- | --- | --- | --- |
| C08-01 | High | ProcessInfo has divergent Rust/TypeScript kind contracts and no host sub-agent projection; its flat optional fields admit cross-kind states. | Real sub-agent runtime processes exist, but no production ProcessInfo producer projects them. "subagent" is accepted by the web type and rejected by the Rust enum; wrong optional combinations are constructible in existing fixtures. |
| C08-02 | High | Monitor probe activity is stored/projected as last_event_ts, which ProcessRow interprets as "last fired" even when MonitorTick.wake is false. | Reachable on the first unchanged Changed probe, any unmatched Regex probe, and any non-matching ExitZero probe in both Lash and scripted monitor loops. |

## Snapshot and method

The supplied exclusions were read before classification. Existing outcomes #2–14,
the planned native artifact viewer (#10), the tracked #16/#18 work, and root
fresh-review regressions were not reported. The two required lenses were
applied:

- schemasmash: invalid-but-representable states, ownership and conversion
  boundaries, duplicate truth, and amplification;
- audit-your-codebase: full ownership coverage, simpler state/control flow,
  producer/consumer reachability, and explicit deferred leads.

The pre-audit commands returned:

~~~
git rev-parse HEAD       3ee0621a603659ab0168f565b99012b642415419
git rev-parse HEAD^{tree} a4aac830c45398a66591f2c44b707aaf3cef281b
git status --porcelain    [empty]
~~~

## Coverage contract

### Whole-file owners inspected in full

Every assigned whole-file owner was read with line numbers:

~~~
app/src/components/processes/ProcessRow.tsx
app/src/components/processes/ProcessesSheet.tsx
app/src/components/processes/ProcessesView.tsx
crates/hirsel-host/src/fork_wake.rs
crates/hirsel-host/src/fork_wake/dispatch.rs
crates/hirsel-host/src/fork_wake/pack.rs
crates/hirsel-host/src/fork_wake/session.rs
crates/hirsel-host/src/fork_wake/tools.rs
crates/hirsel-host/src/lash_runtime/process_engines.rs
crates/hirsel-host/src/lash_runtime/timers.rs
crates/hirsel-host/src/monitors.rs
crates/hirsel-host/src/process_run.rs
crates/hirsel-host/src/storage/monitors.rs
crates/hirsel-host/src/tools/digest.rs
crates/hirsel-host/src/tools/monitors.rs
crates/hirsel-proto/src/process.rs
app/src/components/processes/process-row.test.tsx
crates/hirsel-host/src/fork_wake/tests.rs
crates/hirsel-host/src/storage/monitors/tests.rs
~~~

### Shared definition and read-only consumer context

The assigned shared definition crates/hirsel-host/src/storage/current.sql:63
(monitors) was inspected, including its complete column layout at :63-77.
Read-only conversion/consumer context included:

~~~
app/src/protocol.ts
app/src/store/reducer.ts
app/src/store/selectors.ts
app/src/store/types.ts
app/src/ws/client.ts
app/PROTOCOL.md
crates/hirsel-proto/src/host.rs
crates/hirsel-host/src/protocol.rs
crates/hirsel-host/src/lib.rs
crates/hirsel-host/src/lash_runtime/lifecycle.rs
crates/hirsel-host/src/lash_runtime/runtime_tasks.rs
crates/hirsel-host/src/lash_runtime/runtime.rs
crates/hirsel-host/src/lash_runtime/scripted.rs
crates/hirsel-host/src/lash_runtime/thread_lanes.rs
crates/hirsel-host/src/storage/thread_completion.rs
crates/hirsel-host/src/lash_runtime/plugin.rs
docs/DEVIATIONS.md
crates/hirsel-client-core/tests/client_flow.rs
app/src/store/reducer.processes.test.ts
app/src/store/selectors.test.ts
~~~

Consumers remained consumers; no adjacent file was treated as C08 ownership.

## Finding C08-01 — ProcessInfo kind drift and flattened kind-specific fields

Verdict: recommend a clean contract cutover. Confidence: high. There is one
process DTO contract, but it is declared twice with incompatible kind sets,
and the host production path can only create the monitor half. The web surface
therefore promises a process inventory that the host cannot send, and the
shared flat shape cannot enforce the documented “subagent only” metadata rule.

### Evidence across definitions, producers, conversions, and consumers

The Rust wire definition has only one kind, while its flat structure makes
agent and model legal for every kind:

> crates/hirsel-proto/src/process.rs:6-33

~~~
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessKind {
    Monitor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub thread_id: u64,
    pub id: String,
    pub kind: ProcessKind,
    pub label: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub state: ProcessState,
    pub started_ts: DateTime<Utc>,
    pub last_event_ts: DateTime<Utc>,
    pub summary: Option<String>,
}
~~~

The browser mirror advertises a second kind but retains the same unguarded
optionals:

> app/src/protocol.ts:42-61

~~~
export type ProcessKind = "subagent" | "monitor";
export interface ProcessInfo {
  thread_id: number;
  id: string;
  kind: ProcessKind;
  label: string;
  /** subagent kind only: the acting agent + model, shown as small chips. */
  agent: string | null;
  model: string | null;
  state: ProcessState;
  started_ts: string;
  last_event_ts: string;
  summary: string | null;
}
~~~

The protocol carries this DTO in both the snapshot and upsert envelope:

> crates/hirsel-proto/src/host.rs:24-40

~~~
HelloOk {
    history_id: String,
    threads: Vec<crate::Thread>,
    processes: Vec<ProcessInfo>,
    host_version: String,
    model: Option<ModelSnapshot>,
    subagent_models: Option<SubagentModelCatalog>,
    prompts: Option<PromptSnapshot>,
    providers: Option<ProviderRoster>,
    views: Vec<ViewInstance>,
},
Msg {
    message: ChatMessage,
},
ProcessUpsert {
    process: ProcessInfo,
},
~~~

> app/src/protocol.ts:437-440

~~~
export interface ProcessUpsertMsg {
  type: "process_upsert";
  process: ProcessInfo;
}
~~~

The only production constructor found is the monitor conversion:

> crates/hirsel-host/src/storage/monitors.rs:251-272

~~~
pub fn monitor_process_info(record: &MonitorRecord) -> ProcessInfo {
    ProcessInfo {
        thread_id: record.thread_id,
        id: record.id.clone(),
        kind: ProcessKind::Monitor,
        label: short_label(&record.label),
        agent: None,
        model: None,
        state: if record.cancelled_ts.is_some() {
            ProcessState::Cancelled
        } else {
            ProcessState::Running
        },
        started_ts: record.created_ts,
        last_event_ts: record.last_event_ts,
        // The client's monitor rows have no dedicated cmd/interval fields;
        // the summary carries them (see app/PROTOCOL.md v1.4 notes).
        summary: Some(match &record.summary {
            Some(summary) => format!("{} · every {}s — {summary}", record.cmd, record.every_secs),
            None => format!("{} · every {}s", record.cmd, record.every_secs),
        }),
    }
}
~~~

The snapshot and both monitor broadcast paths remain monitor-only:

> crates/hirsel-host/src/lib.rs:508-534

~~~
pub async fn process_snapshot(&self) -> anyhow::Result<Vec<ProcessInfo>> {
    let all = self.storage.monitor_snapshot().await?;
    let mut running = Vec::new();
    let mut terminal = Vec::new();
    for process in all {
        if matches!(process.state, hirsel_proto::ProcessState::Running) {
            running.push(process);
        } else {
            terminal.push(process);
        }
    }
    running.sort_by(|left, right| {
        left.started_ts
            .cmp(&right.started_ts)
            .then_with(|| left.id.cmp(&right.id))
    });
    terminal.sort_by(|left, right| {
        left.last_event_ts
            .cmp(&right.last_event_ts)
            .then_with(|| left.id.cmp(&right.id))
    });
    if terminal.len() > 10 {
        terminal.drain(..terminal.len() - 10);
    }
    running.extend(terminal);
    Ok(running)
}
~~~

> crates/hirsel-host/src/lib.rs:563-566

~~~
self.broadcast(HostToClient::ProcessUpsert {
    process: monitor_process_info(record),
});
~~~

> crates/hirsel-host/src/tools/monitors.rs:58-61

~~~
self.broadcast(hirsel_proto::HostToClient::ProcessUpsert {
    process: monitor_process_info(record),
});
~~~

This is not merely stale terminology: the actual runtime is configured with
Lash processes/triggers and the project documents sub-agent runtime processes:

> crates/hirsel-host/src/lash_runtime/lifecycle.rs:108-112

~~~
.with_lashlang_abilities(
    lash_protocol_rlm::RlmAbilities::default()
        .with_processes()
        .with_triggers(),
)
~~~

> docs/DEVIATIONS.md:12-15

~~~
Sub-agent starts create Lash Runtime Processes with RecoveryDisposition::OwnerBound;
driver terminal events append terminal Process Events and enqueue Lash ProcessWake work.
~~~

The UI has an active sub-agent branch and only displays metadata under that
branch:

> app/src/components/processes/ProcessRow.tsx:91-95,181-203

~~~
const isSubagent = () => p().kind === "subagent";
const isMonitor = () => p().kind === "monitor";
const hasFired = () => p().last_event_ts !== p().started_ts;
// ...
<Show when={isSubagent() && (p().agent || p().model)}>
  {/* agent/model chips */}
</Show>
<Show when={isMonitor() && hasFired()}>
  <span>last fired {formatRelativeTime(p().last_event_ts)}</span>
</Show>
~~~

The web store only caches/replaces the DTO; it does not repair the mismatch:

> app/src/store/reducer.ts:4-9

~~~
case "hello_ok": return { ...state, processes: hello.processes, /* ... */ };
case "process_upsert": return {
  ...state,
  processes: [...state.processes.filter(row => row.id !== action.payload.process.id), action.payload.process],
};
~~~

### Concrete invalid states and reachability

The following states are representable now:

1. Rust ProcessInfo { kind: ProcessKind::Monitor, agent: Some(...),
   model: Some(...) }. The existing client-flow fixture constructs exactly
   this at crates/hirsel-client-core/tests/client_flow.rs:60-72.
2. TypeScript { kind: "subagent", agent: null, model: null, ... }. The
   selector fixture constructs it at app/src/store/selectors.test.ts:8-20.
3. A real Lash sub-agent process exists in the runtime process store, but no
   production ProcessInfo conversion or ProcessUpsert source exists for it.
   The host's HelloOk process list is therefore monitor-only, while the
   web type and row branch claim both kinds.
4. A JSON process with kind: "subagent" can be accepted by the TypeScript
   mirror but cannot deserialize into the Rust ProcessKind enum at
   crates/hirsel-proto/src/process.rs:6-10.

The first two are reachable in constructed/test values; the third is a
reachable product gap whenever a real sub-agent is started; the fourth is a
cross-client wire incompatibility. No live values were read.

The reproducible producer/consumer query used for this finding was:

~~~
rg -n 'ProcessKind::Subagent|kind === "subagent"|kind: "subagent"|HostToClient::ProcessUpsert|ProcessUpsert \{|process_upsert' \
  crates/hirsel-host/src crates/hirsel-proto/src app/src --glob '!app/node_modules/**'
~~~

It returns 22 matches in this snapshot. The separate constructor query

~~~
rg -n 'ProcessInfo \{' crates/hirsel-host/src crates/hirsel-proto/src app/src --glob '!app/node_modules/**'
~~~

returns 8 matches, of which the only production host constructor is
crates/hirsel-host/src/storage/monitors.rs:251-252; the remaining matches are
the definition, protocol mirror, and fixtures/tests.

### Duplicate truth check

No duplicate mutable process-row write path was found. The host monitor record
is the source for the monitor DTO, and the browser reducer replaces its cache
by process ID at app/src/store/reducer.ts:9. The defect is two divergent type
definitions plus a missing source projection, not two writers that can diverge
one process row. The target must keep ProcessInfo a read projection rather
than introduce a second process table.

### Smallest credible target

Use one discriminated union at the Rust wire boundary and one matching
TypeScript discriminated union. ProcessState remains the existing five-state
sum; only kind-specific fields move into their variants:

~~~
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ProcessInfo {
    Monitor {
        thread_id: u64,
        id: String,
        label: String,
        state: ProcessState,
        started_ts: DateTime<Utc>,
        last_event_ts: DateTime<Utc>,
        summary: Option<String>,
    },
    Subagent {
        thread_id: u64,
        id: String,
        label: String,
        agent: Option<String>,
        model: Option<String>,
        state: ProcessState,
        started_ts: DateTime<Utc>,
        last_event_ts: DateTime<Utc>,
        summary: Option<String>,
    },
}
~~~

The corresponding app representation is:

~~~
type ProcessInfo =
  | (CommonProcessFields & { kind: "monitor" })
  | (CommonProcessFields & {
      kind: "subagent";
      agent: string | null;
      model: string | null;
    });
~~~

CommonProcessFields contains thread_id, id, label, state, started_ts,
last_event_ts, and summary; a monitor has no agent/model fields.
monitor_process_info constructs the Monitor variant. A single read-only host
projection seam must also map the authoritative Lash Runtime Process
records/events into Subagent rows before process_snapshot and subsequent
ProcessUpsert broadcasts. No ProcessInfo table or duplicate mutable inventory
should be added. ProcessRow should switch on the discriminant so the compiler
makes both rendering paths exhaustive.

This target removes Monitor + Some(agent/model) and Subagent + absent producer
as legal-looking states. It also makes the “subagent-only” metadata rule
enforceable instead of comment-only. The exact runtime process-to-DTO adapter
is outside the assigned C08 files; the C08-owned contract and monitor
conversion are the narrow seams to change, with the adapter coordinated with
the Lash process owner.

### Smallest affected implementation surface

Owned files/interfaces: crates/hirsel-proto/src/process.rs,
crates/hirsel-host/src/storage/monitors.rs, crates/hirsel-host/src/lib.rs's
process_snapshot/broadcast seam, app/src/components/processes/ProcessRow.tsx,
and app/src/components/processes/process-row.test.tsx. Required adjacent
contract consumers are app/src/protocol.ts, the process store fixtures, and
the Lash runtime process projection source (not C08-owned).

### Regression, cutover, and validation

This is a coordinated protocol cutover. Monitor JSON currently includes
agent: null and model: null in the round-trip fixture at
crates/hirsel-proto/src/tests.rs:214-238; the proposed monitor variant omits
those meaningless fields. Native clients and the web mirror must update
together. The current-schema policy permits a clean current contract, but no
compatibility shim should be added. The projection must preserve stable IDs,
thread_id, terminal state, last-activity ordering, and full upsert replacement
semantics.

Existing fixtures demonstrate the bad/ambiguous states (the Rust Monitor +
Some fixture and TypeScript sub-agent fixtures), but no test demonstrates a
real host sub-agent ProcessInfo or a Rust/TypeScript round-trip for a
sub-agent. Inspection-only validation required after an implementation is:

- Rust serde tests for both variants and rejection of cross-kind fields;
- a host projection test covering a running and terminal sub-agent plus a
  monitor in one snapshot, with one-ID upsert replacement;
- TypeScript compile/type tests and ProcessRow tests for both discriminants;
- protocol fixture updates for HelloOk and ProcessUpsert, including a
  native-client round trip.

## Finding C08-02 — Monitor probe time is reported as firing time

Verdict: recommend an explicit wake timestamp. Confidence: high. The monitor
has a boolean wake decision, but persistence occurs before the caller branches
on that decision and writes the same timestamp into both last_run_ts and
last_event_ts. The UI uses last_event_ts != started_ts as a firing predicate.
A normal non-waking probe therefore produces a false “last fired” label.

### Evidence across the monitor state machine

The table stores run and event timestamps but no wake timestamp:

> crates/hirsel-host/src/storage/current.sql:63-77

~~~
CREATE TABLE monitors (
    thread_id INTEGER NOT NULL REFERENCES threads(id),
    id TEXT PRIMARY KEY,
    cmd TEXT NOT NULL,
    every_secs INTEGER NOT NULL,
    wake_on TEXT NOT NULL,
    pattern TEXT NULL,
    label TEXT NOT NULL,
    created_ts TEXT NOT NULL,
    last_event_ts TEXT NOT NULL,
    last_run_ts TEXT NULL,
    last_output TEXT NULL,
    summary TEXT NULL,
    cancelled_ts TEXT NULL
);
~~~

The wake decision is distinct from probe completion:

> crates/hirsel-host/src/monitors.rs:20-44,78-91

~~~
pub struct MonitorTick {
    pub probe: MonitorProbeOutput,
    pub summary: String,
    pub wake: bool,
    pub wake_text: Option<String>,
}

pub async fn run_monitor_tick(record: &MonitorRecord) -> MonitorTick {
    let probe = run_probe(&record.cmd).await;
    let wake = monitor_should_wake(record, &probe);
    // ...
}

fn monitor_should_wake(record: &MonitorRecord, probe: &MonitorProbeOutput) -> bool {
    match record.wake_on {
        MonitorWakeOn::Changed => record.last_output.as_ref()
            .is_some_and(|previous| previous != &probe.output),
        MonitorWakeOn::ExitZero => probe.status == Some(0),
        MonitorWakeOn::ExitNonzero => probe.timed_out || probe.status != Some(0),
        MonitorWakeOn::Regex => /* match record.pattern against output */,
    }
}
~~~

Both storage write paths update last_event_ts for every completed probe,
without receiving wake:

> crates/hirsel-host/src/storage/monitors.rs:168-190

~~~
pub async fn record_monitor_tick(/* monitor_id, output, summary */) {
    // ...
    SET last_run_ts = ?2,
        last_event_ts = ?2,
        last_output = ?3,
        summary = ?4
}
~~~

> crates/hirsel-host/src/storage/monitors.rs:398-415

~~~
pub(crate) async fn record_background_monitor_tick(/* ... */) {
    // ...
    c.execute(
        "UPDATE monitors SET last_run_ts=?2,last_event_ts=?2,
         last_output=?3,summary=?4 WHERE id=?1 AND cancelled_ts IS NULL",
        /* ... */,
    )?;
}
~~~

The Lash engine persists before checking the decision:

> crates/hirsel-host/src/lash_runtime/process_engines.rs:86-115

~~~
let tick = run_monitor_tick(&record).await;
let updated = self.tools.storage().record_background_monitor_tick(/* ... */).await?;
if !tick.wake {
    continue;
}
append_monitor_wake(&processes, &updated, &tick, &self.fork_wake).await?;
~~~

The scripted loop has the same ordering:

> crates/hirsel-host/src/lash_runtime/scripted.rs:117-134

~~~
let tick = run_monitor_tick(&record).await;
runtime.tools.record_monitor_tick(/* output, summary */).await?;
if tick.wake && let Some(text) = tick.wake_text {
    runtime.deliver_monitor_wake(text).await;
}
~~~

The storage conversion sends last_event_ts to the generic process DTO:

> crates/hirsel-host/src/storage/monitors.rs:251-266

~~~
started_ts: record.created_ts,
last_event_ts: record.last_event_ts,
~~~

The UI then turns that generic activity timestamp into a firing fact:

> app/src/components/processes/ProcessRow.tsx:91-95,197-203

~~~
const isMonitor = () => p().kind === "monitor";
const hasFired = () => p().last_event_ts !== p().started_ts;
// ...
<Show when={isMonitor() && hasFired()}>
  <span>last fired {formatRelativeTime(p().last_event_ts)}</span>
</Show>
~~~

### Concrete invalid state and reachability

Concrete sequence:

1. At t0, create a Changed monitor. Creation initializes last_output = None
   and last_event_ts = created_ts at
   crates/hirsel-host/src/storage/monitors.rs:28-43.
2. At t1, the first probe returns the same output the command will keep
   returning. MonitorWakeOn::Changed returns false because there is no prior
   output (monitors.rs:80-83). An unmatched regex and a non-zero ExitZero
   probe produce the same wake = false outcome.
3. The engine nevertheless writes last_run_ts = t1 and last_event_ts = t1
   (storage/monitors.rs:178-185), then takes the !tick.wake branch and emits
   no monitor.wake/fork delivery (process_engines.rs:108-110).
4. monitor_process_info exposes last_event_ts = t1, and the row evaluates
   t1 != t0; the UI displays last fired t1 even though no wake occurred.

Thus the invalid projected state is “last_event_ts later than start while
MonitorTick.wake == false”, not a hypothetical malformed database row. It is
reachable in both monitor execution paths. The existing storage fixture at
crates/hirsel-host/src/storage/monitors/tests.rs:7-61 manually records one
tick and checks summary/state, but it does not run run_monitor_tick, encode a
false wake, or assert timestamp meaning. The process-row fixture at
app/src/components/processes/process-row.test.tsx:13-66 only covers a
sub-agent resting row and does not cover monitor firing semantics.

The reproducible consumer query used for this finding was:

~~~
rg -n 'last_event_ts|last_run_ts|last fired|hasFired|monitor_should_wake|record_background_monitor_tick' \
  crates/hirsel-host/src/storage/monitors.rs \
  crates/hirsel-host/src/lash_runtime/process_engines.rs \
  crates/hirsel-host/src/monitors.rs \
  crates/hirsel-host/src/storage/current.sql \
  app/src/components/processes/ProcessRow.tsx \
  crates/hirsel-host/src/tools/monitors.rs
~~~

It returns 38 matches in this snapshot.

### Duplicate truth check

No distinct-writer path was found for one intended timestamp fact. The code
writes the same ?2 into last_run_ts and last_event_ts at
storage/monitors.rs:178-180 and writes both fields again in the background
path at :413; however, cancellation intentionally updates only the lifecycle
event at :154-157. This shows the fields have different intended meanings
(probe completion versus lifecycle activity), so the defect is semantic
conflation rather than an unverified duplicate source of truth. The fix should
make the actual wake fact explicit, not add another writer or infer it from
timestamps.

### Smallest credible target

Keep the existing activity ordering field, and add a nullable wake-specific
field at every layer:

~~~
-- current schema4 target
last_event_ts TEXT NOT NULL, -- latest monitor activity for ordering
last_run_ts TEXT NULL,       -- latest completed probe
last_wake_ts TEXT NULL,      -- latest probe with wake = true
~~~

The Rust record becomes:

~~~
pub struct MonitorRecord {
    // existing identity/config/output/state fields
    pub last_event_ts: DateTime<Utc>,
    pub last_run_ts: Option<DateTime<Utc>>,
    pub last_wake_ts: Option<DateTime<Utc>>,
    // existing last_output, summary, cancelled_ts
}
~~~

record_monitor_tick and record_background_monitor_tick must accept wake: bool.
They always set last_run_ts and last_event_ts; they set last_wake_ts only when
wake is true, for example with CASE WHEN ?5 THEN ?2 ELSE last_wake_ts END.
Creation leaves it null; cancellation updates activity but does not
manufacture a wake. The Lash and scripted callers pass the already computed
tick.wake, so there is one decision and one persistence owner.

The monitor process projection adds last_wake_ts to the monitor DTO. In the
preferred C08-01 union it is a field of the Monitor variant; if that union is
implemented separately, the minimum temporary mirror is
last_wake_ts: string | null on the process protocol. partitionProcesses
continues sorting by last_event_ts, while ProcessRow uses:

~~~
const hasFired = () => isMonitor() && p().last_wake_ts !== null;
// render last fired from p().last_wake_ts
~~~

This removes the invalid inference that any poll is a wake and preserves the
independent ordering fact used by app/src/store/selectors.ts:20-33.

### Smallest affected implementation surface

Owned files/interfaces: crates/hirsel-host/src/storage/current.sql,
crates/hirsel-host/src/storage/monitors.rs, crates/hirsel-host/src/monitors.rs,
crates/hirsel-host/src/lash_runtime/process_engines.rs,
crates/hirsel-host/src/tools/monitors.rs, crates/hirsel-proto/src/process.rs,
app/src/components/processes/ProcessRow.tsx, and
app/src/components/processes/process-row.test.tsx. The adjacent scripted
caller at crates/hirsel-host/src/lash_runtime/scripted.rs and the app
protocol/store fixtures must be updated as consumers.

### Regression, cutover, and validation

The assigned current schema is explicitly schema4/current-only, so add the
column directly in current.sql and update exact-catalog validation; do not add
a compatibility migration. Existing monitor snapshots and process rows must
remain ordered by activity, and cancellation must still be terminal. Protocol
clients need the new nullable monitor field (or the C08-01 variant) in one
coordinated cutover.

Existing tests do not demonstrate the condition. Inspection-only validation
required after implementation is:

- storage tests for initial NULL, a false-wake tick preserving NULL, a
  true-wake tick setting the timestamp, and cancellation not setting it;
- Lash and scripted engine tests proving the same MonitorTick.wake controls
  persistence and delivery;
- process projection tests for running/terminal monitors and activity sorting;
- ProcessRow tests proving a non-wake update has no “last fired” label and a
  true wake does;
- exact current-schema/catalog and protocol round-trip fixture updates.

## Explicit no-findings, deferred leads, and skip reasons

- app/src/components/processes/ProcessesSheet.tsx: inspected as the right pane
  mount and accessibility/container surface. It owns no process state,
  conversion, or write path; no C08 representation finding.
- app/src/components/processes/ProcessesView.tsx: inspected grouping,
  running/finished counts, stop action, and thread focus. It delegates to
  partitionProcesses and ProcessRow; no independent process truth or invalid
  state beyond the two findings above.
- crates/hirsel-host/src/fork_wake.rs: module/export documentation and
  WakeSource/WakeMessage ownership are coherent. WakeMessage.thread_id is
  routing identity and is checked by its dispatcher; no separate mutable wake
  store.
- crates/hirsel-host/src/fork_wake/pack.rs: WakeSource is a closed
  monitor/external sum and build_pack has explicit trigger/thread/chat bounds.
  No invalid collection state or duplicate pack writer found.
- crates/hirsel-host/src/fork_wake/session.rs: fresh per-fork session,
  timeout, close, and exact tool allow-list are explicit. No C08 state defect.
- crates/hirsel-host/src/fork_wake/tools.rs: ForkExit and ExitSlot are
  explicit open/claimed/taken states; record/escalate side effects commit only
  after the sink/storage operation. Existing tool schemas are closed and
  exact. No additional useful simplification found.
- crates/hirsel-host/src/fork_wake/tests.rs: inspected all fixtures and tests.
  It covers failed/no-exit/panicking forks, concurrency, owner bypass,
  refusal, empty transcript, and activity recording. It does not cover the
  two promoted findings or context lookup failure; no additional test-only
  finding is reported.
- crates/hirsel-host/src/fork_wake/dispatch.rs: an unpromoted lead exists:
  dispatch_now documents “every failure mode” must fail open at :129-131, but
  destination mismatch and background_context errors return at :149-161. The
  mismatch is a direct API invariant violation and normal registry routing
  selects the runtime from message.thread_id at thread_lanes.rs:280-287;
  history reset aborts and drains old runtime tasks before replacing
  projections at runtime_tasks.rs:20-33 and thread_lanes.rs:229-260. The
  snapshot therefore does not establish a normal reachable wake-loss path, and
  no fixture reaches either branch. This is deferred, not a recommendation.
- crates/hirsel-host/src/lash_runtime/timers.rs: TimerSchedule has three
  optional clock fields at :133-139, but from_registration rejects every count
  other than exactly one at :181-189 before due_occurrence's one-shot expect
  at :228-243. The trigger source schema at lash_runtime/plugin.rs:60-86 is
  permissive, but invalid external registrations are rejected and no
  production writer stores an unvalidated TimerSchedule. Existing tests cover
  valid one-shot/recurring schedules and a multiple-field rejection at
  lash_runtime/tests.rs:842-893; this latent internal construction possibility
  is not materially promoted.
- crates/hirsel-host/src/monitors.rs, storage/monitors.rs, and
  lash_runtime/tool_defs.rs also expose a lower-priority shape lead:
  MonitorWakeOn plus optional pattern accepts Changed + Some(pattern) because
  validation at storage/monitors.rs:275-287 requires a pattern only for Regex,
  while monitors.rs:78-90 ignores it for other modes. The tool schema is
  correspondingly independent at tool_defs.rs:307-325. This is a real
  representable combination, but its extra value is inert, required regex
  input is already rejected when absent, and no divergent write path or
  user-visible consequence was demonstrated. It is not promoted over C08-02.
- crates/hirsel-host/src/process_run.rs: process-group timeout cleanup and the
  owned timeout fixture were inspected; no new finding beyond excluded #18.
- crates/hirsel-host/src/tools/digest.rs: scheduled digest activity and
  expired-snooze cleanup use one storage/publish path; no duplicate truth.
- crates/hirsel-host/src/tools/monitors.rs: monitor create/cancel/tick methods
  forward storage results and broadcast the single monitor projection; their
  timestamp forwarding is part of C08-02, with no independent issue.
- crates/hirsel-host/src/storage/monitors/tests.rs: valid creation, interval
  floor, tick summary, projection, and cancellation are covered. The missing
  false-wake assertion is a validation gap for C08-02, not a third finding.
- The shared monitors table at current.sql:63-77 has no separate writer
  outside the storage helper in this snapshot. Its flat wake/pattern columns
  and timestamp columns are discussed above; no additional schema owner was
  found.

## Final handoff (under 300 words)

Verdict: two fixes are worth promoting.

1. C08-01, high confidence/high priority: make ProcessInfo one Rust and
   TypeScript discriminated union, then add the missing host projection for
   authoritative Lash sub-agent processes. Current Rust production output is
   monitor-only, while the web contract claims subagents and both flat DTOs
   admit wrong kind-specific metadata. Existing fixtures demonstrate the
   ambiguity; no live values or tests were run.
2. C08-02, high confidence/high priority: persist last_wake_ts separately
   from probe/activity time and make the UI use it. A normal false Changed,
   unmatched Regex, or non-matching ExitZero poll currently advances
   last_event_ts, and ProcessRow labels it “last fired”.

Evidence is fully recorded above in
/tmp/hirsel-combined-audit/workers/C08-PROCESS-WAKE.md. Expected HEAD is
3ee0621a603659ab0168f565b99012b642415419; expected tree is
a4aac830c45398a66591f2c44b707aaf3cef281b. Post-report verification must
confirm the source checkout remains clean and unchanged.

## Post-report source invariant

The required post-report read-only verification commands returned:

~~~
git rev-parse HEAD        3ee0621a603659ab0168f565b99012b642415419
git rev-parse HEAD^{tree} a4aac830c45398a66591f2c44b707aaf3cef281b
git status --porcelain     [empty]
~~~

The source checkout therefore remains at the expected commit/tree and clean.
Only this report file was written outside the source checkout.
