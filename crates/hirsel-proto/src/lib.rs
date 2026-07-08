use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickReply {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InboxStatus {
    Open,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxItem {
    pub id: u64,
    pub content: String,
    pub anchor: u64,
    pub requires_response: bool,
    pub quick_replies: Vec<QuickReply>,
    pub status: InboxStatus,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum ClientToHost {
    Hello {
        token: String,
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
    },
    CancelTurn {},
    CancelQueued {
        client_id: String,
    },
    UploadBlob {
        client_id: String,
        name: String,
        mime: String,
        data_b64: String,
    },
    ArchiveItem {
        item_id: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentActivityState {
    Thinking,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum HostToClient {
    HelloOk {
        latest_msg_id: u64,
        messages: Vec<ChatMessage>,
        inbox: Vec<InboxItem>,
    },
    Msg {
        message: ChatMessage,
    },
    MsgRemoved {
        id: u64,
    },
    AgentActivity {
        state: AgentActivityState,
        text: Option<String>,
    },
    InboxUpsert {
        item: InboxItem,
    },
    BlobOk {
        client_id: String,
        blob: Blob,
    },
    Error {
        detail: String,
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
            "token": "secret",
            "last_seen_msg_id": null
        });

        let parsed: ClientToHost = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            parsed,
            ClientToHost::Hello {
                token: "secret".to_string(),
                last_seen_msg_id: None,
            }
        );
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
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
            }
        );
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
    }

    #[test]
    fn cancel_frames_round_trip() {
        let cancel_turn = ClientToHost::CancelTurn {};
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
    fn hello_ok_round_trips_chat_and_inbox() {
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
        };
        let item = InboxItem {
            id: 9,
            content: "Done".to_string(),
            anchor: 1,
            requires_response: true,
            quick_replies: vec![QuickReply {
                value: "ship".to_string(),
                label: "Ship it".to_string(),
            }],
            status: InboxStatus::Open,
            ts,
        };
        let response = HostToClient::HelloOk {
            latest_msg_id: 1,
            messages: vec![message],
            inbox: vec![item],
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
    }
}
