//! The admission types of the session wire (docs/spec/launch.md "The launcher", "Publish
//! and assemble"): what an `Admit` verifies, the host's approvals, the `Admitted` summary
//! with the law gate's green verdict, and why an admission failed.
use super::{Grants, Reader, Wire, Writer, malformed};
use std::io;

/// What an admission verifies: a recipe R the host attests, streamed in `AdmitData`; a
/// publication R already in the launcher's code store; or source, a `launcher.Tree`'s
/// codec bytes, which the launcher publishes and admits as a publication, running the law
/// gate first when its flag is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Input {
    Attested(Vec<u8>),
    Publication(Vec<u8>),
    Source(Vec<u8>, bool),
}

/// The host's policy for one admission: whether the image records, the Foreign and Exec
/// approvals, the group's working directory, the assembly the deploy would replace, and the
/// package roots the group's source may link (launch.md "Package roots per group").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Approvals {
    pub(crate) recording: bool,
    pub(crate) foreign: Vec<String>,
    pub(crate) exec: Vec<String>,
    pub(crate) workdir: String,
    pub(crate) old: Option<Vec<u8>>,
    pub(crate) packages: Vec<[u8; 32]>,
}

/// What the host compares before `Assemble`: the new and old root limits, the grant diff
/// against the old recipe, and the new recipe's target and place allowance, which the host
/// checks NEW's preview against.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Summary {
    pub(crate) root_limit: u64,
    pub(crate) old_root_limit: u64,
    pub(crate) grants: Grants,
    pub(crate) target: String,
    pub(crate) n_place: u64,
    /// The law gate's green verdict when the admission ran it.
    pub(crate) gate: Option<Green>,
}

/// A green law gate ("The verdict"): the submitted tree's digest, the root R it publishes,
/// and the compiler identity that ran its tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Green {
    pub(crate) tree: Vec<u8>,
    pub(crate) root: Vec<u8>,
    pub(crate) toolchain: Vec<u8>,
}

/// Why an admission ended: a policy refusal printed as is, an invalid input or release,
/// or a failed assembly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Denied(String),
    Invalid(String),
    Unassembled(String),
    /// The checker refused submitted source; its diagnostics.
    Uncompiled(String),
    /// Compiling submitted source reached the toolchain's memory limit.
    Exhausted(String),
    /// The law gate's red verdict: each test that failed, stopped or did not start.
    Red(Vec<String>),
    /// The resident compiler's session was lost while the gate ran: never green and never
    /// a durable red.
    GateLost(String),
}

impl Wire for Input {
    fn put(&self, w: &mut Writer) {
        let (tag, bytes) = match self {
            Self::Attested(r) => (0, r),
            Self::Publication(r) => (1, r),
            Self::Source(tree, _) => (2, tree),
        };
        w.u32(tag);
        w.bytes(bytes);
        if let Self::Source(_, gate) = self {
            w.bool(*gate);
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Attested(r.bytes()?),
            1 => Self::Publication(r.bytes()?),
            2 => Self::Source(r.bytes()?, r.bool()?),
            _ => return Err(malformed("admission input tag")),
        })
    }
}
