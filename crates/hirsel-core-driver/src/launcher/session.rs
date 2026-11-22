//! The control session between the platform host and the launcher.
//!
//! These types mirror `core/programs/store/launcher.hirsel` and encode in its value codec
//! (core/std/api/codec.md): integers little-endian at their width, bool one byte,
//! String/Bytes/List a u32 count then the contents, Option and sums a u32 tag then the
//! payload, and records their fields in order. Declared variant order is the tag. Each
//! frame on the socket is a u32 little-endian length followed by one value.
use std::io::{self, Write};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallKind {
    Genesis,
    Standalone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Serve {
    pub(crate) activation: u64,
    pub(crate) idle: u64,
    pub(crate) candidate: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Install(u64, InstallKind),
    Serving(u64, Serve),
    Describe(Attempt),
    Preview(Attempt, Vec<u8>),
    Prepare(Attempt, String),
}

impl Mode {
    pub(crate) fn role(&self) -> Role {
        match *self {
            Self::Install(g, _) => Role::Install(g),
            Self::Serving(g, _) => Role::Serving(g),
            Self::Describe(a) => Role::Describe(a),
            Self::Preview(a, _) => Role::Preview(a),
            Self::Prepare(a, _) => Role::Prepare(a),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Launch {
    pub(crate) group: Group,
    pub(crate) assembly: Vec<u8>,
    pub(crate) workdir: String,
    pub(crate) args: Vec<String>,
    pub(crate) mode: Mode,
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
    Import(Vec<u8>, Vec<u8>, Vec<u8>),
    Cutover(Vec<u8>, Vec<u8>, u64, bool),
    Abort(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlaceReady {
    pub(crate) version: u64,
    pub(crate) store_hi: u64,
    pub(crate) store_lo: u64,
    pub(crate) release: Vec<u8>,
    pub(crate) retain_from: u64,
    pub(crate) ready_ns: u64,
}

/// runner's `HostReply`, a serving child's answer or notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostReply {
    Opened(PlaceReady),
    Current(u64, Vec<u8>),
    Refused(String),
    Exported(u64, String),
    Imported(Vec<u8>),
    Installed(u64, bool),
    Drift,
    Aborted,
    Uncertain,
    WriteAck(u64),
}

/// A group's containment label (launch.md "Containment (R27)").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Label {
    Contained,
    Uncontained,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Hello,
    Bind(Group, String, Label),
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
pub(crate) struct ChildState {
    pub(crate) child: Child,
    pub(crate) phase: Phase,
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
    Inventory(Vec<ChildState>, Vec<Group>),
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
}

fn malformed(what: &'static str) -> io::Error {
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
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> io::Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn bool(&mut self) -> io::Result<bool> {
        match self.take(1)?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(malformed("session bool")),
        }
    }
    fn bytes(&mut self) -> io::Result<Vec<u8>> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    fn string(&mut self) -> io::Result<String> {
        String::from_utf8(self.bytes()?).map_err(|_| malformed("session string"))
    }
    fn count(&mut self, least: usize) -> io::Result<usize> {
        // A count can claim no more elements than the bytes left can hold.
        let n = self.u32()? as usize;
        if n.saturating_mul(least) > self.bytes.len() - self.at {
            return Err(malformed("session count"));
        }
        Ok(n)
    }
    fn option<T>(&mut self, f: impl FnOnce(&mut Self) -> io::Result<T>) -> io::Result<Option<T>> {
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
    fn u32(&mut self, x: u32) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn i32(&mut self, x: i32) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn u64(&mut self, x: u64) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn bool(&mut self, x: bool) {
        self.0.push(u8::from(x));
    }
    fn len(&mut self, n: usize) {
        self.u32(u32::try_from(n).expect("session count fits u32"));
    }
    fn bytes(&mut self, x: &[u8]) {
        self.len(x.len());
        self.0.extend_from_slice(x);
    }
    fn option<T>(&mut self, x: &Option<T>, f: impl FnOnce(&mut Self, &T)) {
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

impl Wire for Mode {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::Install(g, kind) => {
                w.u32(0);
                w.u64(*g);
                w.u32(match kind {
                    InstallKind::Genesis => 0,
                    InstallKind::Standalone => 1,
                });
            }
            Self::Serving(g, serve) => {
                w.u32(1);
                w.u64(*g);
                w.u64(serve.activation);
                w.u64(serve.idle);
                w.option(&serve.candidate, |w, &c| w.u64(c));
            }
            Self::Describe(a) => (w.u32(2), a.put(w)).1,
            Self::Preview(a, old) => {
                w.u32(3);
                a.put(w);
                w.bytes(old);
            }
            Self::Prepare(a, export) => {
                w.u32(4);
                a.put(w);
                w.bytes(export.as_bytes());
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Install(
                r.u64()?,
                match r.u32()? {
                    0 => InstallKind::Genesis,
                    1 => InstallKind::Standalone,
                    _ => return Err(malformed("install kind tag")),
                },
            ),
            1 => Self::Serving(
                r.u64()?,
                Serve {
                    activation: r.u64()?,
                    idle: r.u64()?,
                    candidate: r.option(Reader::u64)?,
                },
            ),
            2 => Self::Describe(Attempt::get(r)?),
            3 => Self::Preview(Attempt::get(r)?, r.bytes()?),
            4 => Self::Prepare(Attempt::get(r)?, r.string()?),
            _ => return Err(malformed("mode tag")),
        })
    }
}

impl Wire for Launch {
    fn put(&self, w: &mut Writer) {
        self.group.put(w);
        w.bytes(&self.assembly);
        w.bytes(self.workdir.as_bytes());
        w.len(self.args.len());
        for arg in &self.args {
            w.bytes(arg.as_bytes());
        }
        self.mode.put(w);
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        let group = Group::get(r)?;
        let assembly = r.bytes()?;
        let workdir = r.string()?;
        let n = r.count(4)?;
        let args = (0..n).map(|_| r.string()).collect::<io::Result<_>>()?;
        Ok(Self {
            group,
            assembly,
            workdir,
            args,
            mode: Mode::get(r)?,
        })
    }
}

impl Wire for HostCommand {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::Export(a, retry) => {
                w.u32(0);
                w.bytes(a);
                w.bool(*retry);
            }
            Self::Import(d, release, digest) => {
                w.u32(1);
                w.bytes(d);
                w.bytes(release);
                w.bytes(digest);
            }
            Self::Cutover(a, digest, pause, retry) => {
                w.u32(2);
                w.bytes(a);
                w.bytes(digest);
                w.u64(*pause);
                w.bool(*retry);
            }
            Self::Abort(a) => (w.u32(3), w.bytes(a)).1,
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Export(r.bytes()?, r.bool()?),
            1 => Self::Import(r.bytes()?, r.bytes()?, r.bytes()?),
            2 => Self::Cutover(r.bytes()?, r.bytes()?, r.u64()?, r.bool()?),
            3 => Self::Abort(r.bytes()?),
            _ => return Err(malformed("host command tag")),
        })
    }
}

impl Wire for HostReply {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::Opened(ready) => {
                w.u32(0);
                w.u64(ready.version);
                w.u64(ready.store_hi);
                w.u64(ready.store_lo);
                w.bytes(&ready.release);
                w.u64(ready.retain_from);
                w.u64(ready.ready_ns);
            }
            Self::Current(version, release) => {
                w.u32(1);
                w.u64(*version);
                w.bytes(release);
            }
            Self::Refused(why) => (w.u32(2), w.bytes(why.as_bytes())).1,
            Self::Exported(version, name) => {
                w.u32(3);
                w.u64(*version);
                w.bytes(name.as_bytes());
            }
            Self::Imported(digest) => (w.u32(4), w.bytes(digest)).1,
            Self::Installed(version, waiting) => {
                w.u32(5);
                w.u64(*version);
                w.bool(*waiting);
            }
            Self::Drift => w.u32(6),
            Self::Aborted => w.u32(7),
            Self::Uncertain => w.u32(8),
            Self::WriteAck(at) => (w.u32(9), w.u64(*at)).1,
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Opened(PlaceReady {
                version: r.u64()?,
                store_hi: r.u64()?,
                store_lo: r.u64()?,
                release: r.bytes()?,
                retain_from: r.u64()?,
                ready_ns: r.u64()?,
            }),
            1 => Self::Current(r.u64()?, r.bytes()?),
            2 => Self::Refused(r.string()?),
            3 => Self::Exported(r.u64()?, r.string()?),
            4 => Self::Imported(r.bytes()?),
            5 => Self::Installed(r.u64()?, r.bool()?),
            6 => Self::Drift,
            7 => Self::Aborted,
            8 => Self::Uncertain,
            9 => Self::WriteAck(r.u64()?),
            _ => return Err(malformed("host reply tag")),
        })
    }
}

impl Wire for Request {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::Hello => w.u32(0),
            Self::Bind(g, address, label) => {
                w.u32(1);
                g.put(w);
                w.bytes(address.as_bytes());
                w.u32(match label {
                    Label::Contained => 0,
                    Label::Uncontained => 1,
                });
            }
            Self::Close(g) => (w.u32(2), g.put(w)).1,
            Self::Start(launch) => (w.u32(3), launch.put(w)).1,
            Self::Stop(c) => (w.u32(4), c.put(w)).1,
            Self::Kill(c) => (w.u32(5), c.put(w)).1,
            Self::Acknowledge(c) => (w.u32(6), c.put(w)).1,
            Self::Command(c, command) => {
                w.u32(7);
                c.put(w);
                command.put(w);
            }
            Self::Retire(g) => (w.u32(8), g.put(w)).1,
            Self::Release(a) => (w.u32(9), a.put(w)).1,
            Self::CollectBegin => w.u32(10),
            Self::Collect(roots) => {
                w.u32(11);
                w.len(roots.len());
                for root in roots {
                    w.bytes(root);
                }
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Hello,
            1 => Self::Bind(
                Group::get(r)?,
                r.string()?,
                match r.u32()? {
                    0 => Label::Contained,
                    1 => Label::Uncontained,
                    _ => return Err(malformed("label tag")),
                },
            ),
            2 => Self::Close(Group::get(r)?),
            3 => Self::Start(Launch::get(r)?),
            4 => Self::Stop(Child::get(r)?),
            5 => Self::Kill(Child::get(r)?),
            6 => Self::Acknowledge(Child::get(r)?),
            7 => Self::Command(Child::get(r)?, HostCommand::get(r)?),
            8 => Self::Retire(Group::get(r)?),
            9 => Self::Release(Attempt::get(r)?),
            10 => Self::CollectBegin,
            11 => {
                let n = r.count(4)?;
                Self::Collect((0..n).map(|_| r.bytes()).collect::<io::Result<_>>()?)
            }
            _ => return Err(malformed("request tag")),
        })
    }
}

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

impl Wire for Event {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::Inventory(children, readable) => {
                w.u32(0);
                w.len(children.len());
                for state in children {
                    state.child.put(w);
                    state.phase.put(w);
                }
                w.len(readable.len());
                for g in readable {
                    g.put(w);
                }
            }
            Self::Launched(c, phase) => {
                w.u32(1);
                c.put(w);
                phase.put(w);
            }
            Self::Refused(c, why) => {
                w.u32(2);
                c.put(w);
                why.put(w);
            }
            Self::Ended(c, end) => {
                w.u32(3);
                c.put(w);
                end.put(w);
            }
            Self::Arrived(g) => (w.u32(4), g.put(w)).1,
            Self::Bound(g) => (w.u32(5), g.put(w)).1,
            Self::BindFailed(g, why) => {
                w.u32(6);
                g.put(w);
                w.bytes(why.as_bytes());
            }
            Self::Closed(g) => (w.u32(7), g.put(w)).1,
            Self::Retired(g) => (w.u32(8), g.put(w)).1,
            Self::Placed(c, reply) => {
                w.u32(9);
                c.put(w);
                reply.put(w);
            }
            Self::Fenced => w.u32(10),
            Self::Collected(n) => (w.u32(11), w.u64(*n)).1,
            Self::Answered(c, answer) => {
                w.u32(12);
                c.put(w);
                w.bytes(answer);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => {
                let n = r.count(4)?;
                let children = (0..n)
                    .map(|_| {
                        Ok(ChildState {
                            child: Child::get(r)?,
                            phase: Phase::get(r)?,
                        })
                    })
                    .collect::<io::Result<_>>()?;
                let n = r.count(4)?;
                let readable = (0..n).map(|_| Group::get(r)).collect::<io::Result<_>>()?;
                Self::Inventory(children, readable)
            }
            1 => Self::Launched(Child::get(r)?, Phase::get(r)?),
            2 => Self::Refused(Child::get(r)?, Refusal::get(r)?),
            3 => Self::Ended(Child::get(r)?, End::get(r)?),
            4 => Self::Arrived(Group::get(r)?),
            5 => Self::Bound(Group::get(r)?),
            6 => Self::BindFailed(Group::get(r)?, r.string()?),
            7 => Self::Closed(Group::get(r)?),
            8 => Self::Retired(Group::get(r)?),
            9 => Self::Placed(Child::get(r)?, HostReply::get(r)?),
            10 => Self::Fenced,
            11 => Self::Collected(r.u64()?),
            12 => Self::Answered(Child::get(r)?, r.bytes()?),
            _ => return Err(malformed("event tag")),
        })
    }
}

/// The largest session frame either side accepts.
pub(crate) const MAX_FRAME: usize = 16 << 20;

pub(crate) fn write_frame(out: &mut impl Write, value: &impl Wire) -> io::Result<()> {
    let body = value.encode();
    let size = u32::try_from(body.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&size.to_le_bytes());
    frame.extend_from_slice(&body);
    out.write_all(&frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
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
    fn serving() -> Child {
        Child {
            group: group(),
            role: Role::Serving(7),
        }
    }
    fn preparing() -> Child {
        Child {
            group: group(),
            role: Role::Prepare(attempt()),
        }
    }
    fn launch(mode: Mode) -> Request {
        Request::Start(Launch {
            group: group(),
            assembly: (0..32).collect(),
            workdir: "/w".into(),
            args: vec!["a".into()],
            mode,
        })
    }

    // The expected bytes are shared with core/programs/store/launcher_wire.hirsel. They
    // were derived from core/std/api/codec.md's format by an encoder written apart from
    // both implementations.
    #[test]
    fn session_vectors() {
        let requests = [
            (Request::Hello, "00000000".to_owned()),
            (
                Request::Bind(group(), "127.0.0.1:9".into(), Label::Uncontained),
                "01000000010000006703000000000000000b0000003132372e302e302e313a3901000000".into(),
            ),
            (
                Request::Close(group()),
                "0200000001000000670300000000000000".into(),
            ),
            (
                launch(Mode::Install(1, InstallKind::Genesis)),
                "030000000100000067030000000000000020000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f020000002f7701000000010000006100000000010000000000000000000000".into(),
            ),
            (
                launch(Mode::Serving(
                    7,
                    Serve {
                        activation: 5,
                        idle: 250,
                        candidate: Some(6),
                    },
                )),
                "030000000100000067030000000000000020000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f020000002f770100000001000000610100000007000000000000000500000000000000fa00000000000000010000000600000000000000".into(),
            ),
            (
                launch(Mode::Describe(attempt())),
                "030000000100000067030000000000000020000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f020000002f770100000001000000610200000008070605040302011817161514131211".into(),
            ),
            (
                launch(Mode::Preview(attempt(), vec![1])),
                "030000000100000067030000000000000020000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f020000002f7701000000010000006103000000080706050403020118171615141312110100000001".into(),
            ),
            (
                launch(Mode::Prepare(attempt(), "x".into())),
                "030000000100000067030000000000000020000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f020000002f7701000000010000006104000000080706050403020118171615141312110100000078".into(),
            ),
            (
                Request::Stop(serving()),
                "0400000001000000670300000000000000010000000700000000000000".into(),
            ),
            (
                Request::Kill(preparing()),
                "05000000010000006703000000000000000400000008070605040302011817161514131211"
                    .into(),
            ),
            (
                Request::Acknowledge(serving()),
                "0600000001000000670300000000000000010000000700000000000000".into(),
            ),
            (
                Request::Command(
                    serving(),
                    HostCommand::Cutover(vec![0xaa], vec![0xbb], 2000, false),
                ),
                "07000000010000006703000000000000000100000007000000000000000200000001000000aa01000000bbd00700000000000000".into(),
            ),
            (
                Request::Retire(group()),
                "0800000001000000670300000000000000".into(),
            ),
            (
                Request::Release(attempt()),
                "0900000008070605040302011817161514131211".into(),
            ),
            (Request::CollectBegin, "0a000000".into()),
            (
                Request::Collect(vec![vec![1, 2]]),
                "0b00000001000000020000000102".into(),
            ),
        ];
        let events = [
            (
                Event::Inventory(
                    vec![
                        ChildState {
                            child: serving(),
                            phase: Phase::Started,
                        },
                        ChildState {
                            child: preparing(),
                            phase: Phase::Exited(End {
                                status: Status::Signal(9),
                                report: None,
                                readable: false,
                            }),
                        },
                    ],
                    vec![group()],
                ),
                "0000000002000000010000006703000000000000000100000007000000000000000000000001000000670300000000000000040000000807060504030201181716151413121101000000010000000900000000000000000100000001000000670300000000000000",
            ),
            (
                Event::Launched(serving(), Phase::Started),
                "010000000100000067030000000000000001000000070000000000000000000000",
            ),
            (
                Event::Refused(serving(), Refusal::Held),
                "020000000100000067030000000000000001000000070000000000000000000000",
            ),
            (
                Event::Refused(preparing(), Refusal::Spawn("e".into())),
                "02000000010000006703000000000000000400000008070605040302011817161514131211050000000100000065",
            ),
            (
                Event::Ended(
                    serving(),
                    End {
                        status: Status::Code(0),
                        report: Some(Some(2000)),
                        readable: true,
                    },
                ),
                "030000000100000067030000000000000001000000070000000000000000000000000000000100000001000000d00700000000000001",
            ),
            (
                Event::Arrived(group()),
                "0400000001000000670300000000000000",
            ),
            (Event::Bound(group()), "0500000001000000670300000000000000"),
            (
                Event::BindFailed(group(), "in use".into()),
                "060000000100000067030000000000000006000000696e20757365",
            ),
            (Event::Closed(group()), "0700000001000000670300000000000000"),
            (
                Event::Retired(group()),
                "0800000001000000670300000000000000",
            ),
            (
                Event::Placed(serving(), HostReply::Installed(4, true)),
                "090000000100000067030000000000000001000000070000000000000005000000040000000000000001",
            ),
            (
                Event::Placed(
                    serving(),
                    HostReply::Opened(PlaceReady {
                        version: 4,
                        store_hi: 1,
                        store_lo: 2,
                        release: vec![5],
                        retain_from: 1,
                        ready_ns: 99,
                    }),
                ),
                "090000000100000067030000000000000001000000070000000000000000000000040000000000000001000000000000000200000000000000010000000501000000000000006300000000000000",
            ),
            (
                Event::Answered(preparing(), vec![1, 2]),
                "0c000000010000006703000000000000000400000008070605040302011817161514131211020000000102",
            ),
            (Event::Fenced, "0a000000"),
            (Event::Collected(3), "0b0000000300000000000000"),
        ];
        for (value, expected) in &requests {
            assert_eq!(value.encode(), hex(expected), "{value:?}");
            assert_eq!(&Request::decode(&hex(expected)).unwrap(), value);
        }
        for (value, expected) in &events {
            assert_eq!(value.encode(), hex(expected), "{value:?}");
            assert_eq!(&Event::decode(&hex(expected)).unwrap(), value);
        }
    }
}
