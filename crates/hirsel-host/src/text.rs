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
