CREATE TABLE threads (
        id INTEGER PRIMARY KEY AUTOINCREMENT, client_id TEXT UNIQUE,
        parent_thread_id INTEGER REFERENCES threads(id), pinned_at TEXT,
        title TEXT NOT NULL, icon TEXT,
        showcased_artifact_id INTEGER REFERENCES artifacts(id) ON DELETE SET NULL,
        description TEXT NOT NULL, instrument TEXT NOT NULL,
        attention TEXT NOT NULL CHECK(attention IN ('quiet','needs_owner')),
        settled_at TEXT, archived_at TEXT, snoozed_until TEXT, read INTEGER NOT NULL,
        created_at TEXT NOT NULL, updated_at TEXT NOT NULL, revision INTEGER NOT NULL,
        CHECK(parent_thread_id IS NULL OR parent_thread_id != id),
        CHECK(parent_thread_id IS NULL OR pinned_at IS NULL));
CREATE INDEX threads_parent ON threads(parent_thread_id,id);
        CREATE TABLE thread_action_receipts (client_id TEXT PRIMARY KEY, payload TEXT NOT NULL);
        CREATE TABLE thread_requests (id INTEGER PRIMARY KEY AUTOINCREMENT, client_id TEXT NOT NULL UNIQUE, payload TEXT NOT NULL,
            thread_id INTEGER GENERATED ALWAYS AS (json_extract(payload,'$.thread_id')) STORED NOT NULL REFERENCES threads(id),
            report_triggered INTEGER GENERATED ALWAYS AS (COALESCE(json_extract(payload,'$.report_triggered'),0)) STORED);
        CREATE INDEX thread_requests_fifo ON thread_requests(thread_id,id);
        CREATE TABLE thread_turns (
        id INTEGER PRIMARY KEY AUTOINCREMENT, thread_id INTEGER NOT NULL REFERENCES threads(id),
        requester_thread_id INTEGER REFERENCES threads(id),
        requester_turn_id INTEGER REFERENCES thread_turns(id),
        owner_message_id INTEGER UNIQUE, agent_message_id INTEGER, state TEXT NOT NULL,
        started_at TEXT NOT NULL, finished_at TEXT,
        CHECK(requester_turn_id IS NULL OR requester_thread_id IS NOT NULL));
CREATE UNIQUE INDEX thread_one_running ON thread_turns(thread_id) WHERE state='running';
        CREATE TABLE thread_activities (
        id INTEGER PRIMARY KEY AUTOINCREMENT, thread_id INTEGER NOT NULL REFERENCES threads(id),
        turn_id INTEGER REFERENCES thread_turns(id), kind TEXT NOT NULL, data TEXT NOT NULL, ts TEXT NOT NULL);
        CREATE TABLE thread_activity_keys (key TEXT PRIMARY KEY, activity_id INTEGER NOT NULL REFERENCES thread_activities(id));
        CREATE INDEX thread_turns_thread ON thread_turns(thread_id,id);
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

            CREATE TABLE push_tokens (
                token TEXT PRIMARY KEY,
                platform TEXT NOT NULL,
                created_ts TEXT NOT NULL,
                last_seen_ts TEXT NOT NULL
            );
            CREATE TABLE device_tokens (
                token TEXT PRIMARY KEY,
                device_label TEXT NOT NULL,
                node_id TEXT NOT NULL,
                created_ts TEXT NOT NULL,
                last_seen_ts TEXT NOT NULL,
                revoked_ts TEXT NULL
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
        id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, kind TEXT NOT NULL,
        mime TEXT NOT NULL, filename TEXT, content TEXT NOT NULL,
        created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
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
    report_seq INTEGER NOT NULL, payload TEXT NOT NULL, activity_id INTEGER NOT NULL REFERENCES thread_activities(id),
    PRIMARY KEY(child_turn_id,operation_id), UNIQUE(child_turn_id,report_seq));
CREATE TABLE thread_execution_bindings (
    history_id TEXT NOT NULL, session_id TEXT NOT NULL, execution_id TEXT NOT NULL,
    turn_id INTEGER NOT NULL REFERENCES thread_turns(id), revoked INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(session_id,execution_id));
CREATE TABLE thread_mutation_receipts (
    turn_id INTEGER NOT NULL REFERENCES thread_turns(id),operation_id TEXT NOT NULL,payload TEXT NOT NULL,result TEXT NOT NULL,
    PRIMARY KEY(turn_id,operation_id));
CREATE TABLE thread_execution_preferences (thread_id INTEGER PRIMARY KEY REFERENCES threads(id),config TEXT NOT NULL);
CREATE TABLE thread_turn_execution (turn_id INTEGER PRIMARY KEY REFERENCES thread_turns(id),config TEXT NOT NULL);
CREATE TABLE plugin_thread_kv (
    plugin_id TEXT NOT NULL,thread_id INTEGER NOT NULL REFERENCES threads(id),key TEXT NOT NULL,value TEXT NOT NULL,
    PRIMARY KEY(plugin_id,thread_id,key));

CREATE TABLE turn_output_artifacts (turn_id INTEGER NOT NULL REFERENCES thread_turns(id),artifact_id INTEGER NOT NULL REFERENCES artifacts(id),PRIMARY KEY(turn_id,artifact_id));

CREATE TABLE thread_cancellations (turn_id INTEGER PRIMARY KEY REFERENCES thread_turns(id));

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
