use super::{ARGS_LITERAL, Corrupt, Cursor, put_bytes, put_varint, xxh3};

/// `args` of an ISSUE or OP frame: up to 16 literal bytes, else a digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Args {
    Literal(Vec<u8>),
    Digest(u64),
    Buffered {
        arguments: Box<Args>,
        spare: [u64; 2],
    },
}
impl Args {
    pub fn of(bytes: &[u8]) -> Self {
        Self::of_parts(&[bytes])
    }
    pub fn of_parts(parts: &[&[u8]]) -> Self {
        let len = parts
            .iter()
            .fold(0usize, |len, part| len.saturating_add(part.len()));
        if len <= ARGS_LITERAL {
            let mut bytes = Vec::with_capacity(len);
            for part in parts {
                bytes.extend_from_slice(part);
            }
            Args::Literal(bytes)
        } else {
            Args::Digest(xxh3(parts))
        }
    }
    pub fn buffered(self, spare: [u64; 2]) -> Self {
        Self::Buffered {
            arguments: Box::new(self),
            spare,
        }
    }

    pub fn recorded_spare(&self) -> Option<[u64; 2]> {
        match self {
            Self::Buffered { spare, .. } => Some(*spare),
            _ => None,
        }
    }

    pub fn operands(&self) -> &Self {
        match self {
            Self::Buffered { arguments, .. } => arguments,
            _ => self,
        }
    }

    pub(super) fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Args::Literal(b) => put_bytes(out, b),
            Args::Digest(d) => {
                out.push(ARGS_LITERAL as u8 + 1);
                out.extend_from_slice(&d.to_le_bytes());
            }
            Self::Buffered { arguments, spare } => {
                out.push(ARGS_LITERAL as u8 + 2);
                for room in spare {
                    put_varint(out, *room);
                }
                arguments.encode(out);
            }
        }
    }
    pub(super) fn decode(c: &mut Cursor) -> Result<Self, Corrupt> {
        let n = c.varint()?;
        if n == ARGS_LITERAL as u64 + 2 {
            let spare = [c.varint()?, c.varint()?];
            let len = c.varint()?;
            Ok(Self::decode_plain(c, len)?.buffered(spare))
        } else {
            Self::decode_plain(c, n)
        }
    }

    fn decode_plain(c: &mut Cursor, n: u64) -> Result<Self, Corrupt> {
        if n <= ARGS_LITERAL as u64 {
            Ok(Args::Literal(c.take(n as usize)?.to_vec()))
        } else if n == ARGS_LITERAL as u64 + 1 {
            Ok(Args::Digest(u64::from_le_bytes(c.fixed()?)))
        } else {
            Err(Corrupt)
        }
    }
}
