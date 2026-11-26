//! The operator wire's Begin input and Decide verdict (`operator.BeginInput`, `operator.Verdict`).
use super::{Fates, Input};
use crate::launcher::session::{Reader, Wire, Writer, malformed};
use std::io;

/// What a Begin deploys (`operator.BeginInput`): a Register's inputs, with a source carrying
/// whether it asks for the law gate. Only a source can be gated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BeginInput {
    Attested(Vec<u8>),
    Publication(Vec<u8>),
    Source { manifest: Vec<u8>, gate: bool },
}

impl From<Input> for BeginInput {
    /// The Begin input that deploys `input` without the law gate.
    fn from(input: Input) -> Self {
        match input {
            Input::Attested(r) => Self::Attested(r),
            Input::Publication(r) => Self::Publication(r),
            Input::Source(manifest) => Self::Source {
                manifest,
                gate: false,
            },
        }
    }
}

impl Wire for BeginInput {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::Attested(r) => (w.u32(0), w.bytes(r)).1,
            Self::Publication(r) => (w.u32(1), w.bytes(r)).1,
            Self::Source { manifest, gate } => {
                w.u32(2);
                w.bytes(manifest);
                w.bool(*gate);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Attested(r.bytes()?),
            1 => Self::Publication(r.bytes()?),
            2 => Self::Source {
                manifest: r.bytes()?,
                gate: r.bool()?,
            },
            _ => return Err(malformed("operator begin input tag")),
        })
    }
}

/// The operator's verdict on a held request (`operator.Verdict`): an approval, with the fates
/// of the held runs its decision shows, or a refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Approve(Fates),
    Refuse,
}

impl Wire for Verdict {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::Approve(fates) => (w.u32(0), fates.put(w)).1,
            Self::Refuse => w.u32(1),
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Approve(Fates::get(r)?),
            1 => Self::Refuse,
            _ => return Err(malformed("operator verdict tag")),
        })
    }
}
