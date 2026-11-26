//! The child mode and its launcher-session encoding.
use super::{Attempt, Reader, Role, Wire, Writer, malformed};
use hirsel_core_ids::launch::MAX_MOVE_ENVELOPE;
use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallKind {
    Genesis,
    Standalone(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Serve {
    pub(crate) activation: u64,
    pub(crate) idle: u64,
    pub(crate) candidate: Option<u64>,
    pub(crate) store_id: Option<(u64, u64)>,
    pub(crate) witness: Option<u64>,
    pub(crate) move_envelope: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Install(u64, InstallKind, Vec<u8>),
    Serving(u64, Serve),
    Describe(Attempt),
    Preview(Attempt, Vec<u8>),
    /// The export directory's name and the deploy's value-codec `Fates`.
    Prepare(Attempt, String, Vec<u8>),
}

impl Mode {
    pub(crate) fn role(&self) -> Role {
        match *self {
            Self::Install(g, ..) => Role::Install(g),
            Self::Serving(g, _) => Role::Serving(g),
            Self::Describe(a) => Role::Describe(a),
            Self::Preview(a, _) => Role::Preview(a),
            Self::Prepare(a, _, _) => Role::Prepare(a),
        }
    }

    pub(crate) fn move_envelope(&self) -> &[u8] {
        match self {
            Self::Install(_, _, bytes) => bytes,
            Self::Serving(_, serve) => &serve.move_envelope,
            _ => &[],
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(in crate::launcher) struct MoveEnvelopeTooLarge(pub(in crate::launcher) usize);

impl std::fmt::Display for MoveEnvelopeTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "move envelope has {} bytes, maximum {MAX_MOVE_ENVELOPE}",
            self.0
        )
    }
}

impl std::error::Error for MoveEnvelopeTooLarge {}

fn decode_envelope(reader: &mut Reader<'_>) -> io::Result<Vec<u8>> {
    let n = reader.u32()? as usize;
    if n > MAX_MOVE_ENVELOPE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            MoveEnvelopeTooLarge(n),
        ));
    }
    Ok(reader.take(n)?.to_vec())
}

impl Wire for Mode {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::Install(g, kind, envelope) => {
                w.u32(0);
                w.u64(*g);
                match *kind {
                    InstallKind::Genesis => w.u32(0),
                    InstallKind::Standalone(activation) => {
                        w.u32(1);
                        w.u64(activation);
                    }
                }
                w.bytes(envelope);
            }
            Self::Serving(g, serve) => {
                w.u32(1);
                w.u64(*g);
                w.u64(serve.activation);
                w.u64(serve.idle);
                w.option(&serve.candidate, |w, &c| w.u64(c));
                w.option(&serve.store_id, |w, &(hi, lo)| {
                    w.u64(hi);
                    w.u64(lo);
                });
                w.option(&serve.witness, |w, &version| w.u64(version));
                w.bytes(&serve.move_envelope);
            }
            Self::Describe(a) => (w.u32(2), a.put(w)).1,
            Self::Preview(a, old) => {
                w.u32(3);
                a.put(w);
                w.bytes(old);
            }
            Self::Prepare(a, export, fates) => {
                w.u32(4);
                a.put(w);
                w.bytes(export.as_bytes());
                w.bytes(fates);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Install(
                r.u64()?,
                match r.u32()? {
                    0 => InstallKind::Genesis,
                    1 => InstallKind::Standalone(r.u64()?),
                    _ => return Err(malformed("install kind tag")),
                },
                decode_envelope(r)?,
            ),
            1 => Self::Serving(
                r.u64()?,
                Serve {
                    activation: r.u64()?,
                    idle: r.u64()?,
                    candidate: r.option(Reader::u64)?,
                    store_id: r.option(|r| Ok((r.u64()?, r.u64()?)))?,
                    witness: r.option(Reader::u64)?,
                    move_envelope: decode_envelope(r)?,
                },
            ),
            2 => Self::Describe(Attempt::get(r)?),
            3 => Self::Preview(Attempt::get(r)?, r.bytes()?),
            4 => Self::Prepare(Attempt::get(r)?, r.string()?, r.bytes()?),
            _ => return Err(malformed("mode tag")),
        })
    }
}
