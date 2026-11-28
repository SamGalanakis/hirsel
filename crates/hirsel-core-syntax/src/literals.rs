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

/// The bytes a quoted literal denotes (syntax.md): `".."`, `r#".."#`, and with `bytes`
/// `b".."`, `br#".."#`. Escapes are `\n \t \r \0 \\ \" \'` and `\xNN` (any byte in
/// byte strings), and in strings `\u{..}` as UTF-8; a raw body is its source bytes. The
/// lexer has refused every malformed spelling; None marks one anyway.
pub fn quoted_bytes(spelling: &str, bytes: bool) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    out.try_reserve_exact(spelling.len()).ok()?;
    quoted_into(spelling, bytes, &mut out)?;
    Some(out)
}

/// Decode into storage reserved by the caller for at least spelling.len()
/// additional bytes. The decoded body never exceeds the original spelling.
pub fn quoted_into(spelling: &str, bytes: bool, out: &mut Vec<u8>) -> Option<()> {
    let spelling = if bytes {
        spelling.strip_prefix('b')?
    } else {
        spelling
    };
    if let Some(raw) = spelling.strip_prefix('r') {
        let hashes = raw.len() - raw.trim_start_matches('#').len();
        let body = raw[hashes..].strip_prefix('"')?;
        let body = body.strip_suffix(&raw[..hashes])?.strip_suffix('"')?;
        out.extend_from_slice(body.as_bytes());
        return Some(());
    }
    let body = spelling.strip_prefix('"')?.strip_suffix('"')?.as_bytes();
    let mut i = 0;
    while i < body.len() {
        if body[i] != b'\\' {
            out.push(body[i]);
            i += 1;
            continue;
        }
        let b = match *body.get(i + 1)? {
            b'n' => b'\n',
            b't' => b'\t',
            b'r' => b'\r',
            b'0' => 0,
            c @ (b'\\' | b'"' | b'\'') => c,
            b'x' => {
                let hex = std::str::from_utf8(body.get(i + 2..i + 4)?).ok()?;
                i += 2;
                u8::from_str_radix(hex, 16).ok()?
            }
            b'u' if !bytes => {
                let close = i + body[i..].iter().position(|&c| c == b'}')?;
                let hex = std::str::from_utf8(body.get(i + 3..close)?).ok()?;
                let c = char::from_u32(u32::from_str_radix(hex, 16).ok()?)?;
                out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                i = close + 1;
                continue;
            }
            _ => return None,
        };
        out.push(b);
        i += 2;
    }
    Some(())
}
