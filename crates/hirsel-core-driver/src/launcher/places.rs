use super::*;
use session::{Reader, Writer};
use std::io::Seek;

/// The bound on a wake report (one value-codec `Option[u64]`).
const REPORT_BYTES: u64 = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InstallRefusal {
    Held,
    Store,
    Place,
    Migration,
    Current,
    Identity,
    Capacity,
    Config,
    WrongVersion(u64),
    Predecessor(Vec<u8>),
    /// A held run with no fate, or an unmatched entry path (RUNNER-SPEC §14.6).
    Unsettled,
}

impl session::Selection {
    pub(super) fn new(activation: u64, assembly: Vec<u8>) -> Self {
        Self {
            activation,
            assembly,
        }
    }
}

impl<I: Images> Launcher<I> {
    #[cfg(test)]
    pub(crate) fn turn(&mut self, timeout: Duration) -> io::Result<()> {
        self.wait_turn(Some(timeout))
    }
}

pub(super) fn install_reply(
    answer: &[u8],
    status: Status,
    target: Option<u64>,
) -> Option<HostReply> {
    match (HostReply::decode(answer), status) {
        (Ok(reply @ HostReply::Installed(version, false)), Status::Code(0))
            if version != 0 && target.is_none_or(|target| version == target) =>
        {
            Some(reply)
        }
        (Ok(reply @ HostReply::InstallRefused(_)), Status::Code(1)) => Some(reply),
        (Ok(reply @ HostReply::Deposed(..)), Status::Code(1)) => Some(reply),
        _ => None,
    }
}

/// Read a finished child's bounded stdout answer or prepare `TRIGGERS` file, returning
/// any resulting `Placed` (install) or `Answered` event plus any saved install reply.
pub(super) fn read_child_outputs(
    child: &Child,
    status: Status,
    answer: Option<(File, u64)>,
    install_target: Option<u64>,
    triggers: Option<(PathBuf, u64)>,
) -> io::Result<(Option<HostReply>, Vec<Event>)> {
    let mut events = Vec::new();
    let mut saved_install = None;
    if let Some((mut out, bound)) = answer
        && (status == Status::Code(0) || (install_target.is_some() && status == Status::Code(1)))
    {
        let mut bytes = Vec::new();
        out.rewind()?;
        Read::by_ref(&mut out)
            .take(bound + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 <= bound {
            if let Some(target) = install_target {
                if let Some(reply) = install_reply(&bytes, status, Some(target)) {
                    saved_install = Some(reply.clone());
                    events.push(Event::Placed(child.clone(), reply));
                }
            } else {
                events.push(Event::Answered(child.clone(), bytes));
            }
        }
    }
    if let Some((path, bound)) = triggers
        && status == Status::Code(0)
    {
        // An unreadable or oversized report is no answer: the host then fails the
        // prepare, which ended without one.
        let mut bytes = Vec::new();
        let read = File::open(&path).and_then(|file| file.take(bound + 1).read_to_end(&mut bytes));
        if read.is_ok() && bytes.len() as u64 <= bound {
            events.push(Event::Answered(child.clone(), bytes));
        }
    }
    Ok((saved_install, events))
}

/// A wake report: an empty read is no report, and anything but one `Option[u64]` is none.
pub(super) fn read_report(file: &mut File) -> Option<Option<u64>> {
    let mut bytes = Vec::new();
    let _ = file.take(REPORT_BYTES + 1).read_to_end(&mut bytes);
    match bytes.as_slice() {
        [0, 0, 0, 0] => Some(None),
        [1, 0, 0, 0, due @ ..] if due.len() == 8 => {
            Some(Some(u64::from_le_bytes(due.try_into().unwrap())))
        }
        _ => None,
    }
}

impl Wire for InstallRefusal {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::Held => w.u32(0),
            Self::Store => w.u32(1),
            Self::Place => w.u32(2),
            Self::Migration => w.u32(3),
            Self::Current => w.u32(4),
            Self::Identity => w.u32(5),
            Self::Capacity => w.u32(6),
            Self::Config => w.u32(7),
            Self::WrongVersion(next) => {
                w.u32(8);
                w.u64(*next);
            }
            Self::Predecessor(found) => {
                w.u32(9);
                w.bytes(found);
            }
            Self::Unsettled => w.u32(10),
        }
    }

    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Held,
            1 => Self::Store,
            2 => Self::Place,
            3 => Self::Migration,
            4 => Self::Current,
            5 => Self::Identity,
            6 => Self::Capacity,
            7 => Self::Config,
            8 => Self::WrongVersion(r.u64()?),
            9 => Self::Predecessor(r.bytes()?),
            10 => Self::Unsettled,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "install refusal tag",
                ));
            }
        })
    }
}
