use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatAuthor {
    Owner,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: u64,
    pub author: ChatAuthor,
    pub body: String,
    #[serde(rename = "ref")]
    pub r#ref: Option<u64>,
    pub ts: DateTime<Utc>,
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
    AgentActivity {
        state: AgentActivityState,
        text: Option<String>,
    },
    InboxUpsert {
        item: InboxItem,
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
            "ref": 42
        });

        let parsed: ClientToHost = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            parsed,
            ClientToHost::SendMessage {
                client_id: "client-1".to_string(),
                body: "hello".to_string(),
                r#ref: Some(42),
            }
        );
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
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
}
