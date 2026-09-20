CREATE TABLE threads (
        id INTEGER PRIMARY KEY AUTOINCREMENT, client_id TEXT UNIQUE,
        kind TEXT NOT NULL CHECK(kind IN ('space','task')),
        parent_thread_id INTEGER REFERENCES threads(id), pinned_at TEXT,
        title TEXT NOT NULL,
        icon_symbol TEXT CHECK(icon_symbol IS NULL OR icon_symbol IN ('hammer','wrench','bug','flask','rocket','package','git-branch','terminal','book','file-text','lightbulb','graduation-cap','brain','search','users','home','building','globe','map-pin','wallet','receipt','calendar','clock','timer','mail','message-square','bell','megaphone','image','music','film','camera','star','heart','flag','tag','shield','key','zap','leaf','sun','moon','coffee','gift','puzzle')),
        icon_tint TEXT CHECK(icon_tint IS NULL OR icon_tint IN ('neutral','red','orange','amber','green','teal','blue','violet','pink')),
        icon_blob_id TEXT REFERENCES blobs(id),
        showcased_artifact_id INTEGER REFERENCES artifacts(id) ON DELETE SET NULL,
        description TEXT NOT NULL, instrument TEXT CHECK(instrument IS NULL OR (json_type(instrument) IN ('object','array') AND json(instrument) NOT IN ('{}','[]'))),
        attention TEXT NOT NULL CHECK(attention IN ('quiet','needs_owner')),
        settled_at TEXT, archived_at TEXT, snoozed_until TEXT, read INTEGER NOT NULL,
        created_at TEXT NOT NULL, updated_at TEXT NOT NULL, revision INTEGER NOT NULL,
        CHECK(kind = 'task' OR settled_at IS NULL),
        CHECK(parent_thread_id IS NULL OR parent_thread_id != id),
        CHECK(parent_thread_id IS NULL OR pinned_at IS NULL),
        CHECK(icon_symbol IS NULL OR icon_blob_id IS NULL),
        CHECK((icon_tint IS NULL) = (icon_symbol IS NULL)));
CREATE INDEX threads_parent ON threads(parent_thread_id,id);
CREATE TABLE thread_state (
    thread_id INTEGER PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK(COALESCE(revision > 0,0)),
    own_headline TEXT NOT NULL CHECK(COALESCE(
        length(CAST(own_headline AS BLOB)) BETWEEN 1 AND 240 AND
        own_headline=trim(own_headline) AND instr(own_headline,'  ')=0 AND
        instr(own_headline,char(9))=0 AND instr(own_headline,char(10))=0 AND
        instr(own_headline,char(11))=0 AND instr(own_headline,char(12))=0 AND instr(own_headline,char(13))=0 AND
        length(own_headline)-length(replace(own_headline,' ',''))+1<=12
    ,0)),
    headline TEXT NOT NULL CHECK(COALESCE(
        length(CAST(headline AS BLOB)) BETWEEN 1 AND 240 AND
        headline=trim(headline) AND instr(headline,'  ')=0 AND
        instr(headline,char(9))=0 AND instr(headline,char(10))=0 AND
        instr(headline,char(11))=0 AND instr(headline,char(12))=0 AND instr(headline,char(13))=0 AND
        length(headline)-length(replace(headline,' ',''))+1<=12
    ,0)),
    findings_json TEXT NOT NULL CHECK(COALESCE(
        json_valid(findings_json) AND json_type(findings_json)='array' AND
        json_array_length(findings_json)<=32 AND length(CAST(findings_json AS BLOB))<=8192
    ,0)),
    checkpoint_at TEXT,
    steering_revision INTEGER NOT NULL CHECK(COALESCE(steering_revision>=0,0))
);
CREATE TABLE thread_state_artifacts (
    thread_id INTEGER NOT NULL REFERENCES thread_state(thread_id) ON DELETE CASCADE,
    artifact_id INTEGER NOT NULL REFERENCES artifacts(id),
    PRIMARY KEY(thread_id,artifact_id)
);
CREATE INDEX thread_state_artifacts_by_artifact ON thread_state_artifacts(artifact_id,thread_id);
CREATE TABLE thread_state_changes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    state_revision INTEGER NOT NULL CHECK(COALESCE(state_revision>0,0)),
    before_json TEXT NOT NULL CHECK(COALESCE(json_valid(before_json) AND json_type(before_json)='object',0)),
    after_json TEXT NOT NULL CHECK(COALESCE(json_valid(after_json) AND json_type(after_json)='object',0)),
    actor_kind TEXT NOT NULL CHECK(COALESCE(actor_kind IN ('owner','thread','host'),0)),
    actor_thread_id INTEGER REFERENCES threads(id),
    actor_turn_id INTEGER REFERENCES thread_turns(id),
    cause TEXT NOT NULL CHECK(COALESCE(length(trim(cause)) BETWEEN 1 AND 200,0)),
    created_at TEXT NOT NULL,
    CHECK(COALESCE((actor_kind='thread')=(actor_thread_id IS NOT NULL),0)),
    CHECK(COALESCE(actor_turn_id IS NULL OR actor_kind='thread',0)),
    UNIQUE(thread_id,state_revision)
);
CREATE TRIGGER threads_create_material_state
AFTER INSERT ON threads
BEGIN
    INSERT INTO thread_state(thread_id,revision,own_headline,headline,findings_json,checkpoint_at,steering_revision)
    VALUES(NEW.id,1,CASE NEW.kind WHEN 'task' THEN 'Task ready' ELSE 'Space ready' END,
           CASE NEW.kind WHEN 'task' THEN 'Task ready' ELSE 'Space ready' END,'[]',NULL,0);
END;
CREATE TRIGGER threads_parent_immutable
BEFORE UPDATE OF parent_thread_id ON threads
WHEN NEW.parent_thread_id IS NOT OLD.parent_thread_id
BEGIN
    SELECT RAISE(ABORT, 'Thread parent is immutable');
END;
CREATE TRIGGER threads_kind_parent_insert
BEFORE INSERT ON threads
WHEN NEW.parent_thread_id IS NOT NULL
BEGIN
    SELECT CASE WHEN (SELECT kind FROM threads WHERE id=NEW.parent_thread_id)='task'
                          AND NEW.kind!='task'
        THEN RAISE(ABORT, 'Task Threads can only contain Tasks') END;
END;
CREATE TRIGGER threads_kind_update
BEFORE UPDATE OF kind ON threads
WHEN NEW.kind IS NOT OLD.kind
BEGIN
    SELECT CASE WHEN NEW.kind='space' AND OLD.settled_at IS NOT NULL
        THEN RAISE(ABORT, 'reopen a settled Task before converting it to a Space') END;
    SELECT CASE WHEN NEW.kind='space' AND
        (SELECT kind FROM threads WHERE id=OLD.parent_thread_id)='task'
        THEN RAISE(ABORT, 'a Task parent can only contain Tasks') END;
    SELECT CASE WHEN NEW.kind='task' AND EXISTS(
        SELECT 1 FROM threads WHERE parent_thread_id=OLD.id AND kind='space'
    ) THEN RAISE(ABORT, 'a Task cannot contain Spaces') END;
END;
        CREATE TABLE thread_action_receipts (client_id TEXT PRIMARY KEY, payload TEXT NOT NULL);
        CREATE TABLE thread_requests (id INTEGER PRIMARY KEY AUTOINCREMENT, client_id TEXT NOT NULL UNIQUE, payload TEXT NOT NULL,
            thread_id INTEGER GENERATED ALWAYS AS (json_extract(payload,'$.thread_id')) STORED NOT NULL REFERENCES threads(id),
            report_triggered INTEGER GENERATED ALWAYS AS (COALESCE(json_extract(payload,'$.report_triggered'),0)) STORED);
        CREATE INDEX thread_requests_fifo ON thread_requests(thread_id,id);
        CREATE TABLE thread_turns (
        id INTEGER PRIMARY KEY AUTOINCREMENT, thread_id INTEGER NOT NULL REFERENCES threads(id),
        requester_thread_id INTEGER REFERENCES threads(id),
        requester_turn_id INTEGER REFERENCES thread_turns(id),
        owner_message_id INTEGER UNIQUE, agent_message_id INTEGER, state TEXT NOT NULL CHECK(state IN ($TURN_STATES)),
        accepted_at TEXT NOT NULL, started_at TEXT, finished_at TEXT, cancel_requested_at TEXT,
        CHECK(state != 'queued' OR started_at IS NULL),
        CHECK(state != 'running' OR started_at IS NOT NULL),
        CHECK((state IN ($TERMINAL_TURN_STATES)) = (finished_at IS NOT NULL)),
        CHECK(requester_turn_id IS NULL OR requester_thread_id IS NOT NULL));
CREATE UNIQUE INDEX thread_one_running ON thread_turns(thread_id) WHERE state='running';
        CREATE TABLE thread_activities (
        id INTEGER PRIMARY KEY AUTOINCREMENT, thread_id INTEGER NOT NULL REFERENCES threads(id),
        turn_id INTEGER REFERENCES thread_turns(id), kind TEXT NOT NULL, data TEXT NOT NULL, ts TEXT NOT NULL);
        CREATE TABLE thread_activity_keys (key TEXT PRIMARY KEY, activity_id INTEGER NOT NULL REFERENCES thread_activities(id));
        CREATE INDEX thread_turns_thread ON thread_turns(thread_id,id);
CREATE TABLE thread_turn_events (turn_id INTEGER NOT NULL REFERENCES thread_turns(id), seq INTEGER NOT NULL, event TEXT NOT NULL, PRIMARY KEY(turn_id,seq));
        CREATE INDEX thread_activities_thread ON thread_activities(thread_id,id);

            CREATE TABLE chat_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                author TEXT NOT NULL,
                body TEXT NOT NULL,
                ref INTEGER NULL,
                ts TEXT NOT NULL,
                tool_calls TEXT NOT NULL DEFAULT '[]',
                thread_id INTEGER NOT NULL REFERENCES threads(id),
                mentions TEXT NOT NULL DEFAULT '[]'
            );
            CREATE TABLE client_messages (
                client_id TEXT PRIMARY KEY,
                msg_id INTEGER NOT NULL REFERENCES chat_messages(id)
            );
            CREATE TABLE message_task_focus (
                message_id INTEGER PRIMARY KEY REFERENCES chat_messages(id),
                task_thread_id INTEGER NOT NULL REFERENCES threads(id),
                snapshot_json TEXT NOT NULL CHECK(json_type(snapshot_json)='object')
            );
            CREATE TABLE blobs (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                mime TEXT NOT NULL,
                size INTEGER NOT NULL,
                created_ts TEXT NOT NULL
            );
            CREATE TABLE client_blobs (
                client_id TEXT PRIMARY KEY,
                blob_id TEXT NOT NULL REFERENCES blobs(id)
            );
            CREATE TABLE message_attachments (
                message_id INTEGER NOT NULL REFERENCES chat_messages(id),
                blob_id TEXT NOT NULL REFERENCES blobs(id),
                position INTEGER NOT NULL,
                PRIMARY KEY (message_id, position)
            );
            CREATE TABLE process_deliveries (
                delivery_key TEXT PRIMARY KEY,
                thread_id INTEGER NOT NULL REFERENCES threads(id),
                process_id TEXT NOT NULL,
                process_name TEXT NOT NULL,
                trigger_label TEXT NOT NULL,
                outcome TEXT NOT NULL,
                result TEXT NOT NULL,
                message_id INTEGER UNIQUE REFERENCES chat_messages(id),
                triage_dispatched INTEGER NOT NULL DEFAULT 0 CHECK(triage_dispatched IN (0,1))
            );

            CREATE TABLE device_tokens (
                token TEXT PRIMARY KEY,
                device_label TEXT NOT NULL,
                node_id TEXT NOT NULL,
                created_ts TEXT NOT NULL,
                last_seen_ts TEXT NOT NULL,
                revoked_ts TEXT NULL
            );
            CREATE TABLE push_tokens (
                token TEXT PRIMARY KEY,
                device_token TEXT NOT NULL REFERENCES device_tokens(token),
                platform TEXT NOT NULL CHECK(platform IN ('android','web','ios')),
                created_ts TEXT NOT NULL,
                last_seen_ts TEXT NOT NULL
            );
            CREATE TABLE meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE plugin_state (
                plugin_id TEXT PRIMARY KEY,
                enabled INTEGER NOT NULL
            );
            CREATE TABLE plugin_settings (
                plugin_id TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (plugin_id, key)
            );
            CREATE TABLE plugin_kv (
                plugin_id TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (plugin_id, key)
            );
CREATE TABLE artifacts (
        id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL,
        kind TEXT NOT NULL CHECK (kind IN ($ARTIFACT_KINDS)),
        kind_data TEXT NOT NULL, content TEXT NOT NULL,
        created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
        revision INTEGER NOT NULL DEFAULT 1 CHECK(COALESCE(revision>0,0)));
        CREATE TABLE message_artifacts (
        message_id INTEGER NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
        artifact_id INTEGER NOT NULL REFERENCES artifacts(id), PRIMARY KEY(message_id,artifact_id));
        CREATE INDEX message_artifacts_by_artifact ON message_artifacts(artifact_id,message_id);
        CREATE TABLE artifact_operations (
        operation_id TEXT PRIMARY KEY, payload TEXT NOT NULL,
        artifact_id INTEGER NOT NULL REFERENCES artifacts(id),
        message_id INTEGER REFERENCES chat_messages(id) ON DELETE SET NULL);
CREATE INDEX chat_messages_thread ON chat_messages(thread_id,id);

CREATE TABLE activity_artifacts (
    activity_id INTEGER NOT NULL REFERENCES thread_activities(id) ON DELETE CASCADE,
    artifact_id INTEGER NOT NULL REFERENCES artifacts(id),
    PRIMARY KEY(activity_id,artifact_id));
CREATE INDEX activity_artifacts_by_artifact ON activity_artifacts(artifact_id,activity_id);

CREATE TABLE thread_delegations (
    requester_turn_id INTEGER NOT NULL REFERENCES thread_turns(id), operation_id TEXT NOT NULL, payload TEXT NOT NULL,
    child_thread_id INTEGER NOT NULL REFERENCES threads(id), child_turn_id INTEGER NOT NULL REFERENCES thread_turns(id),
    PRIMARY KEY(requester_turn_id,operation_id));
CREATE TABLE thread_reports (
    child_turn_id INTEGER NOT NULL REFERENCES thread_turns(id), operation_id TEXT NOT NULL,
    activity_id INTEGER NOT NULL REFERENCES thread_activities(id),
    PRIMARY KEY(child_turn_id,operation_id));
CREATE TABLE thread_execution_bindings (
    history_id TEXT NOT NULL, session_id TEXT NOT NULL, execution_id TEXT NOT NULL,
    turn_id INTEGER NOT NULL REFERENCES thread_turns(id), revoked INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(session_id,execution_id));
CREATE TABLE thread_process_sessions (
    history_id TEXT NOT NULL, session_id TEXT PRIMARY KEY,
    thread_id INTEGER NOT NULL REFERENCES threads(id));
CREATE TABLE thread_process_authorities (
    session_id TEXT NOT NULL REFERENCES thread_process_sessions(session_id),
    process_id TEXT NOT NULL, turn_id INTEGER NOT NULL REFERENCES thread_turns(id),
    PRIMARY KEY(session_id,process_id));
CREATE TABLE thread_mutation_receipts (
    turn_id INTEGER NOT NULL REFERENCES thread_turns(id),operation_id TEXT NOT NULL,payload TEXT NOT NULL,result TEXT NOT NULL,
    PRIMARY KEY(turn_id,operation_id));
CREATE TABLE thread_effect_receipts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    turn_id INTEGER NOT NULL REFERENCES thread_turns(id),
    operation_id TEXT NOT NULL,
    effect_index INTEGER NOT NULL CHECK(effect_index >= 0),
    tool TEXT NOT NULL,
    effect TEXT NOT NULL CHECK(effect IN ('created','sent_to','delegated','read','edited','refused')),
    target_json TEXT NOT NULL CHECK(COALESCE(
        json_type(target_json)='object' AND
        ((json_extract(target_json,'$.kind')='thread' AND json_type(target_json,'$.thread_id')='integer' AND json_extract(target_json,'$.thread_id')>0 AND json_remove(target_json,'$.kind','$.thread_id')='{}') OR
         (json_extract(target_json,'$.kind')='artifact' AND json_type(target_json,'$.artifact_id')='integer' AND json_extract(target_json,'$.artifact_id')>0 AND json_remove(target_json,'$.kind','$.artifact_id')='{}') OR
         (json_extract(target_json,'$.kind')='root' AND json_remove(target_json,'$.kind')='{}'))
    ,0)),
    target_turn_id INTEGER REFERENCES thread_turns(id),
    request_client_id TEXT,
    refusal_json TEXT CHECK(COALESCE(
        refusal_json IS NULL OR
        (json_type(refusal_json)='object' AND
         json_type(refusal_json,'$.reason')='text' AND
         json_type(refusal_json,'$.grant_summary')='text' AND
         json_type(refusal_json,'$.detail')='text' AND
         json_remove(refusal_json,'$.reason','$.grant_summary','$.detail')='{}')
    ,0)),
    created_at TEXT NOT NULL,
    CHECK((effect='refused')=(refusal_json IS NOT NULL)),
    UNIQUE(turn_id,operation_id,effect_index)
);
CREATE INDEX thread_effect_receipts_target_turn ON thread_effect_receipts(target_turn_id);
CREATE TABLE thread_execution_preferences (thread_id INTEGER PRIMARY KEY REFERENCES threads(id),config TEXT NOT NULL);
CREATE TABLE thread_turn_execution (turn_id INTEGER PRIMARY KEY REFERENCES thread_turns(id),config TEXT NOT NULL);
CREATE TABLE plugin_thread_kv (
    plugin_id TEXT NOT NULL,thread_id INTEGER NOT NULL REFERENCES threads(id),key TEXT NOT NULL,value TEXT NOT NULL,
    PRIMARY KEY(plugin_id,thread_id,key));

CREATE TABLE turn_output_artifacts (turn_id INTEGER NOT NULL REFERENCES thread_turns(id),artifact_id INTEGER NOT NULL REFERENCES artifacts(id),PRIMARY KEY(turn_id,artifact_id));


CREATE TABLE thread_related_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    url TEXT,
    target_thread_id INTEGER REFERENCES threads(id) ON DELETE CASCADE,
    title TEXT,
    created_at TEXT NOT NULL,
    CHECK((url IS NOT NULL AND target_thread_id IS NULL) OR
          (url IS NULL AND target_thread_id IS NOT NULL AND title IS NULL)),
    UNIQUE(thread_id,url),
    UNIQUE(thread_id,target_thread_id)
);
CREATE TABLE thread_related_receipts (
    client_id TEXT PRIMARY KEY,
    payload TEXT NOT NULL
);

CREATE TABLE thread_grants (
    thread_id INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    -- NULL is the root: every Thread in the history, including Threads made
    -- after the grant. A named Thread carries its subtree, as it always did.
    target_thread_id INTEGER REFERENCES threads(id) ON DELETE CASCADE,
    -- One grant per target per Thread, root included: a NULL target would not
    -- collide under a plain key, so identity is the coalesced key instead.
    target_key INTEGER NOT NULL GENERATED ALWAYS AS (COALESCE(target_thread_id,0)) VIRTUAL,
    granted_by TEXT NOT NULL CHECK(granted_by IN ('owner','thread')),
    granted_by_thread_id INTEGER REFERENCES threads(id) ON DELETE CASCADE,
    granted_at TEXT NOT NULL,
    note TEXT,
    CHECK(thread_id != target_thread_id),
    CHECK((granted_by='thread') = (granted_by_thread_id IS NOT NULL))
);
CREATE UNIQUE INDEX thread_grants_key ON thread_grants(thread_id,target_key);
CREATE INDEX thread_grants_target ON thread_grants(target_thread_id,thread_id);
CREATE TABLE thread_grant_receipts (
    client_id TEXT PRIMARY KEY,
    payload TEXT NOT NULL
);
