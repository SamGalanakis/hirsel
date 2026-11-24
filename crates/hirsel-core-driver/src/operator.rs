//! The operator wire on `STATE/operator.sock` (launch.md "The operator socket"), as the CLI
//! speaks it. These types mirror `core/programs/platform/operator.hirsel` and encode in the
//! same value codec as the launcher session; operator_wire.hirsel holds the byte vectors
//! both sides check. One connection carries one exchange.
use crate::code_admission::ObjectKey;
use crate::launcher::session::{
    Attempt, Bindings, Grants, Group, Input, Label, Limits, Reader, Wire, Writer, get_wanted,
    malformed, put_wanted,
};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::Path;

/// The largest frame either side accepts.
pub(crate) const MAX_FRAME: usize = 16 << 20;
/// The largest logical reply the CLI reassembles from `Part`s.
const MAX_REPLY: u64 = 64 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) dir: String,
    pub(crate) retention_s: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Registration {
    pub(crate) name: String,
    pub(crate) tenant: String,
    pub(crate) workdir: String,
    pub(crate) trusted: bool,
    pub(crate) label: Label,
    pub(crate) listen: String,
    pub(crate) bindings: Bindings,
    pub(crate) limits: Limits,
    pub(crate) args: Vec<String>,
    pub(crate) record: Option<Record>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    Begin(Vec<u8>, Group, Input, Vec<String>, Vec<String>),
    Data(Vec<u8>, Attempt, Vec<u8>),
    End(Vec<u8>, Attempt),
    Confirm(Vec<u8>, Attempt, Vec<u8>),
    Status(Vec<u8>),
    Resolve(Vec<u8>),
    Lookup(String),
    Register(Vec<u8>, Box<Registration>, Input, Vec<String>, Vec<String>),
    /// A person's decision on a request that awaits one for exactly this grant diff.
    Decide(Vec<u8>, Grants, bool),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    Found(Vec<u8>),
    Gone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pause {
    NotStarted,
    AwaitWrite,
    Measured(u64),
    MeasurementLost,
    StartFailed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) fetch_ns: Option<u64>,
    pub(crate) assemble_ns: Option<u64>,
    pub(crate) pause: Pause,
    pub(crate) preview: Preview,
}

/// What a deploy changes (launch.md "Deploy preview").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Preview {
    pub(crate) grants: Grants,
    pub(crate) entries: Vec<EntryLine>,
    pub(crate) triggers: Vec<TriggerLine>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryChange {
    Added,
    Removed,
    Changed,
    Unchanged,
}

const ENTRY_CHANGES: [EntryChange; 4] = [
    EntryChange::Added,
    EntryChange::Removed,
    EntryChange::Changed,
    EntryChange::Unchanged,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EntryLine {
    pub(crate) identity: Vec<u8>,
    pub(crate) change: EntryChange,
}

/// One group of pending triggers in the prepare child's report (`runner.TriggerScanRow`):
/// class tag (0 cancelling, 1 held, 2 gone, 3 changed, 4 unchanged), kind tag (0 compute,
/// 1 transact, 2 query, 3 granted), path, schema, count and earliest due time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TriggerLine {
    pub(crate) class: u32,
    pub(crate) kind: u32,
    pub(crate) path: Vec<u8>,
    pub(crate) schema: Vec<u8>,
    pub(crate) count: u64,
    pub(crate) earliest_due: u64,
}

impl Wire for Preview {
    fn put(&self, w: &mut Writer) {
        self.grants.put(w);
        w.u32(self.entries.len() as u32);
        for e in &self.entries {
            w.bytes(&e.identity);
            w.u32(ENTRY_CHANGES.iter().position(|c| *c == e.change).unwrap() as u32);
        }
        w.u32(self.triggers.len() as u32);
        for t in &self.triggers {
            w.u32(t.class);
            w.u32(t.kind);
            w.bytes(&t.path);
            w.bytes(&t.schema);
            w.u64(t.count);
            w.u64(t.earliest_due);
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        let grants = Grants::get(r)?;
        let n = r.u32()?;
        let mut entries = Vec::new();
        for _ in 0..n {
            let identity = r.bytes()?;
            let change = *ENTRY_CHANGES
                .get(r.u32()? as usize)
                .ok_or_else(|| malformed("entry change tag"))?;
            entries.push(EntryLine { identity, change });
        }
        let n = r.u32()?;
        let mut triggers = Vec::new();
        for _ in 0..n {
            let class = r.u32()?;
            let kind = r.u32()?;
            if class > 4 || kind > 3 {
                return Err(malformed("trigger report tag"));
            }
            triggers.push(TriggerLine {
                class,
                kind,
                path: r.bytes()?,
                schema: r.bytes()?,
                count: r.u64()?,
                earliest_due: r.u64()?,
            });
        }
        Ok(Self {
            grants,
            entries,
            triggers,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    Protocol,
    Denied,
    UnknownRequest,
    UnknownGroup,
    Conflict,
    Busy,
    Full,
    Invalid,
    Unassembled,
    Store,
    Uncompiled,
    NotInstalledAfterRecovery,
    Exhausted,
}

const CLASSES: [Class; 13] = [
    Class::Protocol,
    Class::Denied,
    Class::UnknownRequest,
    Class::UnknownGroup,
    Class::Conflict,
    Class::Busy,
    Class::Full,
    Class::Invalid,
    Class::Unassembled,
    Class::Store,
    Class::Uncompiled,
    Class::NotInstalledAfterRecovery,
    Class::Exhausted,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    AwaitInput(Attempt),
    Admitting(Attempt),
    AwaitConfirm(Attempt),
    Assembling(Attempt),
    Preparing(Attempt),
    Pending(Attempt, u64),
    Undecided(Attempt, u64, String),
    Installed(u64, Vec<u8>, Report),
    Failed(Class, String, Report),
    /// Waits, holding no attempt, for a person's decision on this grant diff.
    AwaitDecision(Grants),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    State(Vec<u8>, Group, Input, Phase),
    Need(Vec<u8>, Attempt, Vec<ObjectKey>, bool),
    Display(Vec<u8>, Attempt, Vec<u8>, String),
    Pin(Resolved),
    Error(Class, String),
    Group(Group),
    Part(u64, u64, Vec<u8>),
}

impl Wire for Registration {
    fn put(&self, w: &mut Writer) {
        w.bytes(self.name.as_bytes());
        w.bytes(self.tenant.as_bytes());
        w.bytes(self.workdir.as_bytes());
        w.bool(self.trusted);
        self.label.put(w);
        w.bytes(self.listen.as_bytes());
        self.bindings.put(w);
        self.limits.put(w);
        w.strings(&self.args);
        w.option(&self.record, |w, record| {
            w.bytes(record.dir.as_bytes());
            w.u64(record.retention_s);
        });
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(Self {
            name: r.string()?,
            tenant: r.string()?,
            workdir: r.string()?,
            trusted: r.bool()?,
            label: Label::get(r)?,
            listen: r.string()?,
            bindings: Bindings::get(r)?,
            limits: Limits::get(r)?,
            args: r.strings()?,
            record: r.option(|r| {
                Ok(Record {
                    dir: r.string()?,
                    retention_s: r.u64()?,
                })
            })?,
        })
    }
}

impl Wire for Ask {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::Begin(d, g, input, foreign, exec) => {
                w.u32(0);
                w.bytes(d);
                g.put(w);
                input.put(w);
                w.strings(foreign);
                w.strings(exec);
            }
            Self::Data(d, a, bytes) => {
                w.u32(1);
                w.bytes(d);
                a.put(w);
                w.bytes(bytes);
            }
            Self::End(d, a) => {
                w.u32(2);
                w.bytes(d);
                a.put(w);
            }
            Self::Confirm(d, a, hash) => {
                w.u32(3);
                w.bytes(d);
                a.put(w);
                w.bytes(hash);
            }
            Self::Status(d) => (w.u32(4), w.bytes(d)).1,
            Self::Resolve(id) => (w.u32(5), w.bytes(id)).1,
            Self::Lookup(name) => (w.u32(6), w.bytes(name.as_bytes())).1,
            Self::Register(d, registration, input, foreign, exec) => {
                w.u32(7);
                w.bytes(d);
                registration.put(w);
                input.put(w);
                w.strings(foreign);
                w.strings(exec);
            }
            Self::Decide(d, grants, approve) => {
                w.u32(8);
                w.bytes(d);
                grants.put(w);
                w.bool(*approve);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Begin(
                r.bytes()?,
                Group::get(r)?,
                Input::get(r)?,
                r.strings()?,
                r.strings()?,
            ),
            1 => Self::Data(r.bytes()?, Attempt::get(r)?, r.bytes()?),
            2 => Self::End(r.bytes()?, Attempt::get(r)?),
            3 => Self::Confirm(r.bytes()?, Attempt::get(r)?, r.bytes()?),
            4 => Self::Status(r.bytes()?),
            5 => Self::Resolve(r.bytes()?),
            6 => Self::Lookup(r.string()?),
            7 => Self::Register(
                r.bytes()?,
                Box::new(Registration::get(r)?),
                Input::get(r)?,
                r.strings()?,
                r.strings()?,
            ),
            8 => Self::Decide(r.bytes()?, Grants::get(r)?, r.bool()?),
            _ => return Err(malformed("operator ask tag")),
        })
    }
}

impl Wire for Class {
    fn put(&self, w: &mut Writer) {
        w.u32(CLASSES.iter().position(|c| c == self).unwrap() as u32);
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        CLASSES
            .get(r.u32()? as usize)
            .copied()
            .ok_or_else(|| malformed("operator class tag"))
    }
}

impl Wire for Report {
    fn put(&self, w: &mut Writer) {
        w.option(&self.fetch_ns, |w, &ns| w.u64(ns));
        w.option(&self.assemble_ns, |w, &ns| w.u64(ns));
        match &self.pause {
            Pause::NotStarted => w.u32(0),
            Pause::AwaitWrite => w.u32(1),
            Pause::Measured(ns) => (w.u32(2), w.u64(*ns)).1,
            Pause::MeasurementLost => w.u32(3),
            Pause::StartFailed(why) => (w.u32(4), w.bytes(why.as_bytes())).1,
        }
        self.preview.put(w);
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(Self {
            fetch_ns: r.option(Reader::u64)?,
            assemble_ns: r.option(Reader::u64)?,
            pause: match r.u32()? {
                0 => Pause::NotStarted,
                1 => Pause::AwaitWrite,
                2 => Pause::Measured(r.u64()?),
                3 => Pause::MeasurementLost,
                4 => Pause::StartFailed(r.string()?),
                _ => return Err(malformed("operator pause tag")),
            },
            preview: Preview::get(r)?,
        })
    }
}

impl Wire for Phase {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::AwaitInput(a) => (w.u32(0), a.put(w)).1,
            Self::Admitting(a) => (w.u32(1), a.put(w)).1,
            Self::AwaitConfirm(a) => (w.u32(2), a.put(w)).1,
            Self::Assembling(a) => (w.u32(3), a.put(w)).1,
            Self::Preparing(a) => (w.u32(4), a.put(w)).1,
            Self::Pending(a, v) => {
                w.u32(5);
                a.put(w);
                w.u64(*v);
            }
            Self::Undecided(a, v, why) => {
                w.u32(6);
                a.put(w);
                w.u64(*v);
                w.bytes(why.as_bytes());
            }
            Self::Installed(v, assembly, report) => {
                w.u32(7);
                w.u64(*v);
                w.bytes(assembly);
                report.put(w);
            }
            Self::Failed(class, detail, report) => {
                w.u32(8);
                class.put(w);
                w.bytes(detail.as_bytes());
                report.put(w);
            }
            Self::AwaitDecision(grants) => (w.u32(9), grants.put(w)).1,
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::AwaitInput(Attempt::get(r)?),
            1 => Self::Admitting(Attempt::get(r)?),
            2 => Self::AwaitConfirm(Attempt::get(r)?),
            3 => Self::Assembling(Attempt::get(r)?),
            4 => Self::Preparing(Attempt::get(r)?),
            5 => Self::Pending(Attempt::get(r)?, r.u64()?),
            6 => Self::Undecided(Attempt::get(r)?, r.u64()?, r.string()?),
            7 => Self::Installed(r.u64()?, r.bytes()?, Report::get(r)?),
            8 => Self::Failed(Class::get(r)?, r.string()?, Report::get(r)?),
            9 => Self::AwaitDecision(Grants::get(r)?),
            _ => return Err(malformed("operator phase tag")),
        })
    }
}

impl Wire for Answer {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::State(d, g, input, phase) => {
                w.u32(0);
                w.bytes(d);
                g.put(w);
                input.put(w);
                phase.put(w);
            }
            Self::Need(d, a, keys, last) => {
                w.u32(1);
                w.bytes(d);
                a.put(w);
                put_wanted(w, keys);
                w.bool(*last);
            }
            Self::Display(d, a, root, text) => {
                w.u32(2);
                w.bytes(d);
                a.put(w);
                w.bytes(root);
                w.bytes(text.as_bytes());
            }
            Self::Pin(resolved) => {
                w.u32(3);
                match resolved {
                    Resolved::Found(assembly) => (w.u32(0), w.bytes(assembly)).1,
                    Resolved::Gone => w.u32(1),
                }
            }
            Self::Error(class, detail) => {
                w.u32(4);
                class.put(w);
                w.bytes(detail.as_bytes());
            }
            Self::Group(g) => (w.u32(5), g.put(w)).1,
            Self::Part(total, offset, bytes) => {
                w.u32(6);
                w.u64(*total);
                w.u64(*offset);
                w.bytes(bytes);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::State(r.bytes()?, Group::get(r)?, Input::get(r)?, Phase::get(r)?),
            1 => Self::Need(r.bytes()?, Attempt::get(r)?, get_wanted(r)?, r.bool()?),
            2 => Self::Display(r.bytes()?, Attempt::get(r)?, r.bytes()?, r.string()?),
            3 => Self::Pin(match r.u32()? {
                0 => Resolved::Found(r.bytes()?),
                1 => Resolved::Gone,
                _ => return Err(malformed("operator pin tag")),
            }),
            4 => Self::Error(Class::get(r)?, r.string()?),
            5 => Self::Group(Group::get(r)?),
            6 => Self::Part(r.u64()?, r.u64()?, r.bytes()?),
            _ => return Err(malformed("operator answer tag")),
        })
    }
}

/// One operator connection.
pub(crate) struct Connection {
    stream: UnixStream,
}

/// The operator UID: this process's effective UID.
fn operator_uid() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

#[cfg(not(target_os = "linux"))]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: uid and gid are live result slots.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } == 0 {
        Ok(uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred and len are live, sized for SO_PEERCRED.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    if rc == 0 {
        Ok(cred.uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Connect to `state/operator.sock`. The state directory must be the operator's own and
/// private, and the listener must run as the operator.
pub(crate) fn connect(state: &Path) -> io::Result<Connection> {
    let meta = std::fs::metadata(state)?;
    if meta.uid() != operator_uid() || meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the host state directory is not the operator's private directory",
        ));
    }
    let stream = UnixStream::connect(state.join("operator.sock"))?;
    if peer_uid(&stream)? != operator_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the operator socket's listener is another user",
        ));
    }
    Ok(Connection { stream })
}

impl Connection {
    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
        })
    }

    pub(crate) fn set_read_timeout(&self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        hirsel_core_net::sys::timeout_set(self.stream.set_read_timeout(timeout))
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
        self.stream.write_all(&frame)
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
        let mut whole = Vec::with_capacity(total as usize);
        let mut next = Some((total, offset, chunk));
        while let Some((t, at, chunk)) = next.take() {
            if t != total || at != whole.len() as u64 || chunk.is_empty() {
                return Err(malformed("operator reply parts"));
            }
            whole.extend_from_slice(&chunk);
            if whole.len() as u64 > total {
                return Err(malformed("operator reply parts"));
            }
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
            match self.stream.read(&mut size[got..]) {
                Ok(0) if got == 0 => return Ok(None),
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => got += n,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
                Err(error) => return Err(error),
            }
        }
        let size = u32::from_le_bytes(size) as usize;
        if size > MAX_FRAME {
            return Err(malformed("operator frame too large"));
        }
        let mut body = vec![0; size];
        self.stream.read_exact(&mut body)?;
        Answer::decode(&body).map(Some)
    }

    pub(crate) fn shutdown(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_admission::ObjectKind;
    use hirsel_core_ids::ContentHash;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
    fn d() -> Vec<u8> {
        vec![0x21; 16]
    }
    fn group() -> Group {
        Group {
            name: "g".into(),
            incarnation: 3,
        }
    }
    fn attempt() -> Attempt {
        Attempt {
            hi: 0x0102030405060708,
            lo: 0x1112131415161718,
        }
    }
    fn state(phase: Phase) -> Answer {
        Answer::State(d(), group(), Input::Publication(vec![1]), phase)
    }
    fn report(pause: Pause) -> Report {
        Report {
            fetch_ns: None,
            assemble_ns: None,
            pause,
            preview: Preview::default(),
        }
    }
    fn grants() -> Grants {
        Grants {
            added: vec!["Clock".into()],
            removed: vec!["Dir=a".into()],
        }
    }

    // The expected bytes are shared with core/programs/platform/operator_wire.hirsel. They were
    // derived from core/std/api/codec.md's format by an encoder written apart from both
    // implementations.
    #[test]
    fn operator_vectors() {
        let asks = [
            (
                Ask::Begin(
                    d(),
                    group(),
                    Input::Attested(vec![0xcc; 32]),
                    vec!["f".into()],
                    vec!["e".into()],
                ),
                "000000001000000021212121212121212121212121212121010000006703000000000000000000000020000000cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc010000000100000066010000000100000065",
            ),
            (
                Ask::Data(d(), attempt(), vec![1, 2]),
                "01000000100000002121212121212121212121212121212108070605040302011817161514131211020000000102",
            ),
            (
                Ask::End(d(), attempt()),
                "02000000100000002121212121212121212121212121212108070605040302011817161514131211",
            ),
            (
                Ask::Confirm(d(), attempt(), vec![0xdd; 32]),
                "0300000010000000212121212121212121212121212121210807060504030201181716151413121120000000dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            ),
            (
                Ask::Status(d()),
                "040000001000000021212121212121212121212121212121",
            ),
            (
                Ask::Resolve(vec![0x31; 16]),
                "050000001000000031313131313131313131313131313131",
            ),
            (Ask::Lookup("g".into()), "060000000100000067"),
            (
                Ask::Decide(d(), grants(), true),
                "0800000010000000212121212121212121212121212121210100000005000000436c6f636b01000000050000004469723d6101",
            ),
            (
                Ask::Register(
                    d(),
                    Box::new(Registration {
                        name: "g".into(),
                        tenant: "t".into(),
                        workdir: "/w".into(),
                        trusted: false,
                        label: Label::Uncontained,
                        listen: "127.0.0.1:9".into(),
                        bindings: Bindings {
                            store: "/store".into(),
                            dirs: vec!["data".into()],
                            net: Vec::new(),
                        },
                        limits: Limits {
                            memory: Some(4096),
                            cpu: None,
                            pids: Some(8),
                        },
                        args: vec!["--port".into()],
                        record: Some(Record {
                            dir: "/tapes".into(),
                            retention_s: 60,
                        }),
                    }),
                    Input::Publication(vec![1]),
                    Vec::new(),
                    Vec::new(),
                ),
                "07000000100000002121212121212121212121212121212101000000670100000074020000002f7700010000000b0000003132372e302e302e313a39060000002f73746f7265010000000400000064617461000000000100000000100000000000000000000001000000080000000000000001000000060000002d2d706f727401000000060000002f74617065733c000000000000000100000001000000010000000000000000",
            ),
        ];
        let answers = [
            (
                Answer::State(
                    d(),
                    group(),
                    Input::Attested(vec![0xcc; 32]),
                    Phase::AwaitInput(attempt()),
                ),
                "000000001000000021212121212121212121212121212121010000006703000000000000000000000020000000cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc0000000008070605040302011817161514131211",
            ),
            (
                state(Phase::Pending(attempt(), 5)),
                "0000000010000000212121212121212121212121212121210100000067030000000000000001000000010000000105000000080706050403020118171615141312110500000000000000",
            ),
            (
                state(Phase::Undecided(attempt(), 5, "lost".into())),
                "0000000010000000212121212121212121212121212121210100000067030000000000000001000000010000000106000000080706050403020118171615141312110500000000000000040000006c6f7374",
            ),
            (
                state(Phase::Installed(
                    5,
                    vec![0xaa; 32],
                    Report {
                        fetch_ns: Some(7),
                        assemble_ns: None,
                        pause: Pause::Measured(2000),
                        preview: Preview {
                            grants: Grants {
                                added: vec!["Clock".into()],
                                removed: Vec::new(),
                            },
                            entries: vec![EntryLine {
                                identity: vec![0xee],
                                change: EntryChange::Changed,
                            }],
                            triggers: vec![TriggerLine {
                                class: 1,
                                kind: 1,
                                path: vec![1],
                                schema: vec![2],
                                count: 3,
                                earliest_due: 4,
                            }],
                        },
                    },
                )),
                "0000000010000000212121212121212121212121212121210100000067030000000000000001000000010000000107000000050000000000000020000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa0100000007000000000000000000000002000000d0070000000000000100000005000000436c6f636b000000000100000001000000ee020000000100000001000000010000000100000001010000000203000000000000000400000000000000",
            ),
            (
                state(Phase::Installed(
                    0,
                    vec![0xaa; 32],
                    report(Pause::StartFailed("x".into())),
                )),
                "0000000010000000212121212121212121212121212121210100000067030000000000000001000000010000000107000000000000000000000020000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa000000000000000004000000010000007800000000000000000000000000000000",
            ),
            (
                state(Phase::Failed(
                    Class::Denied,
                    "no".into(),
                    report(Pause::NotStarted),
                )),
                "000000001000000021212121212121212121212121212121010000006703000000000000000100000001000000010800000001000000020000006e6f00000000000000000000000000000000000000000000000000000000",
            ),
            (
                state(Phase::AwaitDecision(grants())),
                "00000000100000002121212121212121212121212121212101000000670300000000000000010000000100000001090000000100000005000000436c6f636b01000000050000004469723d61",
            ),
            (
                Answer::Need(
                    d(),
                    attempt(),
                    vec![ObjectKey {
                        kind: ObjectKind::Group,
                        hash: ContentHash([0xaa; 32]),
                    }],
                    true,
                ),
                "01000000100000002121212121212121212121212121212108070605040302011817161514131211010000000000000020000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa01",
            ),
            (
                Answer::Display(d(), attempt(), vec![0xcc; 32], "foreign: x\n".into()),
                "0200000010000000212121212121212121212121212121210807060504030201181716151413121120000000cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc0b000000666f726569676e3a20780a",
            ),
            (
                Answer::Pin(Resolved::Found(vec![0xaa; 32])),
                "030000000000000020000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            (Answer::Pin(Resolved::Gone), "0300000001000000"),
            (
                Answer::Error(Class::NotInstalledAfterRecovery, "x".into()),
                "040000000b0000000100000078",
            ),
            (Answer::Group(group()), "0500000001000000670300000000000000"),
            (
                Answer::Part(10, 0, vec![1, 2]),
                "060000000a000000000000000000000000000000020000000102",
            ),
        ];
        for (value, expected) in &asks {
            assert_eq!(value.encode(), hex(expected), "{value:?}");
            assert_eq!(&Ask::decode(&hex(expected)).unwrap(), value);
        }
        for (value, expected) in &answers {
            assert_eq!(value.encode(), hex(expected), "{value:?}");
            assert_eq!(&Answer::decode(&hex(expected)).unwrap(), value);
        }
    }
}
