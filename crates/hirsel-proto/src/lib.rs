use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

pub const IROH_OWNER_ALPN: &[u8] = b"hirsel/owner/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatAuthor {
    Owner,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: u64,
    pub author: ChatAuthor,
    pub body: String,
    #[serde(rename = "ref")]
    pub r#ref: Option<u64>,
    pub ts: DateTime<Utc>,
    #[serde(default)]
    pub attachments: Vec<Blob>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallSummary {
    pub name: String,
    pub ok: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessKind {
    Subagent,
    Monitor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessState {
    Running,
    Done,
    Failed,
    Cancelled,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInfo {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideChatSummary {
    pub sc: String,
    pub ping_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewInstance {
    pub instance_id: String,
    pub placement: String,
    pub spec: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickReply {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PingStatus {
    Open,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ping {
    pub id: u64,
    pub name: String,
    pub description: String,
    pub content: String,
    pub anchor: u64,
    pub requires_response: bool,
    pub quick_replies: Vec<QuickReply>,
    pub status: PingStatus,
    #[serde(default)]
    pub read: bool,
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendMode {
    #[default]
    Send,
    NextTurn,
}

impl SendMode {
    pub fn is_send(&self) -> bool {
        matches!(self, Self::Send)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PushPlatform {
    Android,
    Web,
    Ios,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HelloAuth {
    StaticToken(String),
    DeviceToken(String),
    PairingCode { code: String, device_label: String },
}

impl<'de> Deserialize<'de> for HelloAuth {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum TaggedHelloAuth {
            StaticToken(String),
            DeviceToken(String),
            PairingCode { code: String, device_label: String },
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum HelloAuthWire {
            LegacyStaticToken(String),
            Tagged(TaggedHelloAuth),
        }

        Ok(match HelloAuthWire::deserialize(deserializer)? {
            HelloAuthWire::LegacyStaticToken(token) => Self::StaticToken(token),
            HelloAuthWire::Tagged(TaggedHelloAuth::StaticToken(token)) => Self::StaticToken(token),
            HelloAuthWire::Tagged(TaggedHelloAuth::DeviceToken(token)) => Self::DeviceToken(token),
            HelloAuthWire::Tagged(TaggedHelloAuth::PairingCode { code, device_label }) => {
                Self::PairingCode { code, device_label }
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum ClientToHost {
    Hello {
        #[serde(alias = "token")]
        auth: HelloAuth,
        last_seen_msg_id: Option<u64>,
    },
    SendMessage {
        client_id: String,
        body: String,
        #[serde(rename = "ref")]
        r#ref: Option<u64>,
        #[serde(default)]
        attachments: Vec<String>,
        #[serde(default, skip_serializing_if = "SendMode::is_send")]
        mode: SendMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sc: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mentions: Vec<u64>,
    },
    CancelTurn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sc: Option<String>,
    },
    CancelQueued {
        client_id: String,
    },
    UploadBlob {
        client_id: String,
        name: String,
        mime: String,
        data_b64: String,
    },
    GetBlobUrl {
        client_id: String,
        blob_id: String,
    },
    ResolvePing {
        ping_id: u64,
    },
    ReadPing {
        ping_id: u64,
    },
    RegisterPushToken {
        platform: PushPlatform,
        token: String,
    },
    UnregisterPushToken {
        token: String,
    },
    OpenSideChat {
        client_id: String,
        ping_id: u64,
    },
    ConcludeSideChat {
        sc: String,
    },
    ConfirmConclusion {
        sc: String,
        text: String,
    },
    DiscardSideChat {
        sc: String,
    },
    ViewEvent {
        instance_id: String,
        action: String,
        #[serde(default)]
        data: serde_json::Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentActivityState {
    Thinking,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnEvent {
    pub seq: u64,
    pub event: TurnEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
#[serde(rename_all = "snake_case")]
pub enum TurnEventKind {
    Prose {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolStart {
        /// Correlates start/done pairs so clients resolve the right row.
        id: String,
        name: String,
        summary: Option<String>,
    },
    ToolDone {
        id: String,
        name: String,
        ok: bool,
        summary: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum HostToClient {
    Paired {
        device_token: String,
    },
    HelloOk {
        latest_msg_id: u64,
        messages: Vec<ChatMessage>,
        pings: Vec<Ping>,
        processes: Vec<ProcessInfo>,
        #[serde(default)]
        side_chats: Vec<SideChatSummary>,
        /// Host build identity (crate version + git sha), shown in Settings → About.
        /// `#[serde(default)]` keeps older hosts/snapshots that omit it parseable.
        #[serde(default)]
        host_version: String,
        #[serde(default)]
        views: Vec<ViewInstance>,
    },
    Msg {
        message: ChatMessage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sc: Option<String>,
    },
    ProcessUpsert {
        process: ProcessInfo,
    },
    TurnEvent {
        seq: u64,
        event: TurnEventKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sc: Option<String>,
    },
    MsgRemoved {
        id: u64,
    },
    AgentActivity {
        state: AgentActivityState,
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sc: Option<String>,
    },
    PingUpsert {
        ping: Ping,
    },
    BlobOk {
        client_id: String,
        blob: Blob,
    },
    BlobUrl {
        client_id: String,
        blob_id: String,
        url: String,
        expires_at: u64,
    },
    Error {
        detail: String,
        /// Correlates the error to a specific client request (upload_blob,
        /// cancel_queued) so the client can mark the exact chip/bubble.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
    },
    SideChatOpen {
        sc: String,
        ping_id: u64,
        messages: Vec<ChatMessage>,
    },
    ConclusionDraft {
        sc: String,
        text: String,
    },
    SideChatClosed {
        sc: String,
    },
    ViewUpsert {
        instance_id: String,
        placement: String,
        spec: serde_json::Value,
    },
    ViewRemoved {
        instance_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
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
            name: "release-ready".to_string(),
            description: "Release is ready to ship".to_string(),
            content: "Done".to_string(),
            anchor: 1,
            requires_response: true,
            quick_replies: vec![QuickReply {
                value: "ship".to_string(),
                label: "Ship it".to_string(),
            }],
            status: PingStatus::Open,
            read: true,
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
            pings: vec![ping],
            processes: vec![process],
            side_chats: Vec::new(),
            host_version: "0.1.0 (test)".to_string(),
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
    fn ping_without_read_deserializes_as_unread() {
        let value = json!({
            "id": 9,
            "name": "old-ping",
            "description": "Old ping",
            "content": "old ping",
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
                ping_id: 9,
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
    }

    #[test]
    fn side_chat_host_frames_round_trip() {
        let frames = [
            HostToClient::SideChatOpen {
                sc: "side:abc".to_string(),
                ping_id: 9,
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
            "pings": [],
            "processes": []
        });
        let parsed: HostToClient = serde_json::from_value(old).unwrap();
        assert_eq!(
            parsed,
            HostToClient::HelloOk {
                latest_msg_id: 0,
                messages: Vec::new(),
                pings: Vec::new(),
                processes: Vec::new(),
                side_chats: Vec::new(),
                host_version: String::new(),
                views: Vec::new(),
            }
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
}
