PRAGMA application_id = 0x48505246;
PRAGMA user_version = 2;

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
    cpu INTEGER NOT NULL,
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
    total_bytes ANY NOT NULL
) STRICT;

CREATE TABLE peak_callsite (
    snapshot_id INTEGER NOT NULL REFERENCES peak_snapshot,
    callsite_id INTEGER NOT NULL REFERENCES callsite,
    live_bytes ANY NOT NULL,
    live_count ANY NOT NULL,
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
