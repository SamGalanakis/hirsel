PRAGMA application_id = 0x48505246;
PRAGMA user_version = 1;

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
    task_id INTEGER,
    cpu INTEGER NOT NULL,
    callsite_id INTEGER NOT NULL REFERENCES callsite,
    raw_pc_hex TEXT NOT NULL,
    period ANY NOT NULL,
    stack_root TEXT NOT NULL CHECK (stack_root IN ('fiber', 'host', 'truncated', 'failed')),
    unwind_status TEXT NOT NULL CHECK (unwind_status IN (
        'ok', 'fiber_root', 'truncated_stack', 'missing_cfi', 'unknown_mapping', 'eval_error'
    ))
) STRICT;

CREATE TABLE sample_counter (
    sample_id INTEGER NOT NULL REFERENCES cpu_sample,
    event TEXT NOT NULL,
    value ANY NOT NULL,
    PRIMARY KEY (sample_id, event)
) STRICT, WITHOUT ROWID;

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

CREATE INDEX cpu_sample_callsite ON cpu_sample(callsite_id);
CREATE INDEX fn_cpu_summary_event ON fn_cpu_summary(event);
