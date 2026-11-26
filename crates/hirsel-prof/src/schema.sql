PRAGMA application_id = 0x48505246;
PRAGMA user_version = 5;

CREATE TABLE run_meta (
    key TEXT PRIMARY KEY,
    value ANY NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE mapping (
    id INTEGER PRIMARY KEY,
    start_addr_hex TEXT NOT NULL,
    end_addr_hex TEXT NOT NULL,
    file_offset ANY NOT NULL,
    path TEXT NOT NULL,
    build_id TEXT,
    image_digest TEXT,
    start_ns ANY NOT NULL,
    end_ns ANY
) STRICT;

CREATE TABLE thread (
    id INTEGER PRIMARY KEY,
    pid INTEGER NOT NULL,
    tid INTEGER NOT NULL,
    name TEXT NOT NULL,
    start_ns ANY NOT NULL,
    end_ns ANY
) STRICT;

CREATE TABLE function (
    id INTEGER PRIMARY KEY,
    def_path TEXT,
    instance_label TEXT NOT NULL,
    name TEXT NOT NULL,
    module_kind TEXT NOT NULL CHECK (module_kind IN ('hirsel', 'runtime', 'foreign', 'kernel', 'unknown'))
) STRICT;

CREATE TABLE frame (
    id INTEGER PRIMARY KEY,
    mapping_id INTEGER REFERENCES mapping,
    rel_pc ANY,
    function_id INTEGER REFERENCES function,
    file TEXT,
    line INTEGER,
    col INTEGER,
    inlined INTEGER NOT NULL CHECK (inlined IN (0, 1))
) STRICT;

CREATE TABLE callsite (
    id INTEGER PRIMARY KEY,
    parent_id INTEGER REFERENCES callsite CHECK (parent_id < id),
    frame_id INTEGER NOT NULL REFERENCES frame,
    depth INTEGER NOT NULL CHECK (depth >= 0)
) STRICT;

CREATE TABLE cpu_sample (
    id INTEGER PRIMARY KEY,
    ts_ns ANY NOT NULL,
    thread_id INTEGER NOT NULL REFERENCES thread,
    task_id INTEGER REFERENCES task,
    host_stack_interval_id INTEGER REFERENCES host_stack_interval,
    cpu INTEGER,
    callsite_id INTEGER NOT NULL REFERENCES callsite,
    raw_pc_hex TEXT NOT NULL,
    event TEXT NOT NULL CHECK (event <> ''),
    period ANY NOT NULL CHECK (
        (typeof(period) = 'integer' AND period >= 0)
        OR (typeof(period) = 'text'
            AND instr(period, char(0)) = 0 AND period NOT GLOB '*[^0-9]*'
            AND ((length(period) = 19 AND period >= '9223372036854775808')
                OR (length(period) = 20
                    AND period BETWEEN '10000000000000000000' AND '18446744073709551615')))
    ),
    stack_root TEXT NOT NULL CHECK (stack_root IN ('fiber', 'host', 'truncated', 'failed', 'kernel')),
    unwind_status TEXT NOT NULL CHECK (unwind_status IN (
        'ok', 'fiber_root', 'truncated_stack', 'missing_cfi', 'unknown_mapping', 'eval_error', 'not_attempted'
    ))
) STRICT;

CREATE TABLE source_snippet (
    file TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL CHECK (start_line <= end_line),
    text TEXT NOT NULL,
    PRIMARY KEY (file, start_line, end_line)
) STRICT, WITHOUT ROWID;

CREATE TABLE fn_cpu_summary (
    function_id INTEGER NOT NULL REFERENCES function,
    event TEXT NOT NULL,
    self_total ANY NOT NULL,
    inclusive_total ANY NOT NULL,
    sample_count ANY NOT NULL,
    PRIMARY KEY (function_id, event)
) STRICT, WITHOUT ROWID;

CREATE TABLE line_cpu_summary (
    file TEXT NOT NULL,
    line INTEGER NOT NULL,
    function_id INTEGER NOT NULL REFERENCES function,
    event TEXT NOT NULL,
    self_total ANY NOT NULL,
    sample_count ANY NOT NULL,
    PRIMARY KEY (file, line, function_id, event)
) STRICT, WITHOUT ROWID;

CREATE TABLE codegen_remark (
    function_id INTEGER NOT NULL REFERENCES function,
    file TEXT NOT NULL,
    line INTEGER NOT NULL CHECK (line > 0),
    col INTEGER NOT NULL CHECK (col > 0),
    kind TEXT NOT NULL CHECK (kind IN ('bounds_check', 'overflow_check', 'step_sync')),
    outcome TEXT NOT NULL CHECK (outcome IN ('emitted', 'elided', 'coalesced')),
    evidence_level TEXT NOT NULL CHECK (evidence_level IN ('source_remark', 'verified_scope')),
    count ANY NOT NULL,
    PRIMARY KEY (file, line, function_id, col, kind, outcome, evidence_level)
) STRICT, WITHOUT ROWID;

CREATE TABLE peak_snapshot (
    id INTEGER PRIMARY KEY,
    metric TEXT NOT NULL CHECK (metric IN ('charged_bytes', 'retained_bytes', 'slack_bytes')),
    scope_id INTEGER,
    ts_ns ANY NOT NULL,
    total_bytes ANY NOT NULL,
    resident_bytes ANY
) STRICT;

CREATE TABLE peak_callsite (
    snapshot_id INTEGER NOT NULL REFERENCES peak_snapshot,
    callsite_id INTEGER NOT NULL REFERENCES callsite,
    live_bytes ANY NOT NULL,
    live_count ANY NOT NULL,
    resident_bytes ANY,
    PRIMARY KEY (snapshot_id, callsite_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE mem_alloc_agg (
    callsite_id INTEGER NOT NULL REFERENCES callsite,
    allocation_kind TEXT NOT NULL CHECK (allocation_kind IN ('block', 'task_heap', 'region', 'task_storage', 'channel', 'cause_cell')),
    observed_count ANY NOT NULL,
    observed_bytes ANY NOT NULL,
    estimated_count REAL NOT NULL,
    estimated_bytes REAL NOT NULL,
    freed_count ANY NOT NULL,
    lifetime_ns ANY NOT NULL,
    PRIMARY KEY (callsite_id, allocation_kind)
) STRICT, WITHOUT ROWID;

CREATE TABLE budget_scope_summary (
    scope_id INTEGER PRIMARY KEY,
    parent_id INTEGER,
    scope_kind TEXT NOT NULL CHECK (scope_kind IN ('bytes', 'steps', 'capacity_unattributed')),
    task_id INTEGER NOT NULL,
    callsite_id INTEGER NOT NULL REFERENCES callsite,
    enter_ts ANY NOT NULL,
    exit_ts ANY,
    limit_val ANY,
    peak_val ANY,
    refused INTEGER NOT NULL CHECK (refused IN (0, 1))
) STRICT;

CREATE INDEX cpu_sample_callsite ON cpu_sample(callsite_id);
CREATE INDEX fn_cpu_summary_event ON fn_cpu_summary(event);

CREATE TABLE task (
    id INTEGER PRIMARY KEY,
    unit_id ANY NOT NULL,
    runtime_id ANY NOT NULL,
    parent_id INTEGER REFERENCES task,
    start_ns ANY,
    end_ns ANY,
    UNIQUE (unit_id, runtime_id)
) STRICT;

CREATE TABLE task_slice (
    id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES task,
    tid INTEGER,
    worker_id INTEGER,
    state TEXT NOT NULL,
    wait_reason TEXT,
    object_id ANY,
    start_ns ANY NOT NULL,
    end_ns ANY
) STRICT;

CREATE TABLE causal_edge (
    id INTEGER PRIMARY KEY,
    src_kind TEXT NOT NULL,
    src_id INTEGER REFERENCES task,
    dst_task_id INTEGER NOT NULL REFERENCES task,
    object_id ANY,
    ts_ns ANY NOT NULL
) STRICT;

CREATE TABLE host_stack_interval (
    id INTEGER PRIMARY KEY,
    tid INTEGER NOT NULL,
    task_id INTEGER REFERENCES task,
    site ANY NOT NULL,
    start_ns ANY NOT NULL,
    end_ns ANY
) STRICT;
CREATE INDEX host_stack_at ON host_stack_interval(tid,start_ns);

CREATE TABLE io_op (
    token ANY PRIMARY KEY,
    task_id INTEGER REFERENCES task,
    operation INTEGER NOT NULL,
    site ANY NOT NULL,
    start_ns ANY NOT NULL,
    end_ns ANY,
    wait_callback_ns ANY NOT NULL DEFAULT 0,
    issue_callback_ns ANY NOT NULL DEFAULT 0,
    resume_callback_ns ANY NOT NULL DEFAULT 0,
    deliver_callback_ns ANY NOT NULL DEFAULT 0,
    abort_callback_ns ANY NOT NULL DEFAULT 0,
    callback_ns ANY NOT NULL DEFAULT 0,
    pending_ns ANY NOT NULL DEFAULT 0,
    bytes ANY,
    error_tag INTEGER,
    error_code INTEGER,
    status INTEGER
) STRICT, WITHOUT ROWID;
CREATE INDEX peak_snapshot_scope ON peak_snapshot(scope_id, metric);

CREATE INDEX task_slice_by_start ON task_slice(task_id, start_ns);
CREATE INDEX task_slice_by_end ON task_slice(task_id, end_ns);
CREATE INDEX causal_edge_by_destination ON causal_edge(dst_task_id, ts_ns);

CREATE VIEW task_wait_episode AS
WITH waits AS (
    SELECT id, task_id, state, wait_reason, object_id, start_ns, end_ns AS slice_end_ns
    FROM task_slice WHERE state IN ('waiting', 'provider')
), candidates AS (
    SELECT w.id AS episode_id, c.id, c.src_kind, c.src_id, c.ts_ns,
           count(*) OVER (PARTITION BY w.id) AS wakes,
           count(*) OVER (PARTITION BY c.id) AS episodes
    FROM waits w JOIN causal_edge c
      ON c.dst_task_id = w.task_id AND c.ts_ns BETWEEN w.start_ns AND w.slice_end_ns
     AND (w.object_id IS NULL OR c.object_id IS NULL OR w.object_id = c.object_id)
), paired AS (
    SELECT w.*, c.id AS wake_id, c.src_kind AS wake_kind, c.src_id AS source_task_id,
           c.ts_ns AS wake_ns,
           CASE
               WHEN w.slice_end_ns IS NULL THEN 'open_wait'
               WHEN w.start_ns >= w.slice_end_ns THEN 'timestamp_tie'
               WHEN coalesce((SELECT value FROM run_meta WHERE key = 'runtime_trace_complete'), 0) != 1
                 OR coalesce((SELECT value FROM run_meta WHERE key = 'runtime_events_lost'), 0) != 0
                    THEN 'incomplete_trace'
               WHEN c.id IS NULL THEN
                   CASE WHEN EXISTS (SELECT 1 FROM candidates WHERE episode_id = w.id)
                       THEN 'ambiguous_wake' ELSE 'missing_wake' END
               WHEN c.ts_ns = w.start_ns OR c.ts_ns = w.slice_end_ns THEN 'timestamp_tie'
               WHEN NOT EXISTS (
                   SELECT 1 FROM task_slice s
                   WHERE s.task_id = w.task_id AND s.state = 'ready' AND s.start_ns = w.slice_end_ns
               ) THEN 'missing_ready'
               WHEN c.src_kind <> 'task' THEN c.src_kind
               WHEN c.src_id IS NULL THEN 'missing_source'
           END AS boundary
    FROM waits w LEFT JOIN candidates c
      ON c.episode_id = w.id AND c.wakes = 1 AND c.episodes = 1
)
SELECT id, task_id, state, wait_reason, object_id, start_ns,
       CASE WHEN boundary IS NULL OR boundary IN ('timer','net','cancel','unknown','missing_source')
            THEN wake_ns ELSE slice_end_ns END AS end_ns,
       slice_end_ns, wake_id, wake_kind, source_task_id, wake_ns, boundary
FROM paired;

CREATE VIEW task_wait_critical_path AS
WITH RECURSIVE
episodes AS MATERIALIZED (SELECT * FROM task_wait_episode),
sources AS (
    SELECT e.id, min(s.id) AS slice_id
    FROM episodes e JOIN task_slice s
      ON e.boundary IS NULL AND s.task_id = e.source_task_id AND s.state = 'running'
     AND s.start_ns < e.wake_ns AND e.wake_ns < s.end_ns
    GROUP BY e.id HAVING count(*) = 1
),
predecessors AS (
    SELECT s.id, min(p.id) AS slice_id
    FROM task_slice s JOIN task_slice p
      ON p.task_id = s.task_id AND p.start_ns < s.start_ns AND p.end_ns >= s.start_ns
    GROUP BY s.id HAVING count(*) = 1
),
links AS (
    SELECT id, slice_id, 1 AS follows_wake FROM sources
    UNION ALL
    SELECT id, slice_id, 0 FROM predecessors
),
walk(root_id, root_task_id, slice_id, depth, floor_ns, start_ns, end_ns) AS (
    SELECT id, task_id, id, 0, start_ns, start_ns, end_ns
    FROM episodes WHERE start_ns < end_ns
    UNION ALL
    SELECT w.root_id, w.root_task_id, n.id, w.depth + l.follows_wake,
           CASE l.follows_wake WHEN 1 THEN w.start_ns ELSE w.floor_ns END,
           max(n.start_ns, CASE l.follows_wake WHEN 1 THEN w.start_ns ELSE w.floor_ns END),
           min(coalesce(e.end_ns, n.end_ns),
               CASE l.follows_wake WHEN 1 THEN w.end_ns ELSE w.start_ns END)
    FROM walk w JOIN links l ON l.id = w.slice_id
    JOIN task_slice n ON n.id = l.slice_id
    LEFT JOIN episodes e ON e.id = n.id
    WHERE w.start_ns < w.end_ns
      AND n.start_ns < CASE l.follows_wake WHEN 1 THEN w.end_ns ELSE w.start_ns END
      AND coalesce(e.end_ns, n.end_ns) >
          CASE l.follows_wake WHEN 1 THEN w.start_ns ELSE w.floor_ns END
      AND (l.follows_wake = 0 OR EXISTS (
           SELECT 1 FROM episodes here WHERE here.id = w.slice_id AND here.wake_ns = w.end_ns
      ))
),
points AS (
    SELECT root_id, start_ns AS point FROM walk WHERE start_ns < end_ns
    UNION
    SELECT root_id, end_ns FROM walk WHERE start_ns < end_ns
),
spans AS (
    SELECT root_id, point AS start_ns,
           lead(point) OVER (PARTITION BY root_id ORDER BY point) AS end_ns
    FROM points
),
cover AS (
    SELECT w.root_id, w.root_task_id, w.slice_id, w.depth, s.start_ns, s.end_ns,
           row_number() OVER (PARTITION BY s.root_id, s.start_ns ORDER BY w.depth DESC) AS rank
    FROM spans s JOIN walk w ON w.root_id = s.root_id
      AND w.start_ns <= s.start_ns AND s.end_ns <= w.end_ns
)
SELECT c.root_id, c.root_task_id, c.slice_id, c.depth, s.task_id, s.state,
       s.wait_reason, s.object_id, e.source_task_id, c.start_ns, c.end_ns,
       CASE
           WHEN s.state IN ('waiting', 'provider') THEN coalesce(e.boundary,
               CASE WHEN sources.id IS NULL THEN 'missing_or_ambiguous_source_interval'
                    ELSE 'missing_predecessor' END)
       END AS boundary
FROM cover c JOIN task_slice s ON s.id = c.slice_id
LEFT JOIN episodes e ON e.id = c.slice_id
LEFT JOIN sources ON sources.id = c.slice_id
WHERE rank = 1
UNION ALL
SELECT id, task_id, id, 0, task_id, state, wait_reason, object_id, source_task_id,
       start_ns, NULL, boundary
FROM episodes WHERE end_ns IS NULL;
