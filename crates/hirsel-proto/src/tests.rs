//! Wire-contract round-trip tests for the protocol frames.

use crate::*;
use chrono::{TimeZone, Utc};
use serde_json::json;

#[test]
fn client_hello_round_trips_with_nullable_last_seen() {
    let value = json!({
        "type": "hello",
        "auth": { "static_token": "secret" },
        "last_seen_msg_id": null
    });

    let parsed: ClientToHost = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        parsed,
        ClientToHost::Hello {
            auth: HelloAuth::StaticToken("secret".to_string()),
            last_seen_msg_id: None,
        }
    );
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
}

#[test]
fn legacy_bare_token_hello_decodes_as_static_auth() {
    let parsed: ClientToHost = serde_json::from_value(json!({
        "type": "hello",
        "token": "secret",
        "last_seen_msg_id": null
    }))
    .unwrap();
    assert_eq!(
        parsed,
        ClientToHost::Hello {
            auth: HelloAuth::StaticToken("secret".to_string()),
            last_seen_msg_id: None,
        }
    );
}

#[test]
fn pairing_auth_and_paired_response_round_trip() {
    let hello = ClientToHost::Hello {
        auth: HelloAuth::PairingCode {
            code: "pairing-code".to_string(),
            device_label: "Owner phone".to_string(),
        },
        last_seen_msg_id: Some(42),
    };
    let encoded = serde_json::to_value(&hello).unwrap();
    assert_eq!(encoded["auth"]["pairing_code"]["code"], "pairing-code");
    assert_eq!(
        serde_json::from_value::<ClientToHost>(encoded).unwrap(),
        hello
    );

    let paired = HostToClient::Paired {
        device_token: "device-token".to_string(),
    };
    let encoded = serde_json::to_value(&paired).unwrap();
    assert_eq!(encoded["type"], "paired");
    assert_eq!(
        serde_json::from_value::<HostToClient>(encoded).unwrap(),
        paired
    );
}

#[test]
fn send_message_keeps_ref_field_name() {
    let value = json!({
        "type": "send_message",
        "client_id": "client-1",
        "body": "hello",
        "ref": 42,
        "attachments": ["blob-1"]
    });

    let parsed: ClientToHost = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        parsed,
        ClientToHost::SendMessage {
            client_id: "client-1".to_string(),
            body: "hello".to_string(),
            r#ref: Some(42),
            attachments: vec!["blob-1".to_string()],
            mode: SendMode::Send,
            sc: None,
            mentions: Vec::new(),
        }
    );
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
}

#[test]
fn send_message_without_attachments_deserializes_as_empty() {
    let value = json!({
        "type": "send_message",
        "client_id": "client-1",
        "body": "hello",
        "ref": null
    });

    let parsed: ClientToHost = serde_json::from_value(value).unwrap();
    assert_eq!(
        parsed,
        ClientToHost::SendMessage {
            client_id: "client-1".to_string(),
            body: "hello".to_string(),
            r#ref: None,
            attachments: Vec::new(),
            mode: SendMode::Send,
            sc: None,
            mentions: Vec::new(),
        }
    );
}

#[test]
fn send_message_mode_next_turn_round_trips() {
    let value = json!({
        "type": "send_message",
        "client_id": "client-1",
        "body": "hello",
        "ref": null,
        "attachments": [],
        "mode": "next_turn"
    });

    let parsed: ClientToHost = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        parsed,
        ClientToHost::SendMessage {
            client_id: "client-1".to_string(),
            body: "hello".to_string(),
            r#ref: None,
            attachments: Vec::new(),
            mode: SendMode::NextTurn,
            sc: None,
            mentions: Vec::new(),
        }
    );
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
}

#[test]
fn send_message_mentions_round_trip() {
    let value = json!({
        "type": "send_message",
        "client_id": "client-mention",
        "body": "status?",
        "ref": null,
        "attachments": [],
        "mentions": [3, 7]
    });

    let parsed: ClientToHost = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        parsed,
        ClientToHost::SendMessage {
            client_id: "client-mention".to_string(),
            body: "status?".to_string(),
            r#ref: None,
            attachments: Vec::new(),
            mode: SendMode::Send,
            sc: None,
            mentions: vec![3, 7],
        }
    );
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
}

#[test]
fn fetch_messages_and_correlated_page_round_trip() {
    let request = ClientToHost::FetchMessages {
        client_id: "history-1".to_string(),
        before_id: 201,
        limit: 100,
    };
    let encoded = serde_json::to_value(&request).unwrap();
    assert_eq!(
        encoded,
        json!({
            "type": "fetch_messages",
            "client_id": "history-1",
            "before_id": 201,
            "limit": 100
        })
    );
    assert_eq!(
        serde_json::from_value::<ClientToHost>(encoded).unwrap(),
        request
    );

    let response = HostToClient::Messages {
        client_id: "history-1".to_string(),
        before_id: 201,
        messages: Vec::new(),
        has_more: false,
    };
    let encoded = serde_json::to_value(&response).unwrap();
    assert_eq!(encoded["type"], "messages");
    assert_eq!(encoded["client_id"], "history-1");
    assert_eq!(encoded["before_id"], 201);
    assert_eq!(encoded["has_more"], false);
    assert_eq!(
        serde_json::from_value::<HostToClient>(encoded).unwrap(),
        response
    );
}

#[test]
fn cancel_frames_round_trip() {
    let cancel_turn = ClientToHost::CancelTurn { sc: None };
    let encoded = serde_json::to_string(&cancel_turn).unwrap();
    assert_eq!(encoded, r#"{"type":"cancel_turn"}"#);
    let decoded: ClientToHost = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, cancel_turn);

    let cancel_queued = ClientToHost::CancelQueued {
        client_id: "client-1".to_string(),
    };
    let encoded = serde_json::to_string(&cancel_queued).unwrap();
    let decoded: ClientToHost = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, cancel_queued);
}

#[test]
fn upload_blob_and_blob_ok_round_trip() {
    let upload = ClientToHost::UploadBlob {
        client_id: "upload-1".to_string(),
        name: "tiny.png".to_string(),
        mime: "image/png".to_string(),
        data_b64: "iVBORw0KGgo=".to_string(),
    };
    let encoded = serde_json::to_string(&upload).unwrap();
    let decoded: ClientToHost = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, upload);

    let response = HostToClient::BlobOk {
        client_id: "upload-1".to_string(),
        blob: Blob {
            id: "blob-1".to_string(),
            name: "tiny.png".to_string(),
            mime: "image/png".to_string(),
            size: 8,
        },
    };
    let encoded = serde_json::to_string(&response).unwrap();
    let decoded: HostToClient = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, response);

    let request = ClientToHost::GetBlobUrl {
        client_id: "url-1".to_string(),
        blob_id: "blob-1".to_string(),
    };
    let encoded = serde_json::to_string(&request).unwrap();
    assert_eq!(
        serde_json::from_str::<ClientToHost>(&encoded).unwrap(),
        request
    );
    let response = HostToClient::BlobUrl {
        client_id: "url-1".to_string(),
        blob_id: "blob-1".to_string(),
        url: "/blob/blob-1?exp=300&sig=signed".to_string(),
        expires_at: 300,
    };
    let encoded = serde_json::to_string(&response).unwrap();
    assert_eq!(
        serde_json::from_str::<HostToClient>(&encoded).unwrap(),
        response
    );
}

#[test]
fn read_ping_round_trips() {
    let value = json!({
        "type": "read_ping",
        "ping_id": 9
    });

    let parsed: ClientToHost = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(parsed, ClientToHost::ReadPing { ping_id: 9 });
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
}

#[test]
fn reopen_ping_round_trips() {
    let value = json!({
        "type": "reopen_ping",
        "ping_id": 1
    });

    let parsed: ClientToHost = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(parsed, ClientToHost::ReopenPing { ping_id: 1 });
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
}

#[test]
fn event_action_round_trips_with_authoritative_wire_tag() {
    let value = json!({
        "type": "event_action",
        "event_id": 7,
        "action": "choose",
        "data": { "choice": "A", "record_rule": "Prefer explicit wire ops" }
    });

    let parsed: ClientToHost = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        parsed,
        ClientToHost::EventAction {
            event_id: 7,
            action: "choose".to_string(),
            data: json!({
                "choice": "A",
                "record_rule": "Prefer explicit wire ops"
            }),
        }
    );
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
}

#[test]
fn event_and_event_upsert_round_trip_with_authoritative_shape() {
    let event = Event {
        id: 3,
        kind: EventKind::Summary,
        source: EventSource {
            kind: EventSourceKind::Scheduled,
            r#ref: Some("morning-digest".to_string()),
        },
        name: "digest".to_string(),
        description: "Morning digest".to_string(),
        ui: json!({
            "type": "card",
            "children": [{ "type": "status", "state": "success", "label": "ready" }]
        }),
        requires_response: false,
        quick_replies: Vec::new(),
        status: EventStatus::Open,
        read: false,
        archived: true,
        snoozed_until: Some(Utc.with_ymd_and_hms(2026, 7, 13, 8, 0, 0).unwrap()),
        archived_at: Some(Utc.with_ymd_and_hms(2026, 7, 13, 9, 0, 0).unwrap()),
        fork_sc: Some("side:abc".to_string()),
        anchor: 1,
        ts: Utc.with_ymd_and_hms(2026, 7, 13, 7, 0, 0).unwrap(),
    };
    let frame = HostToClient::EventUpsert {
        event: event.clone(),
    };
    let encoded = serde_json::to_value(&frame).unwrap();
    assert_eq!(encoded["type"], "event_upsert");
    assert_eq!(encoded["event"]["kind"], "summary");
    assert_eq!(encoded["event"]["source"]["kind"], "scheduled");
    assert_eq!(encoded["event"]["ui"], event.ui);
    assert_eq!(encoded["event"]["archived"], true);
    assert_eq!(encoded["event"]["snoozed_until"], "2026-07-13T08:00:00Z");
    assert_eq!(encoded["event"]["archived_at"], "2026-07-13T09:00:00Z");
    assert_eq!(encoded["event"]["fork_sc"], "side:abc");
    assert_eq!(
        serde_json::from_value::<HostToClient>(encoded).unwrap(),
        frame
    );

    let mut legacy = serde_json::to_value(event).unwrap();
    legacy.as_object_mut().unwrap().remove("archived");
    legacy.as_object_mut().unwrap().remove("snoozed_until");
    legacy.as_object_mut().unwrap().remove("archived_at");
    let legacy = serde_json::from_value::<Event>(legacy).unwrap();
    assert!(!legacy.archived);
    assert_eq!(legacy.snoozed_until, None);
    assert_eq!(legacy.archived_at, None);
}

#[test]
fn clear_finished_events_round_trips_with_authoritative_wire_tag() {
    let value = json!({ "type": "clear_finished_events" });
    let parsed: ClientToHost = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(parsed, ClientToHost::ClearFinishedEvents {});
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
}

#[test]
fn view_frames_round_trip_with_resolved_specs_and_event_data() {
    let spec = json!({
        "type": "action",
        "label": "Approve",
        "action": "approve"
    });
    let upsert = HostToClient::ViewUpsert {
        instance_id: "view-1".to_string(),
        placement: "ping:7".to_string(),
        spec: spec.clone(),
    };
    let encoded = serde_json::to_value(&upsert).unwrap();
    assert_eq!(encoded["type"], "view_upsert");
    assert_eq!(encoded["spec"], spec);
    assert_eq!(
        serde_json::from_value::<HostToClient>(encoded).unwrap(),
        upsert
    );

    let event = ClientToHost::ViewEvent {
        instance_id: "view-1".to_string(),
        action: "approve".to_string(),
        data: json!({ "value": true }),
    };
    let encoded = serde_json::to_value(&event).unwrap();
    assert_eq!(encoded["type"], "view_event");
    assert_eq!(
        serde_json::from_value::<ClientToHost>(encoded).unwrap(),
        event
    );

    let removed = HostToClient::ViewRemoved {
        instance_id: "view-1".to_string(),
    };
    let encoded = serde_json::to_value(&removed).unwrap();
    assert_eq!(encoded["type"], "view_removed");
    assert_eq!(
        serde_json::from_value::<HostToClient>(encoded).unwrap(),
        removed
    );
}

#[test]
fn push_token_frames_round_trip() {
    let register = json!({
        "type": "register_push_token",
        "platform": "android",
        "token": "fcm-token"
    });
    let parsed: ClientToHost = serde_json::from_value(register.clone()).unwrap();
    assert_eq!(
        parsed,
        ClientToHost::RegisterPushToken {
            platform: PushPlatform::Android,
            token: "fcm-token".to_string(),
        }
    );
    assert_eq!(serde_json::to_value(parsed).unwrap(), register);

    let unregister = json!({
        "type": "unregister_push_token",
        "token": "fcm-token"
    });
    let parsed: ClientToHost = serde_json::from_value(unregister.clone()).unwrap();
    assert_eq!(
        parsed,
        ClientToHost::UnregisterPushToken {
            token: "fcm-token".to_string(),
        }
    );
    assert_eq!(serde_json::to_value(parsed).unwrap(), unregister);
}

#[test]
fn msg_removed_round_trips() {
    let response = HostToClient::MsgRemoved { id: 42 };
    let encoded = serde_json::to_string(&response).unwrap();
    assert_eq!(encoded, r#"{"type":"msg_removed","id":42}"#);
    let decoded: HostToClient = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, response);
}

#[test]
fn hello_ok_round_trips_chat_and_pings() {
    let ts = Utc.with_ymd_and_hms(2026, 7, 8, 12, 0, 0).unwrap();
    let message = ChatMessage {
        id: 1,
        author: ChatAuthor::Agent,
        body: "pong".to_string(),
        r#ref: None,
        ts,
        attachments: vec![Blob {
            id: "blob-1".to_string(),
            name: "tiny.png".to_string(),
            mime: "image/png".to_string(),
            size: 8,
        }],
        tool_calls: vec![ToolCallSummary {
            name: "shell_run".to_string(),
            ok: true,
        }],
    };
    let ping = Ping {
        id: 9,
        kind: EventKind::Judgment,
        source: EventSource {
            kind: EventSourceKind::Agent,
            r#ref: None,
        },
        name: "release-ready".to_string(),
        description: "Release is ready to ship".to_string(),
        ui: json!({
            "type": "card",
            "children": [{ "type": "text", "text": "Done" }]
        }),
        anchor: 1,
        requires_response: true,
        quick_replies: vec![QuickReply {
            value: "ship".to_string(),
            label: "Ship it".to_string(),
        }],
        status: PingStatus::Open,
        read: true,
        archived: false,
        snoozed_until: None,
        archived_at: None,
        fork_sc: None,
        ts,
    };
    let process = ProcessInfo {
        id: "proc-1".to_string(),
        kind: ProcessKind::Subagent,
        label: "fix bug".to_string(),
        agent: Some("claude".to_string()),
        model: None,
        state: ProcessState::Running,
        started_ts: ts,
        last_event_ts: ts,
        summary: Some("working".to_string()),
    };
    let response = HostToClient::HelloOk {
        latest_msg_id: 1,
        messages: vec![message],
        events: vec![ping],
        processes: vec![process],
        side_chats: Vec::new(),
        host_version: "0.1.0 (test)".to_string(),
        model: Some(ModelSnapshot {
            current: ModelSelection {
                id: "gpt-5.6-sol".to_string(),
                variant: "high".to_string(),
            },
            available: vec![AvailableModel {
                id: "gpt-5.6-sol".to_string(),
                label: "GPT-5.6 Sol".to_string(),
                variants: vec!["low".to_string(), "medium".to_string(), "high".to_string()],
                default_variant: "medium".to_string(),
            }],
            provider_id: Some("codex".to_string()),
            free_text_model: false,
        }),
        subagent_models: Some(SubagentModelCatalog {
            providers: vec![SubagentProviderModels {
                provider: "codex".to_string(),
                label: "Codex CLI".to_string(),
                models: vec![SubagentModel {
                    id: "gpt-5.6-terra".to_string(),
                    label: "Terra".to_string(),
                    variants: vec!["low".to_string(), "medium".to_string(), "high".to_string()],
                    enabled_variants: vec!["low".to_string(), "high".to_string()],
                    enabled: true,
                }],
            }],
        }),
        prompts: Some(PromptSnapshot {
            agent: PromptDoc {
                text: "You are hirsel.".to_string(),
                is_default: true,
            },
            fork: Some(ForkAgentConfig {
                current: ModelSelection {
                    id: "gpt-5.6-luna".to_string(),
                    variant: "medium".to_string(),
                },
                available: vec![AvailableModel {
                    id: "gpt-5.6-luna".to_string(),
                    label: "GPT-5.6 Luna".to_string(),
                    variants: vec!["low".to_string(), "medium".to_string(), "high".to_string()],
                    default_variant: "medium".to_string(),
                }],
                prompt: PromptDoc {
                    text: "Triage the wake.".to_string(),
                    is_default: false,
                },
                provider_id: Some("codex".to_string()),
                free_text_model: false,
            }),
        }),
        providers: Some(ProviderRoster {
            instances: vec![ProviderInstance {
                id: "codex".to_string(),
                kind: ProviderKind::Codex,
                label: "Codex".to_string(),
                base_url: None,
                api_key: MaskedSecret::default(),
                default_model: "gpt-5.6-sol".to_string(),
                detection: Some(DetectionStatus {
                    detected: true,
                    path: "/home/owner/.codex/auth.json".to_string(),
                    account_hint: Some("acct-1".to_string()),
                    detail: None,
                }),
                agent_selectable: true,
                removable: false,
            }],
            booted_provider_id: Some("codex".to_string()),
        }),
        views: Vec::new(),
    };

    let encoded = serde_json::to_string(&response).unwrap();
    let decoded: HostToClient = serde_json::from_str(&encoded).unwrap();

    assert_eq!(decoded, response);
}

#[test]
fn chat_message_without_attachments_deserializes_as_empty() {
    let value = json!({
        "id": 1,
        "author": "owner",
        "body": "old row",
        "ref": null,
        "ts": "2026-07-08T12:00:00Z"
    });

    let parsed: ChatMessage = serde_json::from_value(value).unwrap();
    assert!(parsed.attachments.is_empty());
    assert!(parsed.tool_calls.is_empty());
}

#[test]
fn process_upsert_round_trips() {
    let ts = Utc.with_ymd_and_hms(2026, 7, 9, 12, 0, 0).unwrap();
    let process = ProcessInfo {
        id: "proc-1".to_string(),
        kind: ProcessKind::Monitor,
        label: "watch file".to_string(),
        agent: None,
        model: None,
        state: ProcessState::Done,
        started_ts: ts,
        last_event_ts: ts,
        summary: None,
    };
    let upsert = HostToClient::ProcessUpsert {
        process: process.clone(),
    };
    let encoded = serde_json::to_string(&upsert).unwrap();
    assert_eq!(
        encoded,
        r#"{"type":"process_upsert","process":{"id":"proc-1","kind":"monitor","label":"watch file","agent":null,"model":null,"state":"done","started_ts":"2026-07-09T12:00:00Z","last_event_ts":"2026-07-09T12:00:00Z","summary":null}}"#
    );
    let decoded: HostToClient = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, upsert);
}

#[test]
fn turn_event_prose_round_trips() {
    let event = TurnEvent {
        seq: 1,
        event: TurnEventKind::Prose {
            text: "I will check that now.".to_string(),
        },
    };
    let encoded = serde_json::to_string(&HostToClient::TurnEvent {
        seq: event.seq,
        event: event.event.clone(),
        sc: None,
    })
    .unwrap();
    assert_eq!(
        encoded,
        r#"{"type":"turn_event","seq":1,"event":{"kind":"prose","text":"I will check that now."}}"#
    );
    let decoded: HostToClient = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        decoded,
        HostToClient::TurnEvent {
            seq: 1,
            event: event.event,
            sc: None,
        }
    );
}

#[test]
fn turn_event_tool_start_round_trips() {
    let event = HostToClient::TurnEvent {
        seq: 2,
        event: TurnEventKind::ToolStart {
            id: "call-1".to_string(),
            name: "shell_run".to_string(),
            summary: Some("cmd: true".to_string()),
        },
        sc: None,
    };

    let encoded = serde_json::to_string(&event).unwrap();
    assert_eq!(
        encoded,
        r#"{"type":"turn_event","seq":2,"event":{"kind":"tool_start","id":"call-1","name":"shell_run","summary":"cmd: true"}}"#
    );
    let decoded: HostToClient = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, event);
}

#[test]
fn turn_event_tool_done_round_trips() {
    let event = HostToClient::TurnEvent {
        seq: 3,
        event: TurnEventKind::ToolDone {
            id: "call-1".to_string(),
            name: "shell_run".to_string(),
            ok: true,
            summary: Some("ok status 0".to_string()),
        },
        sc: None,
    };

    let encoded = serde_json::to_string(&event).unwrap();
    assert_eq!(
        encoded,
        r#"{"type":"turn_event","seq":3,"event":{"kind":"tool_done","id":"call-1","name":"shell_run","ok":true,"summary":"ok status 0"}}"#
    );
    let decoded: HostToClient = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, event);
}

#[test]
fn event_without_read_deserializes_as_unread() {
    let value = json!({
        "id": 9,
        "kind": "judgment",
        "source": { "kind": "agent", "ref": null },
        "name": "old-ping",
        "description": "Old ping",
        "ui": { "type": "card", "children": [] },
        "anchor": 1,
        "requires_response": true,
        "quick_replies": [],
        "status": "open",
        "ts": "2026-07-08T12:00:00Z"
    });

    let parsed: Ping = serde_json::from_value(value).unwrap();
    assert!(!parsed.read);
}

#[test]
fn side_chat_client_frames_round_trip() {
    let frames = [
        ClientToHost::OpenSideChat {
            client_id: "open-1".to_string(),
            event_id: Some(9),
            ping_id: None,
        },
        ClientToHost::ConcludeSideChat {
            sc: "side:abc".to_string(),
        },
        ClientToHost::ConfirmConclusion {
            sc: "side:abc".to_string(),
            text: "Ship it".to_string(),
        },
        ClientToHost::DiscardSideChat {
            sc: "side:abc".to_string(),
        },
        ClientToHost::CancelTurn {
            sc: Some("side:abc".to_string()),
        },
    ];

    for frame in frames {
        let encoded = serde_json::to_value(&frame).unwrap();
        let decoded: ClientToHost = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, frame);
    }

    let legacy: ClientToHost = serde_json::from_value(json!({
        "type": "open_side_chat",
        "client_id": "legacy-open",
        "ping_id": 9
    }))
    .unwrap();
    assert_eq!(
        legacy,
        ClientToHost::OpenSideChat {
            client_id: "legacy-open".to_string(),
            event_id: None,
            ping_id: Some(9),
        }
    );
}

#[test]
fn side_chat_host_frames_round_trip() {
    let event = Event {
        id: 9,
        kind: EventKind::Info,
        source: EventSource {
            kind: EventSourceKind::Agent,
            r#ref: None,
        },
        name: "build-ready".to_string(),
        description: "Build completed".to_string(),
        ui: json!({ "type": "card", "children": [] }),
        anchor: 1,
        requires_response: false,
        quick_replies: Vec::new(),
        status: EventStatus::Open,
        read: false,
        archived: false,
        snoozed_until: None,
        archived_at: None,
        fork_sc: Some("side:abc".to_string()),
        ts: Utc.with_ymd_and_hms(2026, 7, 14, 9, 0, 0).unwrap(),
    };
    let frames = [
        HostToClient::SideChatOpen {
            sc: "side:abc".to_string(),
            event_id: 9,
            ping_id: 9,
            event,
            messages: Vec::new(),
        },
        HostToClient::ConclusionDraft {
            sc: "side:abc".to_string(),
            text: "Ship it".to_string(),
        },
        HostToClient::SideChatClosed {
            sc: "side:abc".to_string(),
        },
    ];

    for frame in frames {
        let encoded = serde_json::to_value(&frame).unwrap();
        let decoded: HostToClient = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, frame);
    }
}

#[test]
fn v1_send_message_defaults_new_fields() {
    let old = json!({
        "type": "send_message",
        "client_id": "c1",
        "body": "hi",
        "ref": null
    });
    let parsed: ClientToHost = serde_json::from_value(old).unwrap();
    assert_eq!(
        parsed,
        ClientToHost::SendMessage {
            client_id: "c1".to_string(),
            body: "hi".to_string(),
            r#ref: None,
            attachments: Vec::new(),
            mode: SendMode::Send,
            sc: None,
            mentions: Vec::new(),
        }
    );
}

#[test]
fn hello_ok_defaults_side_chats() {
    let old = json!({
        "type": "hello_ok",
        "latest_msg_id": 0,
        "messages": [],
        "events": [],
        "processes": []
    });
    let parsed: HostToClient = serde_json::from_value(old).unwrap();
    assert_eq!(
        parsed,
        HostToClient::HelloOk {
            latest_msg_id: 0,
            messages: Vec::new(),
            events: Vec::new(),
            processes: Vec::new(),
            side_chats: Vec::new(),
            host_version: String::new(),
            model: None,
            subagent_models: None,
            prompts: None,
            providers: None,
            views: Vec::new(),
        }
    );
}

#[test]
fn model_selection_frames_use_snake_case_protocol_names() {
    let command = ClientToHost::SetModel {
        model_id: "gpt-5.6-sol".to_string(),
        variant: "high".to_string(),
    };
    assert_eq!(
        serde_json::to_value(&command).unwrap(),
        json!({
            "type": "set_model",
            "model_id": "gpt-5.6-sol",
            "variant": "high"
        })
    );

    let event = HostToClient::ModelChanged {
        current: ModelSelection {
            id: "gpt-5.6-sol".to_string(),
            variant: "high".to_string(),
        },
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "model_changed",
            "current": { "id": "gpt-5.6-sol", "variant": "high" }
        })
    );

    let command = ClientToHost::SetSubagentModel {
        provider: "codex".to_string(),
        model_id: "gpt-5.6-terra".to_string(),
        enabled: false,
        enabled_variants: vec!["low".to_string(), "high".to_string()],
    };
    assert_eq!(
        serde_json::to_value(&command).unwrap(),
        json!({
            "type": "set_subagent_model",
            "provider": "codex",
            "model_id": "gpt-5.6-terra",
            "enabled": false,
            "enabled_variants": ["low", "high"]
        })
    );

    let catalog = SubagentModelCatalog {
        providers: vec![SubagentProviderModels {
            provider: "codex".to_string(),
            label: "Codex CLI".to_string(),
            models: vec![SubagentModel {
                id: "gpt-5.6-terra".to_string(),
                label: "Terra".to_string(),
                variants: vec!["low".to_string(), "medium".to_string(), "high".to_string()],
                enabled_variants: vec!["low".to_string(), "high".to_string()],
                enabled: true,
            }],
        }],
    };
    let event = HostToClient::SubagentModelsChanged {
        catalog: catalog.clone(),
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "subagent_models_changed",
            "catalog": {
                "providers": [{
                    "provider": "codex",
                    "label": "Codex CLI",
                    "models": [{
                        "id": "gpt-5.6-terra",
                        "label": "Terra",
                        "variants": ["low", "medium", "high"],
                        "enabled_variants": ["low", "high"],
                        "enabled": true
                    }]
                }]
            }
        })
    );
    assert_eq!(
        serde_json::from_value::<ClientToHost>(serde_json::to_value(command).unwrap()).unwrap(),
        ClientToHost::SetSubagentModel {
            provider: "codex".to_string(),
            model_id: "gpt-5.6-terra".to_string(),
            enabled: false,
            enabled_variants: vec!["low".to_string(), "high".to_string()],
        }
    );
    assert_eq!(
        serde_json::from_value::<HostToClient>(serde_json::to_value(event).unwrap()).unwrap(),
        HostToClient::SubagentModelsChanged { catalog }
    );
}

#[test]
fn scoped_server_frames_round_trip() {
    let ts = Utc.with_ymd_and_hms(2026, 7, 9, 12, 0, 0).unwrap();
    let frames = [
        HostToClient::Msg {
            message: ChatMessage {
                id: 1,
                author: ChatAuthor::Agent,
                body: "hello".to_string(),
                r#ref: None,
                ts,
                attachments: Vec::new(),
                tool_calls: Vec::new(),
            },
            sc: Some("abc".to_string()),
        },
        HostToClient::TurnEvent {
            seq: 1,
            event: TurnEventKind::Prose {
                text: "hello".to_string(),
            },
            sc: Some("abc".to_string()),
        },
        HostToClient::AgentActivity {
            state: AgentActivityState::Thinking,
            text: Some("thinking".to_string()),
            sc: Some("abc".to_string()),
        },
    ];

    for frame in frames {
        let encoded = serde_json::to_value(&frame).unwrap();
        assert_eq!(encoded.get("sc"), Some(&json!("abc")));
        let decoded: HostToClient = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, frame);
    }
}

#[test]
fn main_scope_frames_omit_sc() {
    let ts = Utc.with_ymd_and_hms(2026, 7, 9, 12, 0, 0).unwrap();
    let frames = [
        HostToClient::Msg {
            message: ChatMessage {
                id: 1,
                author: ChatAuthor::Agent,
                body: "hello".to_string(),
                r#ref: None,
                ts,
                attachments: Vec::new(),
                tool_calls: Vec::new(),
            },
            sc: None,
        },
        HostToClient::TurnEvent {
            seq: 1,
            event: TurnEventKind::Prose {
                text: "hello".to_string(),
            },
            sc: None,
        },
        HostToClient::AgentActivity {
            state: AgentActivityState::Idle,
            text: None,
            sc: None,
        },
    ];

    for frame in frames {
        let encoded = serde_json::to_value(frame).unwrap();
        assert!(encoded.get("sc").is_none());
    }
}

/// The three `kind` literals are a cross-language contract: the config store
/// writes them into `hirsel.toml`, the docs quote them, and the client branches
/// on them to tell an editable OpenAI-compatible instance from a detected OAuth
/// one. Derived casing does NOT produce the OpenAI spelling — `snake_case`
/// breaks `OpenAiCompatible` at the capital A — so the literals are pinned here
/// rather than left to the derive.
#[test]
fn provider_kinds_serialize_to_the_documented_literals() {
    for (kind, literal) in [
        (ProviderKind::Codex, "codex"),
        (ProviderKind::Claude, "claude"),
        (ProviderKind::OpenAiCompatible, "openai_compatible"),
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(literal));
        assert_eq!(
            serde_json::from_value::<ProviderKind>(json!(literal)).unwrap(),
            kind
        );
    }
}

#[test]
fn provider_ops_use_snake_case_protocol_names() {
    assert_eq!(
        serde_json::to_value(ClientToHost::SetAgentProvider {
            agent: AgentSlot::Fork,
            provider_id: "openrouter".to_string(),
        })
        .unwrap(),
        json!({
            "type": "set_agent_provider",
            "agent": "fork",
            "provider_id": "openrouter"
        })
    );
    assert_eq!(
        serde_json::to_value(ClientToHost::AddProvider {
            id: "openrouter".to_string(),
            label: "OpenRouter".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            api_key: "sk-or-v1-secret".to_string(),
            default_model: "google/gemini-3.7-flash".to_string(),
        })
        .unwrap(),
        json!({
            "type": "add_provider",
            "id": "openrouter",
            "label": "OpenRouter",
            "base_url": "https://openrouter.ai/api/v1",
            "api_key": "sk-or-v1-secret",
            "default_model": "google/gemini-3.7-flash"
        })
    );
    // An omitted patch field is absent on the wire, which is how the host
    // tells "leave the stored key alone" from "clear it".
    assert_eq!(
        serde_json::to_value(ClientToHost::UpdateProvider {
            id: "openrouter".to_string(),
            label: Some("Router".to_string()),
            base_url: None,
            api_key: None,
            default_model: None,
        })
        .unwrap(),
        json!({
            "type": "update_provider",
            "id": "openrouter",
            "label": "Router"
        })
    );
    assert_eq!(
        serde_json::to_value(ClientToHost::RemoveProvider {
            id: "openrouter".to_string(),
        })
        .unwrap(),
        json!({ "type": "remove_provider", "id": "openrouter" })
    );
    assert_eq!(
        serde_json::to_value(ClientToHost::RedetectProvider {
            id: "codex".to_string(),
        })
        .unwrap(),
        json!({ "type": "redetect_provider", "id": "codex" })
    );
}

#[test]
fn providers_changed_round_trips_and_masks_stay_masked() {
    let roster = ProviderRoster {
        instances: vec![
            ProviderInstance {
                id: "claude".to_string(),
                kind: ProviderKind::Claude,
                label: "Claude".to_string(),
                base_url: None,
                api_key: MaskedSecret::default(),
                default_model: String::new(),
                detection: Some(DetectionStatus {
                    detected: false,
                    path: "/home/owner/.claude/.credentials.json".to_string(),
                    account_hint: None,
                    detail: Some("no credentials file".to_string()),
                }),
                agent_selectable: false,
                removable: false,
            },
            ProviderInstance {
                id: "openrouter".to_string(),
                kind: ProviderKind::OpenAiCompatible,
                label: "OpenRouter".to_string(),
                base_url: Some("https://openrouter.ai/api/v1".to_string()),
                api_key: MaskedSecret {
                    present: true,
                    tail: "cret".to_string(),
                },
                default_model: "google/gemini-3.7-flash".to_string(),
                detection: None,
                agent_selectable: true,
                removable: true,
            },
        ],
        booted_provider_id: Some("codex".to_string()),
    };
    let frame = HostToClient::ProvidersChanged {
        roster: roster.clone(),
    };
    let encoded = serde_json::to_string(&frame).unwrap();
    assert!(encoded.starts_with(r#"{"type":"providers_changed""#));
    assert!(!encoded.contains("sk-or-v1"));
    let decoded: HostToClient = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, frame);
}

#[test]
fn model_snapshot_provider_fields_default_for_older_hosts() {
    let parsed: ModelSnapshot = serde_json::from_value(json!({
        "current": { "id": "gpt-5.6-sol", "variant": "high" },
        "available": []
    }))
    .unwrap();
    assert_eq!(parsed.provider_id, None);
    assert!(!parsed.free_text_model);

    let parsed: ForkAgentConfig = serde_json::from_value(json!({
        "current": { "id": "gpt-5.6-luna", "variant": "max" },
        "available": [],
        "prompt": { "text": "Triage.", "is_default": true }
    }))
    .unwrap();
    assert_eq!(parsed.provider_id, None);
    assert!(!parsed.free_text_model);
}
