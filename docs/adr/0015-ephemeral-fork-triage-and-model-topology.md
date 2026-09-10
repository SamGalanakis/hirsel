# Thread-local ephemeral triage for monitor and scheduled wakes

Updated for the nested Thread decision in [ADR-0016](0016-threads-own-conversation.md).

Each Lash-backed Thread has its own lazy resident session. Human input and
child reports enter that Thread's durable FIFO directly. Monitor and scheduled
wakes carry their originating Thread and history; an ephemeral triage fork
receives only that Thread's context. No global resident or Task transcript is
retained.

The fork has four concrete exits: record information, record a summary,
escalate a concise brief into its own Thread's queue, or drop the wake. Its
catalog contains only those exits. It cannot delegate work, read peer or parent
conversations, invoke shell/plugin tools, or choose another destination.
Triage policy remains in the editable fork prompt; the host enforces resource
scope and validates execution/history at storage boundaries.

A monitor emits one current process event type, `monitor.wake`, with no direct
resident wake selector. The installed Thread dispatcher performs triage. There
is no alternate pre-triage event or fallback into a global Agent. Fork failure
uses the current Thread-addressed fallback brief and preserves provenance.

The main and fork prompts and current provider/model selections remain Settings
features. Child conversations use the enabled delegation model catalog exposed
by `threads_delegate`; model/variant/cwd are captured at acceptance. Advisory
work is an ordinary focused child assignment and returns through the same
durable report mechanism. CLI children and Lash resident sessions share host
coordination authority rather than separate process work inventories.
