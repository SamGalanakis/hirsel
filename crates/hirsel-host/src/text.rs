//! Small shared text helpers used across the host crate.

pub(crate) fn short_label(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX_CHARS: usize = 80;
    if compact.chars().count() <= MAX_CHARS {
        compact
    } else {
        let mut truncated = compact.chars().take(MAX_CHARS - 3).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

pub(crate) fn option_key(index: usize) -> String {
    u8::try_from(index)
        .ok()
        .and_then(|index| b'A'.checked_add(index))
        .filter(u8::is_ascii_uppercase)
        .map(char::from)
        .map(String::from)
        .unwrap_or_else(|| (index + 1).to_string())
}
