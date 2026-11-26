//! The control session between the platform host and the launcher.
//!
//! These types mirror `core/programs/platform/launcher.hirsel` and encode in its value codec
//! (core/std/api/codec.md): integers little-endian at their width, bool one byte,
//! String/Bytes/List a u32 count then the contents, Option and sums a u32 tag then the
//! payload, and records their fields in order. Declared variant order is the tag. Each
//! frame on the socket is a u32 little-endian length followed by one value.
use crate::code_admission::ObjectKey;
use std::io;

pub(crate) use super::grants::Grants;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Attempt {
    pub(crate) hi: u64,
    pub(crate) lo: u64,
}

impl Attempt {
    pub(crate) fn bytes(self) -> [u8; 16] {
        let mut out = [0; 16];
        out[..8].copy_from_slice(&self.hi.to_le_bytes());
        out[8..].copy_from_slice(&self.lo.to_le_bytes());
        out
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Group {
    pub(crate) name: String,
    pub(crate) incarnation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Role {
    Install(u64),
    Serving(u64),
    Describe(Attempt),
    Preview(Attempt),
    Prepare(Attempt),
}

impl Role {
    /// The store-holding roles: each takes the group's next `gen` (§14.1).
    pub(crate) fn holds_store(self) -> bool {
        matches!(self, Self::Install(_) | Self::Serving(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Child {
    pub(crate) group: Group,
    pub(crate) role: Role,
}

/// A recorded serving launch: the 16-byte recording ID whose catalog pin the host made
/// durable before this start, and the designated recording destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Recording {
    pub(crate) id: Vec<u8>,
    pub(crate) destination: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Launch {
    pub(crate) group: Group,
    pub(crate) assembly: Vec<u8>,
    pub(crate) workdir: String,
    pub(crate) args: Vec<String>,
    pub(crate) mode: Mode,
    pub(crate) recording: Option<Recording>,
}

impl Launch {
    pub(crate) fn child(&self) -> Child {
        Child {
            group: self.group.clone(),
            role: self.mode.role(),
        }
    }
}

/// runner's `HostCommand`, relayed to a serving child's private endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostCommand {
    Export(Vec<u8>, bool),
    Import(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>),
    Cutover(Vec<u8>, Vec<u8>, u64, bool),
    Abort(Vec<u8>, Option<Vec<u8>>),
    StoreAccepted,
    Backup(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlaceReady {
    pub(crate) version: u64,
    pub(crate) store_hi: u64,
    pub(crate) store_lo: u64,
    pub(crate) release: Vec<u8>,
    pub(crate) retain_from: u64,
    pub(crate) ready_ns: u64,
    pub(crate) read: Vec<u8>,
}

/// runner's `HostReply`, a serving child's answer or notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostReply {
    Opened(PlaceReady),
    Current(u64, Vec<u8>),
    Refused(String),
    Exported(u64, String),
    /// The installed hash, then OLD's migration of the bound PREPARED: its digest and
    /// changed catalog keys.
    Imported(Vec<u8>, Vec<u8>, Vec<Vec<u8>>),
    Installed(u64, bool),
    Drift,
    Aborted,
    Uncertain,
    WriteAck(u64),
    StoreMismatch(u64, u64, u64, u64),
    InstallRefused(InstallRefusal),
    BackedUp(u64, u64, u64, String),
    Deposed(u64, Vec<u8>),
}

pub(crate) use super::places::InstallRefusal;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Hello,
    Bind(Group, String),
    Close(Group),
    Start(Launch),
    Stop(Child),
    Kill(Child),
    Acknowledge(Child),
    Command(Child, HostCommand),
    Retire(Group),
    Release(Attempt),
    CollectBegin,
    Collect(Vec<Vec<u8>>),
    Admit(Group, Attempt, Input, Approvals, u64),
    AdmitData(Attempt, Vec<u8>),
    AdmitEnd(Attempt),
    Assemble(Attempt),
    CutoverAck(Child, Attempt),
    CollectAbort,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Code(i32),
    Signal(i32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct End {
    pub(crate) status: Status,
    pub(crate) report: Option<Option<u64>>,
    pub(crate) readable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Started,
    Exited(End),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    pub(crate) activation: u64,
    pub(crate) assembly: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChildState {
    pub(crate) child: Child,
    pub(crate) phase: Phase,
    pub(crate) selection: Option<Selection>,
    pub(crate) answer: Option<HostReply>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    Held,
    Retiring,
    UnknownGroup,
    Mismatch,
    Image(String),
    Spawn(String),
    Unbuilt(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Inventory(Vec<ChildState>, Vec<Group>, Vec<(Child, Attempt)>),
    Launched(Child, Phase),
    Refused(Child, Refusal),
    Ended(Child, End),
    Arrived(Group),
    Bound(Group),
    BindFailed(Group, String),
    Closed(Group),
    Retired(Group),
    Placed(Child, HostReply),
    Fenced,
    Collected(u64),
    Answered(Child, Vec<u8>),
    Need(Attempt, Vec<ObjectKey>, bool),
    Admitted(Attempt, Vec<u8>, Summary, String),
    Assembled(Attempt, Vec<u8>, Vec<u8>, u64, u64),
    AdmissionFailed(Attempt, Failure),
    CutoverResult(Child, Attempt, HostReply),
}

pub(crate) fn malformed(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| malformed("truncated session value"))?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }
    pub(crate) fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(crate) fn i32(&mut self) -> io::Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(crate) fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub(crate) fn bool(&mut self) -> io::Result<bool> {
        match self.take(1)?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(malformed("session bool")),
        }
    }
    pub(crate) fn bytes(&mut self) -> io::Result<Vec<u8>> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    pub(crate) fn string(&mut self) -> io::Result<String> {
        String::from_utf8(self.bytes()?).map_err(|_| malformed("session string"))
    }
    pub(crate) fn strings(&mut self) -> io::Result<Vec<String>> {
        (0..self.count(4)?).map(|_| self.string()).collect()
    }
    pub(crate) fn count(&mut self, least: usize) -> io::Result<usize> {
        // A count can claim no more elements than the bytes left can hold.
        let n = self.u32()? as usize;
        if n.saturating_mul(least) > self.bytes.len() - self.at {
            return Err(malformed("session count"));
        }
        Ok(n)
    }
    pub(crate) fn option<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> io::Result<T>,
    ) -> io::Result<Option<T>> {
        match self.u32()? {
            0 => Ok(None),
            1 => f(self).map(Some),
            _ => Err(malformed("session option tag")),
        }
    }
}

#[derive(Default)]
pub(crate) struct Writer(pub(crate) Vec<u8>);

impl Writer {
    pub(crate) fn u32(&mut self, x: u32) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    pub(crate) fn i32(&mut self, x: i32) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    pub(crate) fn u64(&mut self, x: u64) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    pub(crate) fn bool(&mut self, x: bool) {
        self.0.push(u8::from(x));
    }
    pub(crate) fn len(&mut self, n: usize) {
        self.u32(u32::try_from(n).expect("session count fits u32"));
    }
    pub(crate) fn bytes(&mut self, x: &[u8]) {
        self.len(x.len());
        self.0.extend_from_slice(x);
    }
    pub(crate) fn strings(&mut self, strings: &[String]) {
        self.len(strings.len());
        for text in strings {
            self.bytes(text.as_bytes());
        }
    }
    pub(crate) fn option<T>(&mut self, x: &Option<T>, f: impl FnOnce(&mut Self, &T)) {
        match x {
            None => self.u32(0),
            Some(value) => {
                self.u32(1);
                f(self, value);
            }
        }
    }
}

pub(crate) trait Wire: Sized {
    fn put(&self, w: &mut Writer);
    fn get(r: &mut Reader<'_>) -> io::Result<Self>;

    fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        self.put(&mut w);
        w.0
    }
    fn decode(bytes: &[u8]) -> io::Result<Self> {
        let mut r = Reader { bytes, at: 0 };
        let value = Self::get(&mut r)?;
        if r.at != bytes.len() {
            return Err(malformed("trailing session bytes"));
        }
        Ok(value)
    }
}

impl Wire for Attempt {
    fn put(&self, w: &mut Writer) {
        w.u64(self.hi);
        w.u64(self.lo);
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(Self {
            hi: r.u64()?,
            lo: r.u64()?,
        })
    }
}

impl Wire for Group {
    fn put(&self, w: &mut Writer) {
        w.bytes(self.name.as_bytes());
        w.u64(self.incarnation);
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(Self {
            name: r.string()?,
            incarnation: r.u64()?,
        })
    }
}

impl Wire for Role {
    fn put(&self, w: &mut Writer) {
        match *self {
            Self::Install(g) => (w.u32(0), w.u64(g)).1,
            Self::Serving(g) => (w.u32(1), w.u64(g)).1,
            Self::Describe(a) => (w.u32(2), a.put(w)).1,
            Self::Preview(a) => (w.u32(3), a.put(w)).1,
            Self::Prepare(a) => (w.u32(4), a.put(w)).1,
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Install(r.u64()?),
            1 => Self::Serving(r.u64()?),
            2 => Self::Describe(Attempt::get(r)?),
            3 => Self::Preview(Attempt::get(r)?),
            4 => Self::Prepare(Attempt::get(r)?),
            _ => return Err(malformed("role tag")),
        })
    }
}

impl Wire for Child {
    fn put(&self, w: &mut Writer) {
        self.group.put(w);
        self.role.put(w);
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(Self {
            group: Group::get(r)?,
            role: Role::get(r)?,
        })
    }
}

impl Wire for Launch {
    fn put(&self, w: &mut Writer) {
        self.group.put(w);
        w.bytes(&self.assembly);
        w.bytes(self.workdir.as_bytes());
        w.strings(&self.args);
        self.mode.put(w);
        w.option(&self.recording, |w, recording| {
            w.bytes(&recording.id);
            w.bytes(recording.destination.as_bytes());
        });
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        let group = Group::get(r)?;
        let assembly = r.bytes()?;
        let workdir = r.string()?;
        let args = r.strings()?;
        Ok(Self {
            group,
            assembly,
            workdir,
            args,
            mode: Mode::get(r)?,
            recording: r.option(|r| {
                Ok(Recording {
                    id: r.bytes()?,
                    destination: r.string()?,
                })
            })?,
        })
    }
}

#[path = "session_host.rs"]
mod session_host;

#[path = "admission.rs"]
mod admission;
pub(crate) use admission::{Approvals, Failure, Green, Input, Summary};

#[path = "wanted.rs"]
mod wanted;
pub(crate) use wanted::*;

// Each row supplies the tag to both directions of the codec.
macro_rules! session_wire {
    ($ty:ty, $error:literal, $w:ident, $r:ident;
        $($tag:literal => $variant:ident $(($($field:ident),+))? $($put:block)? => $get:expr),+ $(,)?
    ) => {
        impl Wire for $ty {
            fn put(&self, $w: &mut Writer) {
                #[repr(u32)]
                enum Tag {
                    $($variant = $tag),+
                }

                match self {
                    $(Self::$variant $(($($field),+))? => {
                        $w.u32(Tag::$variant as u32);
                        $($put)?
                    }),+
                }
            }

            fn get($r: &mut Reader<'_>) -> io::Result<Self> {
                Ok(match $r.u32()? {
                    $($tag => $get,)+
                    _ => return Err(malformed($error)),
                })
            }
        }
    };
}

session_wire!(Request, "request tag", w, r;
    0 => Hello => Self::Hello,
    1 => Bind(g, address) {
        g.put(w);
        w.bytes(address.as_bytes());
    } => Self::Bind(Group::get(r)?, r.string()?),
    2 => Close(g) { g.put(w); } => Self::Close(Group::get(r)?),
    3 => Start(launch) { launch.put(w); } => Self::Start(Launch::get(r)?),
    4 => Stop(c) { c.put(w); } => Self::Stop(Child::get(r)?),
    5 => Kill(c) { c.put(w); } => Self::Kill(Child::get(r)?),
    6 => Acknowledge(c) { c.put(w); } => Self::Acknowledge(Child::get(r)?),
    7 => Command(c, command) {
        c.put(w);
        command.put(w);
    } => Self::Command(Child::get(r)?, HostCommand::get(r)?),
    8 => Retire(g) { g.put(w); } => Self::Retire(Group::get(r)?),
    9 => Release(a) { a.put(w); } => Self::Release(Attempt::get(r)?),
    10 => CollectBegin => Self::CollectBegin,
    11 => Collect(roots) {
        w.len(roots.len());
        for root in roots {
            w.bytes(root);
        }
    } => {
        let n = r.count(4)?;
        Self::Collect((0..n).map(|_| r.bytes()).collect::<io::Result<_>>()?)
    },
    12 => Admit(g, a, input, approvals, deadline_ms) {
        g.put(w);
        a.put(w);
        input.put(w);
        w.bool(approvals.recording);
        w.strings(&approvals.foreign);
        w.strings(&approvals.exec);
        w.bytes(approvals.workdir.as_bytes());
        w.option(&approvals.old, |w, old| w.bytes(old));
        put_roots(w, &approvals.packages);
        w.u64(*deadline_ms);
    } => Self::Admit(
        Group::get(r)?,
        Attempt::get(r)?,
        Input::get(r)?,
        Approvals {
            recording: r.bool()?,
            foreign: r.strings()?,
            exec: r.strings()?,
            workdir: r.string()?,
            old: r.option(Reader::bytes)?,
            packages: get_roots(r)?,
        },
        r.u64()?,
    ),
    13 => AdmitData(a, bytes) {
        a.put(w);
        w.bytes(bytes);
    } => Self::AdmitData(Attempt::get(r)?, r.bytes()?),
    14 => AdmitEnd(a) { a.put(w); } => Self::AdmitEnd(Attempt::get(r)?),
    15 => Assemble(a) { a.put(w); } => Self::Assemble(Attempt::get(r)?),
    16 => CutoverAck(c, a) {
        c.put(w);
        a.put(w);
    } => Self::CutoverAck(Child::get(r)?, Attempt::get(r)?),
    17 => CollectAbort => Self::CollectAbort,
);

impl Wire for End {
    fn put(&self, w: &mut Writer) {
        match self.status {
            Status::Code(code) => (w.u32(0), w.i32(code)).1,
            Status::Signal(signal) => (w.u32(1), w.i32(signal)).1,
        }
        w.option(&self.report, |w, report| {
            w.option(report, |w, &due| w.u64(due));
        });
        w.bool(self.readable);
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        let status = match r.u32()? {
            0 => Status::Code(r.i32()?),
            1 => Status::Signal(r.i32()?),
            _ => return Err(malformed("status tag")),
        };
        Ok(Self {
            status,
            report: r.option(|r| r.option(Reader::u64))?,
            readable: r.bool()?,
        })
    }
}

impl Wire for Phase {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::Started => w.u32(0),
            Self::Exited(end) => (w.u32(1), end.put(w)).1,
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Started,
            1 => Self::Exited(End::get(r)?),
            _ => return Err(malformed("phase tag")),
        })
    }
}

impl Wire for Refusal {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::Held => w.u32(0),
            Self::Retiring => w.u32(1),
            Self::UnknownGroup => w.u32(2),
            Self::Mismatch => w.u32(3),
            Self::Image(why) => (w.u32(4), w.bytes(why.as_bytes())).1,
            Self::Spawn(why) => (w.u32(5), w.bytes(why.as_bytes())).1,
            Self::Unbuilt(why) => (w.u32(6), w.bytes(why.as_bytes())).1,
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Held,
            1 => Self::Retiring,
            2 => Self::UnknownGroup,
            3 => Self::Mismatch,
            4 => Self::Image(r.string()?),
            5 => Self::Spawn(r.string()?),
            6 => Self::Unbuilt(r.string()?),
            _ => return Err(malformed("refusal tag")),
        })
    }
}

session_wire!(Event, "event tag", w, r;
    0 => Inventory(children, readable, cutovers) {
        w.len(children.len());
        for state in children {
            state.child.put(w);
            state.phase.put(w);
            w.option(&state.selection, |w, selection| {
                w.u64(selection.activation);
                w.bytes(&selection.assembly);
            });
            w.option(&state.answer, |w, answer| answer.put(w));
        }
        w.len(readable.len());
        for g in readable {
            g.put(w);
        }
        w.len(cutovers.len());
        for (c, a) in cutovers {
            c.put(w);
            a.put(w);
        }
    } => {
        let n = r.count(4)?;
        let children = (0..n)
            .map(|_| {
                Ok(ChildState {
                    child: Child::get(r)?,
                    phase: Phase::get(r)?,
                    selection: r.option(|r| Ok(Selection {
                        activation: r.u64()?,
                        assembly: r.bytes()?,
                    }))?,
                    answer: r.option(HostReply::get)?,
                })
            })
            .collect::<io::Result<_>>()?;
        let n = r.count(4)?;
        let readable = (0..n).map(|_| Group::get(r)).collect::<io::Result<_>>()?;
        let n = r.count(4)?;
        let cutovers = (0..n)
            .map(|_| Ok((Child::get(r)?, Attempt::get(r)?)))
            .collect::<io::Result<_>>()?;
        Self::Inventory(children, readable, cutovers)
    },
    1 => Launched(c, phase) {
        c.put(w);
        phase.put(w);
    } => Self::Launched(Child::get(r)?, Phase::get(r)?),
    2 => Refused(c, why) {
        c.put(w);
        why.put(w);
    } => Self::Refused(Child::get(r)?, Refusal::get(r)?),
    3 => Ended(c, end) {
        c.put(w);
        end.put(w);
    } => Self::Ended(Child::get(r)?, End::get(r)?),
    4 => Arrived(g) { g.put(w); } => Self::Arrived(Group::get(r)?),
    5 => Bound(g) { g.put(w); } => Self::Bound(Group::get(r)?),
    6 => BindFailed(g, why) {
        g.put(w);
        w.bytes(why.as_bytes());
    } => Self::BindFailed(Group::get(r)?, r.string()?),
    7 => Closed(g) { g.put(w); } => Self::Closed(Group::get(r)?),
    8 => Retired(g) { g.put(w); } => Self::Retired(Group::get(r)?),
    9 => Placed(c, reply) {
        c.put(w);
        reply.put(w);
    } => Self::Placed(Child::get(r)?, HostReply::get(r)?),
    10 => Fenced => Self::Fenced,
    11 => Collected(n) { w.u64(*n); } => Self::Collected(r.u64()?),
    12 => Answered(c, answer) {
        c.put(w);
        w.bytes(answer);
    } => Self::Answered(Child::get(r)?, r.bytes()?),
    13 => Need(a, keys, last) {
        a.put(w);
        put_wanted(w, keys);
        w.bool(*last);
    } => Self::Need(Attempt::get(r)?, get_wanted(r)?, r.bool()?),
    14 => Admitted(a, root, summary, display) {
        a.put(w);
        w.bytes(root);
        w.u64(summary.root_limit);
        w.u64(summary.old_root_limit);
        summary.grants.put(w);
        w.bytes(summary.target.as_bytes());
        w.u64(summary.n_place);
        w.option(&summary.gate, |w, green| {
            w.bytes(&green.tree);
            w.bytes(&green.root);
            w.bytes(&green.toolchain);
        });
        w.bytes(display.as_bytes());
    } => Self::Admitted(
        Attempt::get(r)?,
        r.bytes()?,
        Summary {
            root_limit: r.u64()?,
            old_root_limit: r.u64()?,
            grants: Grants::get(r)?,
            target: r.string()?,
            n_place: r.u64()?,
            gate: r.option(|r| {
                Ok(Green {
                    tree: r.bytes()?,
                    root: r.bytes()?,
                    toolchain: r.bytes()?,
                })
            })?,
        },
        r.string()?,
    ),
    15 => Assembled(a, key, digest, fetch, assembly) {
        a.put(w);
        w.bytes(key);
        w.bytes(digest);
        w.u64(*fetch);
        w.u64(*assembly);
    } => Self::Assembled(Attempt::get(r)?, r.bytes()?, r.bytes()?, r.u64()?, r.u64()?),
    16 => AdmissionFailed(a, failure) {
        a.put(w);
        let (tag, why) = match failure {
            Failure::Denied(why) => (0, why),
            Failure::Invalid(why) => (1, why),
            Failure::Unassembled(why) => (2, why),
            Failure::Uncompiled(why) => (3, why),
            Failure::Exhausted(why) => (4, why),
            Failure::Red(rows) => {
                w.u32(5);
                w.strings(rows);
                return;
            }
            Failure::GateLost(why) => (6, why),
        };
        w.u32(tag);
        w.bytes(why.as_bytes());
    } => Self::AdmissionFailed(
        Attempt::get(r)?,
        match r.u32()? {
            0 => Failure::Denied(r.string()?),
            1 => Failure::Invalid(r.string()?),
            2 => Failure::Unassembled(r.string()?),
            3 => Failure::Uncompiled(r.string()?),
            4 => Failure::Exhausted(r.string()?),
            5 => Failure::Red(r.strings()?),
            6 => Failure::GateLost(r.string()?),
            _ => return Err(malformed("admission failure tag")),
        },
    ),
    17 => CutoverResult(c, a, reply) {
        c.put(w);
        a.put(w);
        reply.put(w);
    } => Self::CutoverResult(Child::get(r)?, Attempt::get(r)?, HostReply::get(r)?),
);

#[path = "session_mode.rs"]
mod mode;
pub(crate) use mode::{InstallKind, Mode};
#[cfg(test)]
use mode::{MoveEnvelopeTooLarge, Serve};

#[path = "frame.rs"]
mod frame;
pub(crate) use frame::{MAX_FRAME, write_frame};

#[cfg(test)]
#[path = "session_laws.rs"]
pub(super) mod tests;
