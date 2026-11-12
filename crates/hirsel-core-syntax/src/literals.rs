//! Scalar decoding extracted from hirsel-syntax, adjusted to LANG-3 byte escapes.
pub fn character_value(spelling: &str) -> Option<char> {
    quoted_scalar(spelling.strip_prefix('\'')?.strip_suffix('\'')?, false)
}
/// Decode a `b'...'` byte spelling: one ASCII scalar, spelled as a character
/// literal's is, or a `\xNN` escape from 00 to FF. Anything else returns None.
pub fn byte_value(spelling: &str) -> Option<u8> {
    let body = spelling.strip_prefix("b'")?.strip_suffix('\'')?;
    u8::try_from(quoted_scalar(body, true)?).ok()
}
/// The one scalar a quoted literal's body denotes; `hex` admits `\xNN`.
fn quoted_scalar(body: &str, hex: bool) -> Option<char> {
    let mut chars = body.chars();
    let first = chars.next()?;
    let value = if first == '\\' {
        match chars.next()? {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '0' => '\0',
            '\\' => '\\',
            '"' => '"',
            '\'' => '\'',
            'x' => {
                let high = chars.next()?.to_digit(16)?;
                let low = chars.next()?.to_digit(16)?;
                {
                    let n = high * 16 + low;
                    if !hex && n > 127 {
                        return None;
                    }
                    char::from_u32(n)?
                }
            }
            'u' if !hex => {
                if chars.next()? != '{' {
                    return None;
                }
                let mut value = 0u32;
                let mut count = 0;
                loop {
                    let c = chars.next()?;
                    if c == '}' {
                        break;
                    }
                    count += 1;
                    if count > 6 || !c.is_ascii_hexdigit() {
                        return None;
                    }
                    value = value.checked_mul(16)?.checked_add(c.to_digit(16)?)?;
                }
                if count == 0 {
                    return None;
                }
                char::from_u32(value)?
            }
            _ => return None,
        }
    } else {
        if first <= '\u{1f}'
            || first == '\u{7f}'
            || matches!(first, '\'' | '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            return None;
        }
        if hex && !first.is_ascii() {
            return None;
        }
        first
    };
    chars.next().is_none().then_some(value)
}
