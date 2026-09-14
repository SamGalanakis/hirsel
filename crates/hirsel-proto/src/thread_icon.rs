//! The one Thread icon vocabulary: a curated symbol set and a named tint.
//!
//! Both clients draw from this list and nothing else, so an icon always renders
//! and always fits the palette. Artwork for each name is generated from Lucide
//! by `scripts/gen-thread-symbols.mjs`; the names here are the wire contract.
use serde::{Deserialize, Serialize};

/// The picker's groups, in presentation order. Flattened, these are exactly the
/// accepted `symbol` values.
pub const THREAD_SYMBOL_GROUPS: [(&str, &[&str]); 7] = [
    (
        "Work",
        &[
            "hammer",
            "wrench",
            "bug",
            "flask",
            "rocket",
            "package",
            "git-branch",
            "terminal",
        ],
    ),
    (
        "Knowledge",
        &[
            "book",
            "file-text",
            "lightbulb",
            "graduation-cap",
            "brain",
            "search",
        ],
    ),
    (
        "People & places",
        &["users", "home", "building", "globe", "map-pin"],
    ),
    (
        "Money & time",
        &["wallet", "receipt", "calendar", "clock", "timer"],
    ),
    ("Comms", &["mail", "message-square", "bell", "megaphone"]),
    ("Media", &["image", "music", "film", "camera"]),
    (
        "Other",
        &[
            "star", "heart", "flag", "tag", "shield", "key", "zap", "leaf", "sun", "moon",
            "coffee", "gift", "puzzle",
        ],
    ),
];

/// Every accepted symbol name, in picker order. The generator reads this array.
pub const THREAD_SYMBOLS: [&str; 45] = [
    "hammer",
    "wrench",
    "bug",
    "flask",
    "rocket",
    "package",
    "git-branch",
    "terminal",
    "book",
    "file-text",
    "lightbulb",
    "graduation-cap",
    "brain",
    "search",
    "users",
    "home",
    "building",
    "globe",
    "map-pin",
    "wallet",
    "receipt",
    "calendar",
    "clock",
    "timer",
    "mail",
    "message-square",
    "bell",
    "megaphone",
    "image",
    "music",
    "film",
    "camera",
    "star",
    "heart",
    "flag",
    "tag",
    "shield",
    "key",
    "zap",
    "leaf",
    "sun",
    "moon",
    "coffee",
    "gift",
    "puzzle",
];

/// `true` when `name` is part of the vocabulary.
pub fn is_thread_symbol(name: &str) -> bool {
    THREAD_SYMBOLS.contains(&name)
}

/// The tile's colour. A closed palette keeps chosen icons inside the design
/// system on both canvases; `Neutral` is the quiet default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadTint {
    #[default]
    Neutral,
    Red,
    Orange,
    Amber,
    Green,
    Teal,
    Blue,
    Violet,
    Pink,
}

impl ThreadTint {
    pub const ALL: [Self; 9] = [
        Self::Neutral,
        Self::Red,
        Self::Orange,
        Self::Amber,
        Self::Green,
        Self::Teal,
        Self::Blue,
        Self::Violet,
        Self::Pink,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Neutral => "neutral",
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Amber => "amber",
            Self::Green => "green",
            Self::Teal => "teal",
            Self::Blue => "blue",
            Self::Violet => "violet",
            Self::Pink => "pink",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tint| tint.as_str() == value)
    }
}

/// A Thread's optional, durable identity mark: a vocabulary symbol on a tinted
/// tile, or a retained square image. Absent, clients draw a title monogram.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ThreadIcon {
    Symbol {
        name: String,
        #[serde(default)]
        tint: ThreadTint,
    },
    Image {
        blob_id: String,
    },
}

impl ThreadIcon {
    /// Reject anything outside the vocabulary. Deserialization stays permissive
    /// so an unknown name surfaces as one typed refusal, not a parse error.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Symbol { name, .. } if !is_thread_symbol(name) => Err(format!(
                "unknown Thread symbol {name:?}; choose one of: {}",
                THREAD_SYMBOLS.join(", ")
            )),
            Self::Symbol { .. } => Ok(()),
            Self::Image { blob_id } if blob_id.trim().is_empty() => {
                Err("image icon requires a blob_id".to_owned())
            }
            Self::Image { .. } => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_icons_round_trip_with_a_defaulted_tint() {
        let icon = ThreadIcon::Symbol {
            name: "rocket".to_owned(),
            tint: ThreadTint::Violet,
        };
        let wire = serde_json::to_value(&icon).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({"kind":"symbol","name":"rocket","tint":"violet"})
        );
        assert_eq!(
            serde_json::from_value::<ThreadIcon>(wire).unwrap(),
            icon.clone()
        );
        assert_eq!(
            serde_json::from_value::<ThreadIcon>(
                serde_json::json!({"kind":"symbol","name":"rocket"})
            )
            .unwrap(),
            ThreadIcon::Symbol {
                name: "rocket".to_owned(),
                tint: ThreadTint::Neutral
            }
        );
        icon.validate().unwrap();
    }

    #[test]
    fn unknown_names_and_tints_are_refused() {
        let unknown = ThreadIcon::Symbol {
            name: "sparkles".to_owned(),
            tint: ThreadTint::Neutral,
        };
        assert!(unknown.validate().unwrap_err().contains("sparkles"));
        assert!(
            serde_json::from_value::<ThreadIcon>(
                serde_json::json!({"kind":"symbol","name":"rocket","tint":"chartreuse"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ThreadIcon>(serde_json::json!({"kind":"emoji","value":"🌱"}))
                .is_err()
        );
        assert!(
            ThreadIcon::Image {
                blob_id: "  ".to_owned()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn tints_round_trip_through_their_wire_spelling() {
        for tint in ThreadTint::ALL {
            assert_eq!(ThreadTint::parse(tint.as_str()), Some(tint));
            assert_eq!(
                serde_json::to_value(tint).unwrap(),
                serde_json::Value::String(tint.as_str().to_owned())
            );
        }
        assert_eq!(ThreadTint::parse("chartreuse"), None);
    }
}
