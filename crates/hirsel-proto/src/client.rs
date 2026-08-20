//! Client → host frames and the auth/mode enums they carry.

use serde::{Deserialize, Deserializer, Serialize};

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
    FetchMessages {
        client_id: String,
        before_id: u64,
        limit: u64,
    },
    CancelTurn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sc: Option<String>,
    },
    CancelQueued {
        client_id: String,
    },
    SetModel {
        model_id: String,
        variant: String,
    },
    SetSubagentModel {
        provider: String,
        model_id: String,
        enabled: bool,
        enabled_variants: Vec<String>,
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
    ReopenPing {
        ping_id: u64,
    },
    ReadPing {
        ping_id: u64,
    },
    EventAction {
        event_id: u64,
        action: String,
        #[serde(default)]
        data: serde_json::Value,
    },
    ClearFinishedEvents {},
    RegisterPushToken {
        platform: PushPlatform,
        token: String,
    },
    UnregisterPushToken {
        token: String,
    },
    OpenSideChat {
        client_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_id: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ping_id: Option<u64>,
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
