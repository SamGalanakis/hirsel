use crate::provider::IoError;
use hirsel_tape_format::{Corrupt, FORMAT, Stop};

/// Section 4.2's divergence kinds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mismatch {
    Op,
    Object,
    Arg,
    Missing,
    Extra,
    /// The replaying domain's outcome differs from END's (v2.3 R9).
    Outcome,
    Schedule,
    /// A recorded `TAKE` cannot apply at its position (recording.md,
    /// "Replay of a run with helpers").
    Take,
}

/// Where a replay diverged: `at` is the 1-based position of the frame it was
/// checked against, counting the frames after block 0 (one past the last frame
/// when the tape has none left).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Divergence {
    pub at: u64,
    pub kind: Mismatch,
}

/// Why a tape stopped. Every later operation on it fails the same way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    Diverged(Divergence),
    /// The tape is cut at the frame in this position.
    Truncated(u64),
    Damaged(u64),
    Corrupt,
    UnsupportedFormat(u16),
    /// The sink's write failed.
    Io(IoError),
    /// The tape was ended or dropped (section 8).
    Ended,
}
impl Fault {
    /// The fault a block or frame stop gives at frame position `at`.
    pub(super) fn stopped(s: Stop, at: u64) -> Self {
        match s {
            Stop::Missing => Fault::Diverged(Divergence {
                at,
                kind: Mismatch::Missing,
            }),
            Stop::Truncated => Fault::Truncated(at),
            Stop::Damaged(k) => Fault::Damaged(k),
            Stop::Corrupt => Fault::Corrupt,
            Stop::UnsupportedFormat(version) => Fault::UnsupportedFormat(version),
        }
    }
}

impl From<Corrupt> for Fault {
    fn from(_: Corrupt) -> Self {
        Fault::Corrupt
    }
}

/// Section 5.3: open refuses before anything runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    Code,
    Revision,
    Arg,
    Limit,
    Damaged(u64),
    HeaderTooLarge,
    /// `--unit` names no stream the tape holds.
    NoUnit,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenError {
    Refused(Refusal),
    /// The tape file could not be created, read or written.
    Io(IoError),
    /// The header block itself is cut or breaks the grammar.
    Unreadable(Fault),
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::Diverged(d) => write!(f, "diverged at {}, {:?}", d.at, d.kind),
            Fault::Truncated(at) => write!(f, "truncated at {at}"),
            Fault::Damaged(k) => write!(f, "block {k} damaged"),
            Fault::Corrupt => f.write_str("corrupt"),
            Fault::UnsupportedFormat(version) => {
                write!(f, "unsupported tape format {version} (expected {})", FORMAT)
            }
            Fault::Io(e) => write!(f, "tape write failed ({}, {})", e.tag, e.code),
            Fault::Ended => f.write_str("ended"),
        }
    }
}
