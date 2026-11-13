//! The revised grammar's terminal tables. All precedence and boundary decisions live here.
pub const ITEM_START: &[&str] = &[
    "@", "pub", "fn", "type", "stable", "resource", "scoped", "const",
];
pub const PREFIX: &[&str] = &["-", "!", "~", "take", "mut", "copy", "view"];
pub const BOUNDS: &[&str] = &[
    "plain",
    "shareable",
    "send",
    "key",
    "ordered",
    "integer",
    "float",
    "packed",
    "streamable",
];
pub const ASSIGN: &[&str] = &[
    "=", "+=", "-=", "*=", "/=", "%=", "<<=", ">>=", "&=", "|=", "^=",
];
/// (spelling, precedence, nonassociative). Adapted from the old precedence climber.
pub const BINARY: &[(&str, u8, bool)] = &[
    ("..", 1, true),
    ("..=", 1, true),
    ("||", 2, false),
    ("&&", 3, false),
    ("==", 4, true),
    ("!=", 4, true),
    ("<", 4, true),
    ("<=", 4, true),
    (">", 4, true),
    (">=", 4, true),
    ("|", 5, false),
    ("^", 6, false),
    ("&", 7, false),
    ("<<", 8, false),
    (">>", 8, false),
    ("+", 9, false),
    ("-", 9, false),
    ("*", 10, false),
    ("/", 10, false),
    ("%", 10, false),
];
