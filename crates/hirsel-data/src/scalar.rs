use super::*;
use crate::decoded::{host_memory, storage};
use hirsel_core_syntax::{TokenKind, byte_value, character_value, quoted_into};
use serde_json::json;
use std::fmt::Write;

fn text(raw: &str) -> Result<String, DataError> {
    let mut s = String::new();
    s.try_reserve_exact(raw.len()).map_err(|_| host_memory())?;
    s.extend(raw.chars().filter(|&c| c != '_'));
    Ok(s)
}

fn display(n: impl std::fmt::Display, capacity: usize) -> Result<Value, DataError> {
    let mut s = String::new();
    s.try_reserve_exact(capacity).map_err(|_| host_memory())?;
    write!(s, "{n}").expect("writing to String");
    Ok(Value::String(s))
}

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
    pub(crate) fn literal(self, at: Site) -> Result<Option<(Decoded<'static>, usize)>, DataError> {
        let negative = at.kind() == NodeKind::Unary && at.token() == Some("-");
        let leaf = if negative {
            at.kids().next().unwrap()
        } else {
            at
        };
        if leaf.kind() != NodeKind::Literal {
            return Ok(None);
        }
        let token = leaf.0.tree.node(leaf.1).token.unwrap();
        let kind = leaf.0.tree.tokens[token.0 as usize].kind;
        let raw = leaf.token().unwrap();
        if negative && !matches!(kind, TokenKind::Integer | TokenKind::Float) {
            return Ok(None);
        }
        let (value, size) = match (self, kind) {
            (Self::Bool, TokenKind::KwTrue | TokenKind::KwFalse) => (json!(raw == "true"), 1),
            (Self::Char, TokenKind::Character) => (display(character_value(raw).unwrap(), 4)?, 4),
            (Self::U8, TokenKind::Byte) => (json!(byte_value(raw).unwrap()), 1),
            (Self::String, TokenKind::String) | (Self::Bytes, TokenKind::Bytes) => {
                let mut bytes = storage(raw.len())?;
                quoted_into(raw, self == Self::Bytes, &mut bytes)
                    .expect("lexer checked the literal");
                let len = bytes.len();
                if self == Self::Bytes {
                    return Ok(Some((Decoded::Bytes(bytes), len)));
                }
                (
                    Value::String(String::from_utf8(bytes).expect("lexer checked UTF-8")),
                    len,
                )
            }
            (Self::F32 | Self::F64, TokenKind::Float) => {
                let raw = text(raw.strip_suffix(self.name()).unwrap_or(raw))?;
                if self == Self::F32 {
                    let Ok(n) = raw.parse::<f32>() else {
                        return Ok(None);
                    };
                    if !n.is_finite() {
                        return Ok(None);
                    }
                    (display(if negative { -n } else { n }, 64)?, 4)
                } else {
                    let Ok(n) = raw.parse::<f64>() else {
                        return Ok(None);
                    };
                    if !n.is_finite() {
                        return Ok(None);
                    }
                    (display(if negative { -n } else { n }, 350)?, 8)
                }
            }
            (_, TokenKind::Integer) => {
                let Some((n, width)) = self.integer(raw, negative) else {
                    return Ok(None);
                };
                (
                    if width == 8 {
                        display(n, 20)?
                    } else {
                        json!(n as i64)
                    },
                    width,
                )
            }
            _ => return Ok(None),
        };
        Ok(Some((Decoded::Scalar(value), size)))
    }
    fn integer(self, raw: &str, negative: bool) -> Option<(i128, usize)> {
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
        let magnitude = digits
            .chars()
            .filter(|&c| c != '_')
            .try_fold(0u64, |n, c| {
                n.checked_mul(u64::from(radix))?
                    .checked_add(u64::from(c.to_digit(radix)?))
            })?;
        let n = if negative {
            -i128::from(magnitude)
        } else {
            i128::from(magnitude)
        };
        let high = 1i128 << (width * 8 - usize::from(signed));
        if n >= high || n < -high {
            return None;
        }
        Some((n, width))
    }
}
