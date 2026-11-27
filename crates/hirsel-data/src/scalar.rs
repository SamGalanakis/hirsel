use super::*;
use hirsel_core_syntax::{TokenKind, byte_value, character_value, quoted_bytes};
use serde_json::json;

macro_rules! scalars {
    ($($name:literal => $variant:ident),*) => {
        impl Scalar {
            pub fn parse(name: &str) -> Option<Self> {
                Some(match name { $($name => Self::$variant,)* _ => return None })
            }
            pub fn name(self) -> &'static str { match self { $(Self::$variant => $name,)* } }
        }
    }
}
scalars!("String"=>String,"Bytes"=>Bytes,"bool"=>Bool,"char"=>Char,"u8"=>U8,"u16"=>U16,
         "u32"=>U32,"u64"=>U64,"i8"=>I8,"i16"=>I16,"i32"=>I32,"i64"=>I64,"f32"=>F32,"f64"=>F64);

impl Scalar {
    /// Return the lossless projection and its decoded payload size.
    pub(crate) fn literal(self, at: Site) -> Option<(Value, usize)> {
        let negative = at.kind() == NodeKind::Unary && at.token() == Some("-");
        let leaf = if negative { at.kids().next()? } else { at };
        if leaf.kind() != NodeKind::Literal {
            return None;
        }
        let token = leaf.0.tree.node(leaf.1).token?;
        let kind = leaf.0.tree.tokens[token.0 as usize].kind;
        let raw = leaf.token()?;
        if negative && !matches!(kind, TokenKind::Integer | TokenKind::Float) {
            return None;
        }
        match (self, kind) {
            (Self::Bool, TokenKind::KwTrue | TokenKind::KwFalse) => Some((json!(raw == "true"), 1)),
            (Self::Char, TokenKind::Character) => Some((json!(character_value(raw)?), 4)),
            (Self::U8, TokenKind::Byte) => Some((json!(byte_value(raw)?), 1)),
            (Self::String, TokenKind::String) => {
                let text = String::from_utf8(quoted_bytes(raw, false)?).ok()?;
                let len = text.len();
                Some((Value::String(text), len))
            }
            (Self::Bytes, TokenKind::Bytes) => {
                let bytes = quoted_bytes(raw, true)?;
                let len = bytes.len();
                Some((json!(bytes), len))
            }
            (Self::F32 | Self::F64, TokenKind::Float) => {
                let spelling = raw
                    .strip_suffix(self.name())
                    .unwrap_or(raw)
                    .replace('_', "");
                if self == Self::F32 {
                    let n = spelling.parse::<f32>().ok()?;
                    n.is_finite()
                        .then(|| (json!((if negative { -n } else { n }).to_string()), 4))
                } else {
                    let n = spelling.parse::<f64>().ok()?;
                    n.is_finite()
                        .then(|| (json!((if negative { -n } else { n }).to_string()), 8))
                }
            }
            (_, TokenKind::Integer) => self.integer(raw, negative),
            _ => None,
        }
    }
    fn integer(self, raw: &str, negative: bool) -> Option<(Value, usize)> {
        let width: usize = match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 => 4,
            Self::I64 | Self::U64 => 8,
            _ => return None,
        };
        let signed = self.name().starts_with('i');
        if negative && !signed {
            return None;
        }
        let raw = raw.strip_suffix(self.name()).unwrap_or(raw);
        let (digits, radix) = [("0x", 16), ("0o", 8), ("0b", 2)]
            .into_iter()
            .find_map(|(prefix, radix)| raw.strip_prefix(prefix).map(|s| (s, radix)))
            .unwrap_or((raw, 10));
        let magnitude = u64::from_str_radix(&digits.replace('_', ""), radix).ok()?;
        let n = if negative {
            -i128::from(magnitude)
        } else {
            i128::from(magnitude)
        };
        let high = 1i128 << (width * 8 - usize::from(signed));
        if n >= high || n < -high {
            return None;
        }
        Some((
            if width == 8 {
                json!(n.to_string())
            } else {
                json!(n as i64)
            },
            width,
        ))
    }
}
