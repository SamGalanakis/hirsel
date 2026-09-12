//! Wire-contract round-trip tests for the protocol frames.

use crate::*;
use chrono::{TimeZone, Utc};
use serde_json::json;

#[test]
fn client_hello_round_trips_tagged_auth() {
    let value = json!({
        "type": "hello",
        "auth": { "static_token": "secret" }
    });

    let parsed: ClientToHost = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        parsed,
        ClientToHost::Hello {
            auth: HelloAuth::StaticToken("secret".to_string()),
        }
    );
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
}

#[test]
fn pairing_auth_and_paired_response_round_trip() {
    let hello = ClientToHost::Hello {
        auth: HelloAuth::PairingCode {
            code: "pairing-code".to_string(),
            device_label: "Owner phone".to_string(),
        },
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
fn cancel_frames_round_trip() {
    let cancel_turn = ClientToHost::CancelTurn {
        history_id: "history-a".into(),
        thread_id: 1,
    };
    let encoded = serde_json::to_string(&cancel_turn).unwrap();
    assert_eq!(
        encoded,
        r#"{"type":"cancel_turn","history_id":"history-a","thread_id":1}"#
    );
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
fn thread_mutations_require_captured_history_on_the_wire() {
    for value in [
        json!({"type":"create_thread","client_id":"create","history_id":"history-a","kind":"task","title":"Child","parent_thread_id":1}),
        json!({"type":"send_thread_message","client_id":"send","history_id":"history-a","thread_id":1,"body":"Hello","attachments":[],"mentions":[],"mode":"send","artifact_ids":[]}),
        json!({"type":"thread_action","client_id":"action","history_id":"history-a","thread_id":1,"action":"archive","data":{},"expected_revision":null}),
        json!({"type":"cancel_turn","history_id":"history-a","thread_id":1}),
    ] {
        let command: ClientToHost = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(command).unwrap(), value);
        let mut missing_history = value;
        missing_history
            .as_object_mut()
            .unwrap()
            .remove("history_id");
        assert!(serde_json::from_value::<ClientToHost>(missing_history).is_err());
    }

    let mut missing_client_id = json!({"type":"thread_action","client_id":"action","history_id":"history-a","thread_id":1,"action":"archive","data":{},"expected_revision":null});
    missing_client_id
        .as_object_mut()
        .unwrap()
        .remove("client_id");
    assert!(serde_json::from_value::<ClientToHost>(missing_client_id).is_err());

    let applied = HostToClient::ThreadActionApplied {
        client_id: "action".into(),
        history_id: "history-a".into(),
        thread_id: 1,
    };
    let value = json!({"type":"thread_action_applied","client_id":"action","history_id":"history-a","thread_id":1});
    assert_eq!(serde_json::to_value(&applied).unwrap(), value);
    assert_eq!(
        serde_json::from_value::<HostToClient>(value).unwrap(),
        applied
    );

    for invalid in [
        json!({"type":"create_thread","client_id":"missing-kind","history_id":"history-a","title":"Child","parent_thread_id":null}),
        json!({"type":"create_thread","client_id":"bad-kind","history_id":"history-a","kind":"project","title":"Child","parent_thread_id":null}),
    ] {
        assert!(serde_json::from_value::<ClientToHost>(invalid).is_err());
    }
    for kind in [ThreadKind::Space, ThreadKind::Task] {
        let frame = ClientToHost::CreateThread {
            history_id: "history-a".into(),
            kind,
            parent_thread_id: None,
            client_id: format!("{kind:?}"),
            title: "Root".into(),
        };
        let encoded = serde_json::to_value(&frame).unwrap();
        assert_eq!(
            encoded["kind"],
            if kind == ThreadKind::Space {
                "space"
            } else {
                "task"
            }
        );
        assert_eq!(
            serde_json::from_value::<ClientToHost>(encoded).unwrap(),
            frame
        );
    }
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
fn view_frames_round_trip_with_resolved_specs_and_event_data() {
    let spec = json!({
        "type": "action",
        "label": "Approve",
        "action": "approve"
    });
    let upsert = HostToClient::ViewUpsert {
        thread_id: 1,
        instance_id: "view-1".to_string(),
        spec: spec.clone(),
    };
    let encoded = serde_json::to_value(&upsert).unwrap();
    assert_eq!(encoded["type"], "view_upsert");
    assert_eq!(encoded["spec"], spec);
    assert!(encoded.get("placement").is_none());
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
fn chat_message_without_attachments_deserializes_as_empty() {
    let value = json!({
        "id": 1,
        "author": "owner",
        "body": "message",
        "thread_id": 0,
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
        thread_id: 1,
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
        r#"{"type":"process_upsert","process":{"thread_id":1,"id":"proc-1","kind":"monitor","label":"watch file","agent":null,"model":null,"state":"done","started_ts":"2026-07-09T12:00:00Z","last_event_ts":"2026-07-09T12:00:00Z","summary":null}}"#
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
        turn_id: 1,
        thread_id: 1,
        seq: event.seq,
        event: event.event.clone(),
    })
    .unwrap();
    assert_eq!(
        encoded,
        r#"{"type":"turn_event","turn_id":1,"thread_id":1,"seq":1,"event":{"kind":"prose","text":"I will check that now."}}"#
    );
    let decoded: HostToClient = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        decoded,
        HostToClient::TurnEvent {
            turn_id: 1,
            thread_id: 1,
            seq: 1,
            event: event.event,
        }
    );
}

#[test]
fn turn_event_tool_start_round_trips() {
    let event = HostToClient::TurnEvent {
        turn_id: 1,
        thread_id: 1,
        seq: 2,
        event: TurnEventKind::ToolStart {
            id: "call-1".to_string(),
            name: "shell_run".to_string(),
            summary: Some("cmd: true".to_string()),
            input: Some(TurnEventPayload {
                text: r#"{"cmd":"true"}"#.to_string(),
                truncated: false,
            }),
        },
    };

    let encoded = serde_json::to_string(&event).unwrap();
    assert_eq!(
        encoded,
        r#"{"type":"turn_event","turn_id":1,"thread_id":1,"seq":2,"event":{"kind":"tool_start","id":"call-1","name":"shell_run","summary":"cmd: true","input":{"text":"{\"cmd\":\"true\"}","truncated":false}}}"#
    );
    let decoded: HostToClient = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, event);
}

#[test]
fn turn_event_tool_done_round_trips() {
    let event = HostToClient::TurnEvent {
        turn_id: 1,
        thread_id: 1,
        seq: 3,
        event: TurnEventKind::ToolDone {
            id: "call-1".to_string(),
            name: "shell_run".to_string(),
            ok: true,
            summary: Some("ok status 0".to_string()),
            result: Some(TurnEventPayload {
                text: r#"{"stdout":"done"}"#.to_string(),
                truncated: false,
            }),
        },
    };

    let encoded = serde_json::to_string(&event).unwrap();
    assert_eq!(
        encoded,
        r#"{"type":"turn_event","turn_id":1,"thread_id":1,"seq":3,"event":{"kind":"tool_done","id":"call-1","name":"shell_run","ok":true,"summary":"ok status 0","result":{"text":"{\"stdout\":\"done\"}","truncated":false}}}"#
    );
    let decoded: HostToClient = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, event);
}

#[test]
fn persisted_tool_call_summary_requires_canonical_id() {
    let summary = ToolCallSummary {
        id: "call-1".to_string(),
        name: "shell_run".to_string(),
        ok: true,
    };
    let encoded = serde_json::to_value(&summary).unwrap();
    assert_eq!(
        encoded,
        serde_json::json!({"id":"call-1","name":"shell_run","ok":true})
    );
    assert_eq!(
        serde_json::from_value::<ToolCallSummary>(encoded).unwrap(),
        summary
    );
    assert!(
        serde_json::from_value::<ToolCallSummary>(
            serde_json::json!({"name":"shell_run","ok":true})
        )
        .is_err(),
        "name-only stored summaries are outside the current contract"
    );
}

#[test]
fn model_selection_frames_use_snake_case_protocol_names() {
    let command = ClientToHost::SetModel {
        provider_id: "codex".to_string(),
        model_id: "gpt-5.6-sol".to_string(),
        variant: "high".to_string(),
    };
    assert_eq!(
        serde_json::to_value(&command).unwrap(),
        json!({
            "type": "set_model",
            "provider_id": "codex",
            "model_id": "gpt-5.6-sol",
            "variant": "high"
        })
    );

    let fork_command = ClientToHost::SetForkModel {
        provider_id: "codex".to_string(),
        model_id: "gpt-5.6-luna".to_string(),
        variant: "max".to_string(),
    };
    assert_eq!(
        serde_json::to_value(&fork_command).unwrap(),
        json!({
            "type": "set_fork_model",
            "provider_id": "codex",
            "model_id": "gpt-5.6-luna",
            "variant": "max"
        })
    );

    let event = HostToClient::ModelChanged {
        model: ModelSnapshot {
            current: ModelSelection {
                id: "gpt-5.6-sol".to_string(),
                variant: "high".to_string(),
            },
            available: vec![AvailableModel {
                id: "gpt-5.6-sol".to_string(),
                label: "GPT-5.6 Sol".to_string(),
                variants: vec!["low".to_string(), "high".to_string()],
                default_variant: "low".to_string(),
            }],
            provider_id: Some("codex".to_string()),
            free_text_model: false,
        },
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "model_changed",
            "model": {
                "current": { "id": "gpt-5.6-sol", "variant": "high" },
                "available": [{
                    "id": "gpt-5.6-sol",
                    "label": "GPT-5.6 Sol",
                    "variants": ["low", "high"],
                    "default_variant": "low"
                }],
                "provider_id": "codex",
                "free_text_model": false
            }
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
        native_worker: SubagentNativeWorker {
            label: "Native worker".to_string(),
            enabled: true,
            provider_id: Some("openrouter".to_string()),
            eligible_provider_ids: vec!["openrouter".to_string()],
            model: "deepseek/deepseek-v4.1-flash".to_string(),
            default_model: "deepseek/deepseek-v4.1-flash".to_string(),
            model_override: None,
            unavailable_reason: None,
        },
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
                }],
                "native_worker": {
                    "label": "Native worker",
                    "enabled": true,
                    "provider_id": "openrouter",
                    "eligible_provider_ids": ["openrouter"],
                    "model": "deepseek/deepseek-v4.1-flash",
                    "default_model": "deepseek/deepseek-v4.1-flash",
                    "model_override": null
                }
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

    // The native worker row travels as its own command: a free-text model and
    // no variants do not fit `set_subagent_model`'s curated-row shape.
    let command = ClientToHost::SetNativeWorker {
        enabled: true,
        model: Some("z-ai/glm-5".to_string()),
    };
    assert_eq!(
        serde_json::to_value(&command).unwrap(),
        json!({"type": "set_native_worker", "enabled": true, "model": "z-ai/glm-5"})
    );
    assert_eq!(
        serde_json::from_value::<ClientToHost>(serde_json::to_value(command).unwrap()).unwrap(),
        ClientToHost::SetNativeWorker {
            enabled: true,
            model: Some("z-ai/glm-5".to_string()),
        }
    );
    // An absent model is "use the shipped default", and it stays absent on the
    // wire rather than travelling as an empty string.
    assert_eq!(
        serde_json::to_value(ClientToHost::SetNativeWorker {
            enabled: false,
            model: None,
        })
        .unwrap(),
        json!({"type": "set_native_worker", "enabled": false})
    );
    assert_eq!(
        serde_json::from_value::<ClientToHost>(
            json!({"type": "set_native_worker", "enabled": false})
        )
        .unwrap(),
        ClientToHost::SetNativeWorker {
            enabled: false,
            model: None,
        }
    );
}

#[test]
fn main_scope_frames_omit_sc() {
    let ts = Utc.with_ymd_and_hms(2026, 7, 9, 12, 0, 0).unwrap();
    let frames = [
        HostToClient::Msg {
            message: ChatMessage {
                artifact_ids: Vec::new(),
                client_id: None,
                thread_id: 0,
                mentions: Vec::new(),
                id: 1,
                author: ChatAuthor::Agent,
                body: "hello".to_string(),
                r#ref: None,
                ts,
                attachments: Vec::new(),
                tool_calls: Vec::new(),
            },
        },
        HostToClient::TurnEvent {
            turn_id: 1,
            thread_id: 1,
            seq: 1,
            event: TurnEventKind::Prose {
                text: "hello".to_string(),
            },
        },
        HostToClient::AgentActivity {
            turn_id: 1,
            thread_id: 1,
            state: AgentActivityState::Idle,
            text: None,
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
                selection: None,
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
                selection: Some(ProviderSelection::FreeText),
                removable: true,
            },
        ],
        booted_provider_id: Some("codex".to_string()),
        boot_notice: None,
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
fn a_boot_notice_is_absent_from_the_wire_until_there_is_one() {
    let mut roster = ProviderRoster {
        instances: Vec::new(),
        booted_provider_id: Some("codex".to_string()),
        boot_notice: None,
    };
    let encoded = serde_json::to_string(&roster).unwrap();
    assert!(!encoded.contains("boot_notice"), "{encoded}");
    assert_eq!(
        serde_json::from_str::<ProviderRoster>(&encoded).unwrap(),
        roster
    );

    roster.boot_notice = Some(
        "configured provider \"acme\" is unavailable at boot: no API key is stored — running on \
         Codex"
            .to_string(),
    );
    let encoded = serde_json::to_string(&roster).unwrap();
    assert!(encoded.contains("no API key is stored"), "{encoded}");
    assert_eq!(
        serde_json::from_str::<ProviderRoster>(&encoded).unwrap(),
        roster
    );
}

#[test]
fn artifact_content_frames_and_optional_message_references_round_trip() {
    let value = json!({"type":"artifact_opened","client_id":"open-1","artifact":{"id":7,"title":"Result","kind":"solid","mime":"text/jsx","filename":null,"created_at":"2026-09-09T00:00:00Z","updated_at":"2026-09-09T00:00:00Z","thread_ids":[0,2],"content":"export default function App(){return <p>Hello</p>}"}});
    let frame: HostToClient = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(frame).unwrap(), value);
    let list: ClientToHost =
        serde_json::from_value(json!({"type":"list_artifacts","client_id":"all"})).unwrap();
    assert!(matches!(
        list,
        ClientToHost::ListArtifacts {
            thread_id: None,
            ..
        }
    ));
    let message: ChatMessage = serde_json::from_value(
        json!({"id":1,"thread_id":0,"author":"agent","body":"note","ref":null,"ts":"2026-09-09T00:00:00Z"}),
    )
    .unwrap();
    assert!(message.artifact_ids.is_empty());
}

#[test]
fn current_wire_rejects_removed_frames_auth_aliases_and_unowned_execution() {
    for value in [
        json!({"type":"hello","token":"secret"}),
        json!({"type":"hello","auth":"secret"}),
        json!({"type":"hello","auth":{"static_token":"secret"},"last_seen_msg_id":2}),
        json!({"type":"send_message","client_id":"old","body":"hi"}),
        json!({"type":"event_action","event_id":1,"action":"choose"}),
        json!({"type":"open_side_chat","ping_id":1}),
        json!({"type":"cancel_turn"}),
        json!({"type":"cancel_turn","thread_id":2,"sc":"old"}),
    ] {
        assert!(
            serde_json::from_value::<ClientToHost>(value.clone()).is_err(),
            "{value}"
        );
    }
    assert!(
        serde_json::from_value::<ClientToHost>(
            json!({"type":"hello","auth":{"static_token":"secret"}})
        )
        .is_ok()
    );
    assert!(
        serde_json::from_value::<HostToClient>(
            json!({"type":"agent_activity","state":"idle","text":null})
        )
        .is_err()
    );
    let frame = HostToClient::AgentActivity {
        thread_id: 2,
        turn_id: 4,
        state: AgentActivityState::Idle,
        text: None,
    };
    let value = serde_json::to_value(&frame).unwrap();
    assert_eq!(value["thread_id"], 2);
    assert_eq!(value["turn_id"], 4);
    assert!(value.get("sc").is_none());
    assert_eq!(
        serde_json::from_value::<HostToClient>(value).unwrap(),
        frame
    );
}

#[test]
fn related_commands_and_snapshots_preserve_typed_targets_and_history_scope() {
    for value in [
        serde_json::json!({"type":"add_thread_related","client_id":"save-url","history_id":"history-a","thread_id":4,"target":{"kind":"url","url":"https://example.com/a?q=1#b"},"title":null}),
        serde_json::json!({"type":"add_thread_related","client_id":"save-thread","history_id":"history-a","thread_id":4,"target":{"kind":"thread","history_id":"history-a","thread_id":8},"title":null}),
        serde_json::json!({"type":"remove_thread_related","client_id":"remove-1","history_id":"history-a","thread_id":4,"item_id":7}),
    ] {
        let command: ClientToHost = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(command).unwrap(), value);
        let mut missing_history = value;
        missing_history
            .as_object_mut()
            .unwrap()
            .remove("history_id");
        assert!(serde_json::from_value::<ClientToHost>(missing_history).is_err());
    }
    let value = serde_json::json!({"type":"thread_related_changed","client_id":"save-1","history_id":"history-a","thread_id":4,"revision":3,"items":[{"id":7,"thread_id":4,"target":{"kind":"thread","history_id":"history-a","thread_id":8},"title":null,"created_at":"2026-09-10T12:00:00Z"}]});
    let snapshot: HostToClient = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(snapshot).unwrap(), value);
    assert!(
        serde_json::from_value::<crate::ThreadRelatedTarget>(
            serde_json::json!({"kind":"url","url":"https://example.com","thread_id":8})
        )
        .is_err()
    );
}
