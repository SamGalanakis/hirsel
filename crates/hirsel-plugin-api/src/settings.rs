//! Setting descriptors — what a plugin exposes in Settings → Plugins.

use serde::Serialize;
use serde_json::Value;

/// How the app renders a setting, and how the host treats its value.
///
/// `Secret` values are write-only from the app's perspective: the host masks
/// them as `"<set>"` in every list response and never logs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingKind {
    String,
    Boolean,
    Secret,
}

/// One setting a plugin declares.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SettingDescriptor {
    pub key: String,
    pub label: String,
    pub kind: SettingKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

impl SettingDescriptor {
    pub fn string(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(key, label, SettingKind::String)
    }

    pub fn boolean(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(key, label, SettingKind::Boolean)
    }

    /// A credential. Never returned in cleartext by the management API and
    /// never written to the host log.
    pub fn secret(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(key, label, SettingKind::Secret)
    }

    fn new(key: impl Into<String>, label: impl Into<String>, kind: SettingKind) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            kind,
            default: None,
        }
    }

    pub fn with_default(mut self, default: impl Into<Value>) -> Self {
        self.default = Some(default.into());
        self
    }
}
