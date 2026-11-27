//! The operator wire on `STATE/operator.sock` (launch.md "The operator socket"), as the CLI
//! speaks it. The build derives the types and codecs from the declarations in
//! `core/programs/platform/operator.hirsel`. One connection carries one exchange.
use crate::code_admission::ObjectKey;
use crate::launcher::session::{
    Attempt, Grants, Group, Reader, Wire, Writer, get_object_key, malformed, put_object_key,
};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::Path;

mod wire;
include!(concat!(env!("OUT_DIR"), "/operator_wire.rs"));

impl From<Input> for BeginInput {
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

/// The largest frame either side accepts.
pub(crate) const MAX_FRAME: usize = 16 << 20;
/// The largest logical reply the CLI reassembles from `Part`s.
const MAX_REPLY: u64 = 64 << 20;

/// The fate of a held run (RUNNER-SPEC §14.6, `runner.fate.HeldFate`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeldFate {
    Release,
    Cancel,
}

impl HeldFate {
    fn put(self, w: &mut Writer) {
        w.u32(match self {
            Self::Release => 0,
            Self::Cancel => 1,
        });
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        match r.u32()? {
            0 => Ok(Self::Release),
            1 => Ok(Self::Cancel),
            _ => Err(malformed("held fate tag")),
        }
    }
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Cancel => "cancel",
        }
    }
}

/// A deploy's fates, Begin's `held` (`runner.fate.Fates`): `all`, and `entries` sorted by
/// path with no path repeated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Fates {
    pub(crate) all: Option<HeldFate>,
    pub(crate) entries: Vec<(Vec<u8>, HeldFate)>,
}

impl Wire for Fates {
    fn put(&self, w: &mut Writer) {
        w.option(&self.all, |w, f| f.put(w));
        w.u32(self.entries.len() as u32);
        for (path, fate) in &self.entries {
            w.bytes(path);
            fate.put(w);
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        let all = r.option(HeldFate::get)?;
        let n = r.u32()?;
        let mut entries = Vec::new();
        for _ in 0..n {
            entries.push((r.bytes()?, HeldFate::get(r)?));
        }
        Ok(Self { all, entries })
    }
}

/// What holds a held group (`runner.fate.HeldBy`): a cut run, or a `Revision` or `Request`
/// hold, with whether the row's stamp equals NEW's identity digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeldBy {
    Cut,
    Revision(bool),
    Request(bool),
}

/// One group of pending triggers in the prepare child's report (`runner.TriggerScanRow`):
/// class tag (0 cancelling, 1 held, 2 gone, 3 changed, 4 unchanged), kind tag (0 compute,
/// 1 transact, 2 query, 3 granted), path, schema, the hold and fate of a held group, count
/// and earliest due time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TriggerLine {
    pub(crate) class: u32,
    pub(crate) kind: u32,
    pub(crate) path: Vec<u8>,
    pub(crate) schema: Vec<u8>,
    pub(crate) hold: Option<HeldBy>,
    pub(crate) fate: Option<HeldFate>,
    pub(crate) count: u64,
    pub(crate) earliest_due: u64,
}

/// The prepare child's trigger report (`runner.TriggerReport`): its groups, and each held
/// entry path that names no pending run and no entry OLD or NEW registers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TriggerReport {
    pub(crate) rows: Vec<TriggerLine>,
    pub(crate) unmatched: Vec<Vec<u8>>,
}

impl Wire for TriggerReport {
    fn put(&self, w: &mut Writer) {
        w.u32(self.rows.len() as u32);
        for t in &self.rows {
            w.u32(t.class);
            w.u32(t.kind);
            w.bytes(&t.path);
            w.bytes(&t.schema);
            w.option(&t.hold, |w, hold| match *hold {
                HeldBy::Cut => w.u32(0),
                HeldBy::Revision(same) => {
                    w.u32(1);
                    w.bool(same);
                }
                HeldBy::Request(same) => {
                    w.u32(2);
                    w.bool(same);
                }
            });
            w.option(&t.fate, |w, f| f.put(w));
            w.u64(t.count);
            w.u64(t.earliest_due);
        }
        w.u32(self.unmatched.len() as u32);
        for path in &self.unmatched {
            w.bytes(path);
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        let n = r.u32()?;
        let mut rows = Vec::new();
        for _ in 0..n {
            let class = r.u32()?;
            let kind = r.u32()?;
            if class > 4 || kind > 3 {
                return Err(malformed("trigger report tag"));
            }
            let path = r.bytes()?;
            let schema = r.bytes()?;
            let hold = r.option(|r| match r.u32()? {
                0 => Ok(HeldBy::Cut),
                1 => Ok(HeldBy::Revision(r.bool()?)),
                2 => Ok(HeldBy::Request(r.bool()?)),
                _ => Err(malformed("held-by tag")),
            })?;
            rows.push(TriggerLine {
                class,
                kind,
                path,
                schema,
                hold,
                fate: r.option(HeldFate::get)?,
                count: r.u64()?,
                earliest_due: r.u64()?,
            });
        }
        let n = r.u32()?;
        let mut unmatched = Vec::new();
        for _ in 0..n {
            unmatched.push(r.bytes()?);
        }
        Ok(Self { rows, unmatched })
    }
}

impl Decision {
    /// Whether the decision shows a held group or an unmatched path, which its Decide fates.
    pub(crate) fn shows_held(&self) -> bool {
        !self.triggers.unmatched.is_empty() || self.triggers.rows.iter().any(|t| t.class == 1)
    }
}

/// One key a `Need` asks for (`launcher.Wanted`): an object the launcher's verifier lacks,
/// or a source blob, by SHA-256, the group's blob set lacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Needed {
    Object(ObjectKey),
    Blob([u8; 32]),
}

const BLOB_TAG: u32 = 5;

impl Wire for Vec<Needed> {
    fn put(&self, w: &mut Writer) {
        w.len(self.len());
        for key in self {
            match key {
                Needed::Object(key) => put_object_key(w, key),
                Needed::Blob(digest) => {
                    w.u32(BLOB_TAG);
                    w.bytes(digest);
                }
            }
        }
    }

    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        (0..r.count(8)?)
            .map(|_| match r.u32()? {
                BLOB_TAG => Ok(Needed::Blob(
                    <[u8; 32]>::try_from(r.bytes()?)
                        .map_err(|_| malformed("needed blob digest"))?,
                )),
                tag => Ok(Needed::Object(get_object_key(tag, r)?)),
            })
            .collect()
    }
}

/// One operator connection.
pub(crate) struct Connection {
    stream: UnixStream,
    observer: Option<crate::operator_cli::Observer>,
}

/// The operator UID: this process's effective UID.
pub(crate) fn operator_uid() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

use crate::host::peer_uid;

pub(crate) fn check_state(state: &Path) -> io::Result<()> {
    let meta = std::fs::metadata(state)?;
    if !meta.is_dir() || meta.uid() != operator_uid() || meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the host state directory is not the operator's private directory",
        ));
    }
    Ok(())
}

/// Connect to `state/operator.sock`. The state directory must be the operator's own and
/// private, and the listener must run as the operator.
pub(crate) fn connect(state: &Path) -> io::Result<Connection> {
    check_state(state)?;
    let stream = UnixStream::connect(state.join(crate::product::bundled::OPERATOR_SOCKET))?;
    if peer_uid(&stream)? != operator_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the operator socket's listener is another user",
        ));
    }
    Ok(Connection {
        stream,
        observer: None,
    })
}

impl Connection {
    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
            observer: self.observer,
        })
    }

    pub(crate) fn set_read_timeout(&self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        crate::host::socket_option(self.stream.set_read_timeout(timeout))
    }

    pub(crate) fn observe(&mut self, observer: crate::operator_cli::Observer) -> io::Result<()> {
        let interval = Some(std::time::Duration::from_millis(100));
        self.stream.set_read_timeout(interval)?;
        self.stream.set_write_timeout(interval)?;
        self.observer = Some(observer);
        Ok(())
    }

    fn io<T>(&mut self, mut action: impl FnMut(&mut UnixStream) -> io::Result<T>) -> io::Result<T> {
        loop {
            if let Some(observer) = self.observer {
                observer.check()?;
            }
            match action(&mut self.stream) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e)
                    if self.observer.is_some()
                        && matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }

    pub(crate) fn send(&mut self, ask: &Ask) -> io::Result<()> {
        let body = ask.encode();
        if body.len() > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "operator frame too large",
            ));
        }
        let mut frame = Vec::with_capacity(4 + body.len());
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(&body);
        let mut rest = frame.as_slice();
        while !rest.is_empty() {
            let count = self.io(|stream| stream.write(rest))?;
            if count == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            rest = &rest[count..];
        }
        Ok(())
    }

    /// The next answer, reassembled from its `Part`s; None at a clean end between frames.
    pub(crate) fn recv(&mut self) -> io::Result<Option<Answer>> {
        let Some(first) = self.frame()? else {
            return Ok(None);
        };
        let Answer::Part(total, offset, chunk) = first else {
            return Ok(Some(first));
        };
        if total > MAX_REPLY || offset != 0 {
            return Err(malformed("operator reply parts"));
        }
        let mut whole = Vec::new();
        let mut next = Some((total, offset, chunk));
        while let Some((t, at, chunk)) = next.take() {
            if t != total
                || at != whole.len() as u64
                || chunk.is_empty()
                || chunk.len() as u64 > total - at
            {
                return Err(malformed("operator reply parts"));
            }
            let needed = whole.len() + chunk.len();
            if needed > whole.capacity() {
                let capacity = needed.max(2 * whole.capacity()).min(total as usize);
                whole
                    .try_reserve_exact(capacity - whole.len())
                    .map_err(io::Error::other)?;
            }
            whole.extend_from_slice(&chunk);
            if whole.len() as u64 == total {
                break;
            }
            match self.frame()? {
                Some(Answer::Part(t, at, chunk)) => next = Some((t, at, chunk)),
                _ => return Err(malformed("operator reply ended inside its parts")),
            }
        }
        match Answer::decode(&whole)? {
            Answer::Part(..) => Err(malformed("operator reply parts nest")),
            answer => Ok(Some(answer)),
        }
    }

    fn frame(&mut self) -> io::Result<Option<Answer>> {
        let mut size = [0; 4];
        let mut got = 0;
        while got < 4 {
            match self.io(|stream| stream.read(&mut size[got..])) {
                Ok(0) if got == 0 => return Ok(None),
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => got += n,
                Err(error) => return Err(error),
            }
        }
        let size = u32::from_le_bytes(size) as usize;
        if size > MAX_FRAME {
            return Err(malformed("operator frame too large"));
        }
        let mut body = vec![0; size];
        let mut rest = body.as_mut_slice();
        while !rest.is_empty() {
            let count = self.io(|stream| stream.read(rest))?;
            if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            rest = &mut rest[count..];
        }
        Answer::decode(&body).map(Some)
    }

    pub(crate) fn shutdown(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(test)]
mod tests;
