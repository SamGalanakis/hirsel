//! The operator wire on `STATE/operator.sock` (launch.md "The operator socket"), as the CLI
//! speaks it. These types mirror `core/programs/platform/operator.hirsel` and encode in the
//! same value codec as the launcher session; operator_wire.hirsel holds the byte vectors
//! both sides check. One connection carries one exchange.
use crate::code_admission::ObjectKey;
use crate::launcher::session::{
    Attempt, Grants, Group, Reader, Wire, Writer, get_object_key, get_roots, malformed,
    put_object_key, put_roots,
};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::Path;

mod begin;
pub(crate) use begin::{BeginInput, Verdict};

/// The largest frame either side accepts.
pub(crate) const MAX_FRAME: usize = 16 << 20;
/// The largest logical reply the CLI reassembles from `Part`s.
const MAX_REPLY: u64 = 64 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) dir: String,
    pub(crate) retention_s: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Forward {
    Http,
    Stream,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Front {
    pub(crate) names: Vec<String>,
    pub(crate) forward: Forward,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Registration {
    pub(crate) name: String,
    pub(crate) tenant: String,
    pub(crate) workdir: String,
    pub(crate) trusted: bool,
    pub(crate) listen: String,
    pub(crate) store: String,
    pub(crate) memory: Option<u64>,
    pub(crate) args: Vec<String>,
    pub(crate) record: Option<Record>,
    pub(crate) front: Option<Front>,
    /// The package roots the group's source may link (launch.md "Package roots per group").
    pub(crate) packages: Vec<[u8; 32]>,
}

/// What a Register deploys, and a State reports (`operator.Input`): an attested recipe R, a publication
/// R in the launcher's code store, or a source's `launcher.Manifest` codec bytes, whose
/// blobs the host asks for by digest (launch.md "Source by content").
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Input {
    Attested(Vec<u8>),
    Publication(Vec<u8>),
    Source(Vec<u8>),
}

impl Wire for Input {
    fn put(&self, w: &mut Writer) {
        let (tag, bytes) = match self {
            Self::Attested(r) => (0, r),
            Self::Publication(r) => (1, r),
            Self::Source(tree) => (2, tree),
        };
        w.u32(tag);
        w.bytes(bytes);
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Attested(r.bytes()?),
            1 => Self::Publication(r.bytes()?),
            2 => Self::Source(r.bytes()?),
            _ => return Err(malformed("operator input tag")),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    Begin(Vec<u8>, Group, BeginInput, Vec<String>, Vec<String>, Fates),
    Data(Vec<u8>, Attempt, Vec<u8>),
    End(Vec<u8>, Attempt),
    Confirm(Vec<u8>, Attempt, Vec<u8>),
    Status(Vec<u8>),
    Resolve(Vec<u8>),
    Lookup(String),
    Register(Vec<u8>, Box<Registration>, Input, Vec<String>, Vec<String>),
    /// The operator's verdict on a request that awaits exactly this decision.
    Decide(Vec<u8>, Box<Decision>, Verdict),
    /// Back up the named group's store through its running serving place.
    Backup(String),
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
    pub(crate) triggers: TriggerReport,
    /// The migration OLD reported for the PREPARED it bound.
    pub(crate) migration: Option<Migration>,
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

impl Wire for Preview {
    fn put(&self, w: &mut Writer) {
        self.grants.put(w);
        w.u32(self.entries.len() as u32);
        for e in &self.entries {
            w.bytes(&e.identity);
            w.u32(ENTRY_CHANGES.iter().position(|c| *c == e.change).unwrap() as u32);
        }
        self.triggers.put(w);
        w.option(&self.migration, Migration::put);
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
        Ok(Self {
            grants,
            entries,
            triggers: TriggerReport::get(r)?,
            migration: r.option(Migration::get)?,
        })
    }
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

/// Which facts a decision has computed (launch.md "Self-editing"): the admission facts
/// only, or every fact after prepare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecisionStage {
    Admission,
    Prepared,
}

/// R's Foreign and Exec lines and the exact display text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Approvals {
    pub(crate) r: Vec<u8>,
    pub(crate) foreign: Vec<String>,
    pub(crate) exec: Vec<String>,
    pub(crate) text: String,
}

/// The migration PREPARED ships: its digest and the collections it touches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Migration {
    pub(crate) digest: Vec<u8>,
    pub(crate) changes: Vec<String>,
}

impl Migration {
    fn put(w: &mut Writer, m: &Self) {
        w.bytes(&m.digest);
        w.strings(&m.changes);
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(Self {
            digest: r.bytes()?,
            changes: r.strings()?,
        })
    }
}

/// What a held request awaits the operator's decision on (`operator.Decision`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Decision {
    pub(crate) tree: Option<Vec<u8>>,
    pub(crate) root: Option<Vec<u8>>,
    pub(crate) stage: DecisionStage,
    pub(crate) grants: Grants,
    pub(crate) approvals: Option<Approvals>,
    pub(crate) migration: Option<Migration>,
    pub(crate) triggers: TriggerReport,
}

impl Decision {
    /// Whether the decision shows a held group or an unmatched path, which its Decide fates.
    pub(crate) fn shows_held(&self) -> bool {
        !self.triggers.unmatched.is_empty() || self.triggers.rows.iter().any(|t| t.class == 1)
    }
}

impl Wire for Decision {
    fn put(&self, w: &mut Writer) {
        w.option(&self.tree, |w, t| w.bytes(t));
        w.option(&self.root, |w, h| w.bytes(h));
        w.u32(match self.stage {
            DecisionStage::Admission => 0,
            DecisionStage::Prepared => 1,
        });
        self.grants.put(w);
        w.option(&self.approvals, |w, a| {
            w.bytes(&a.r);
            w.strings(&a.foreign);
            w.strings(&a.exec);
            w.bytes(a.text.as_bytes());
        });
        w.option(&self.migration, Migration::put);
        self.triggers.put(w);
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(Self {
            tree: r.option(|r| r.bytes())?,
            root: r.option(|r| r.bytes())?,
            stage: match r.u32()? {
                0 => DecisionStage::Admission,
                1 => DecisionStage::Prepared,
                _ => return Err(malformed("decision stage tag")),
            },
            grants: Grants::get(r)?,
            approvals: r.option(|r| {
                Ok(Approvals {
                    r: r.bytes()?,
                    foreign: r.strings()?,
                    exec: r.strings()?,
                    text: r.string()?,
                })
            })?,
            migration: r.option(Migration::get)?,
            triggers: TriggerReport::get(r)?,
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
    /// A gated source whose tests were not all green.
    Red,
}

const CLASSES: [Class; 14] = [
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
    Class::Red,
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
    /// Waits, holding no attempt, for the operator's decision on exactly this decision.
    AwaitDecision(Box<Decision>),
}

/// One key a `Need` asks for (`launcher.Wanted`): an object the launcher's verifier lacks,
/// or a source blob, by SHA-256, the group's blob set lacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Needed {
    Object(ObjectKey),
    Blob([u8; 32]),
}

const BLOB_TAG: u32 = 5;

fn put_needed(w: &mut Writer, keys: &[Needed]) {
    w.len(keys.len());
    for key in keys {
        match key {
            Needed::Object(key) => put_object_key(w, key),
            Needed::Blob(digest) => {
                w.u32(BLOB_TAG);
                w.bytes(digest);
            }
        }
    }
}

fn get_needed(r: &mut Reader<'_>) -> io::Result<Vec<Needed>> {
    (0..r.count(8)?)
        .map(|_| match r.u32()? {
            BLOB_TAG => Ok(Needed::Blob(
                <[u8; 32]>::try_from(r.bytes()?).map_err(|_| malformed("needed blob digest"))?,
            )),
            tag => Ok(Needed::Object(get_object_key(tag, r)?)),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    State(Vec<u8>, Group, Input, Phase),
    Need(Vec<u8>, Attempt, Vec<Needed>, bool),
    Display(Vec<u8>, Attempt, Vec<u8>, String),
    Pin(Resolved),
    Error(Class, String),
    Group(Group),
    Part(u64, u64, Vec<u8>),
    Backup(Group, u64, u64, u64, String),
    BackupOffline(String),
}

impl Wire for Registration {
    fn put(&self, w: &mut Writer) {
        w.bytes(self.name.as_bytes());
        w.bytes(self.tenant.as_bytes());
        w.bytes(self.workdir.as_bytes());
        w.bool(self.trusted);
        w.bytes(self.listen.as_bytes());
        w.bytes(self.store.as_bytes());
        w.option(&self.memory, |w, &memory| w.u64(memory));
        w.strings(&self.args);
        w.option(&self.record, |w, record| {
            w.bytes(record.dir.as_bytes());
            w.u64(record.retention_s);
        });
        w.option(&self.front, |w, front| {
            w.strings(&front.names);
            w.u32(match front.forward {
                Forward::Http => 0,
                Forward::Stream => 1,
            });
        });
        put_roots(w, &self.packages);
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(Self {
            name: r.string()?,
            tenant: r.string()?,
            workdir: r.string()?,
            trusted: r.bool()?,
            listen: r.string()?,
            store: r.string()?,
            memory: r.option(Reader::u64)?,
            args: r.strings()?,
            record: r.option(|r| {
                Ok(Record {
                    dir: r.string()?,
                    retention_s: r.u64()?,
                })
            })?,
            front: r.option(|r| {
                let names = r.strings()?;
                let forward = match r.u32()? {
                    0 => Forward::Http,
                    1 => Forward::Stream,
                    _ => return Err(malformed("front forwarding mode")),
                };
                Ok(Front { names, forward })
            })?,
            packages: get_roots(r)?,
        })
    }
}

impl Wire for Ask {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::Begin(d, g, input, foreign, exec, held) => {
                w.u32(0);
                w.bytes(d);
                g.put(w);
                input.put(w);
                w.strings(foreign);
                w.strings(exec);
                held.put(w);
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
            Self::Decide(d, decision, verdict) => {
                w.u32(8);
                w.bytes(d);
                decision.put(w);
                verdict.put(w);
            }
            Self::Backup(name) => (w.u32(9), w.bytes(name.as_bytes())).1,
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Begin(
                r.bytes()?,
                Group::get(r)?,
                BeginInput::get(r)?,
                r.strings()?,
                r.strings()?,
                Fates::get(r)?,
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
            8 => Self::Decide(r.bytes()?, Box::new(Decision::get(r)?), Verdict::get(r)?),
            9 => Self::Backup(r.string()?),
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
            Self::AwaitDecision(decision) => (w.u32(9), decision.put(w)).1,
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
            9 => Self::AwaitDecision(Box::new(Decision::get(r)?)),
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
                put_needed(w, keys);
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
            Self::Backup(g, hi, lo, head, name) => {
                w.u32(7);
                g.put(w);
                w.u64(*hi);
                w.u64(*lo);
                w.u64(*head);
                w.bytes(name.as_bytes());
            }
            Self::BackupOffline(store) => (w.u32(8), w.bytes(store.as_bytes())).1,
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::State(r.bytes()?, Group::get(r)?, Input::get(r)?, Phase::get(r)?),
            1 => Self::Need(r.bytes()?, Attempt::get(r)?, get_needed(r)?, r.bool()?),
            2 => Self::Display(r.bytes()?, Attempt::get(r)?, r.bytes()?, r.string()?),
            3 => Self::Pin(match r.u32()? {
                0 => Resolved::Found(r.bytes()?),
                1 => Resolved::Gone,
                _ => return Err(malformed("operator pin tag")),
            }),
            4 => Self::Error(Class::get(r)?, r.string()?),
            5 => Self::Group(Group::get(r)?),
            6 => Self::Part(r.u64()?, r.u64()?, r.bytes()?),
            7 => Self::Backup(Group::get(r)?, r.u64()?, r.u64()?, r.u64()?, r.string()?),
            8 => Self::BackupOffline(r.string()?),
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
    Ok(Connection { stream })
}

impl Connection {
    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
        })
    }

    pub(crate) fn set_read_timeout(&self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
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
mod tests;
