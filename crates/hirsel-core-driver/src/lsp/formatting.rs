use super::documents::{Encoding, Text};
use hirsel_core_syntax::{ParseMode, Symbols, format_verified, parse_bytes};
use serde_json::{Value, json};

pub(super) fn edits(text: &Text, mode: ParseMode, encoding: Encoding) -> Value {
    let mut symbols = Symbols::default();
    let parsed = parse_bytes(&text.bytes, mode, &mut symbols);
    match format_verified(&parsed, mode, &mut symbols) {
        Ok(Some(output)) if output.as_bytes() != text.bytes.as_ref() => {
            let end = u32::try_from(text.bytes.len()).unwrap();
            json!([{
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": text.position(end, encoding).unwrap(),
                },
                "newText": output,
            }])
        }
        Ok(_) => json!([]),
        Err(error) => {
            eprintln!("hirsel lsp: {error}");
            json!([])
        }
    }
}
