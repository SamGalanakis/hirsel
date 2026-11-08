//! Typed reads: what a memo observed, keyed by what it read (design §1b).
//! A memo is valid exactly when every read still observes the same value.
use hirsel_intern::Symbol;
use hirsel_syntax::{ContentHash, DefId, DefPath, FileContext, PackageId};
use std::num::NonZeroU64;
mod rows;
use rows::Rows;

/// One revision of a session's inputs. Revision 1 is the first.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Revision(NonZeroU64);
impl Revision {
    pub const FIRST: Revision = Revision(NonZeroU64::MIN);
    pub fn get(self) -> u64 {
        self.0.get()
    }
    pub fn next(self) -> Revision {
        Revision(self.0.checked_add(1).expect("revision overflow"))
    }
    pub fn from_u64(n: u64) -> Option<Revision> {
        NonZeroU64::new(n).map(Revision)
    }
}

/// A package, by its `name@version` spelling.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageKey(pub Symbol);
impl PackageKey {
    pub fn of(package: &PackageId) -> Self {
        Self(Symbol::intern(&format!(
            "{}@{}",
            package.name, package.version
        )))
    }
    pub fn id(self) -> PackageId {
        let spelling = self.0.as_str();
        let (name, version) = spelling.rsplit_once('@').unwrap_or((spelling, ""));
        PackageId {
            name: name.into(),
            version: version.into(),
        }
    }
}
/// A module inside a package, by its dot-joined spelling (empty for the root).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModuleKey(pub Symbol);
impl ModuleKey {
    pub fn of(modules: &[String]) -> Self {
        Self(Symbol::intern(&modules.join(".")))
    }
}
/// Which lookup a read observed: the signature provider's, the resolver's
/// manifest index, or whether a module head is exposed.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LookupSource {
    Provider,
    Manifest,
    Head,
    /// Accessible spelling repairs, observed only by missing-name diagnostics.
    Candidates,
}

/// One name lookup: the file context it ran in and the path it spelled.
/// Negative lookups are reads too.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LookupKey {
    pub source: LookupSource,
    pub package: PackageKey,
    pub module: ModuleKey,
    /// The package a path names explicitly, if any.
    pub qualifier: Option<PackageKey>,
    /// The path's segments, dot-joined.
    pub path: Symbol,
}
impl LookupKey {
    pub fn of(
        source: LookupSource,
        context: &FileContext,
        qualifier: Option<&PackageId>,
        segments: &[String],
    ) -> Self {
        Self::in_context(source, Self::context_keys(context), qualifier, segments)
    }
    /// The package and module keys of a lookup context.
    pub fn context_keys(context: &FileContext) -> (PackageKey, ModuleKey) {
        (
            PackageKey::of(&context.package),
            ModuleKey::of(&context.modules),
        )
    }
    /// `of` with the context's keys already formed.
    pub fn in_context(
        source: LookupSource,
        (package, module): (PackageKey, ModuleKey),
        qualifier: Option<&PackageId>,
        segments: &[String],
    ) -> Self {
        Self {
            source,
            package,
            module,
            qualifier: qualifier.map(PackageKey::of),
            path: Symbol::intern(&segments.join(".")),
        }
    }
    /// The context the lookup ran in.
    pub fn context(&self) -> FileContext {
        FileContext {
            package: self.package.id(),
            modules: split(self.module.0.as_str()),
        }
    }
    pub fn qualifier(&self) -> Option<PackageId> {
        self.qualifier.map(PackageKey::id)
    }
    pub fn segments(&self) -> Vec<String> {
        split(self.path.as_str())
    }
}
fn split(dotted: &str) -> Vec<String> {
    if dotted.is_empty() {
        vec![]
    } else {
        dotted.split('.').map(str::to_owned).collect()
    }
}

/// Every input a memo can observe. Each read records the observed value's
/// content hash (`Reads`).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReadKey {
    /// A signature's fingerprint.
    Signature(DefId),
    /// A failed signature's failed key.
    FailedSignature(DefId),
    /// A lookup's output, including a negative one.
    Lookup(LookupKey),
    /// The set of names a module declares.
    NameSet(PackageKey, ModuleKey),
    /// The private view's `ViewDigest`.
    Representation(DefId),
    /// An executed body's text hash.
    ExecutedBody(DefId),
    /// An executed definition's signature fingerprint.
    ExecutedSignature(DefId),
    /// A cited item's text hash.
    CitedText(DefId),
    /// H(count, then (file path, header hash) per candidate in source order);
    /// read by every query keyed on the definition.
    DeclarationCandidates(DefId),
    /// V's digest of a view.
    ValidationView(DefId),
    /// The aggregate digest of the signature universe's V errors.
    UniverseErrors,
    /// The selected selected heap role.
    HeapRole,
    /// The query environment's hash.
    Environment,
    /// All V input views in the originating package's private universe.
    /// The package is explicit because execution-read unions cross packages.
    ValidationUniverse(PackageKey),
    /// A representation body consumed by H, without interpreting its code.
    LayoutBody(DefId),
    /// Normalized declaration structure, before private inference evidence.
    DeclaredSignature(DefId),
    /// Settled private inference output, independent of its module inputs.
    PrivateSummary(DefId),
}

/// The kind of a read, for counters and durability rules.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReadKind {
    Signature,
    FailedSignature,
    Lookup,
    NameSet,
    Representation,
    ExecutedBody,
    ExecutedSignature,
    CitedText,
    DeclarationCandidates,
    ValidationView,
    UniverseErrors,
    HeapRole,
    Environment,
    ValidationUniverse,
    LayoutBody,
    DeclaredSignature,
    PrivateSummary,
}
impl ReadKind {
    pub const ALL: [ReadKind; 17] = [
        ReadKind::Signature,
        ReadKind::FailedSignature,
        ReadKind::Lookup,
        ReadKind::NameSet,
        ReadKind::Representation,
        ReadKind::ExecutedBody,
        ReadKind::ExecutedSignature,
        ReadKind::CitedText,
        ReadKind::DeclarationCandidates,
        ReadKind::ValidationView,
        ReadKind::UniverseErrors,
        ReadKind::HeapRole,
        ReadKind::Environment,
        ReadKind::ValidationUniverse,
        ReadKind::LayoutBody,
        ReadKind::DeclaredSignature,
        ReadKind::PrivateSummary,
    ];
    pub fn name(self) -> &'static str {
        match self {
            ReadKind::Signature => "signature",
            ReadKind::DeclaredSignature => "declared signature",
            ReadKind::PrivateSummary => "private summary",
            ReadKind::FailedSignature => "failed signature",
            ReadKind::Lookup => "lookup",
            ReadKind::NameSet => "name set",
            ReadKind::Representation => "representation",
            ReadKind::ExecutedBody => "executed body",
            ReadKind::LayoutBody => "layout body",
            ReadKind::ExecutedSignature => "executed signature",
            ReadKind::CitedText => "cited text",
            ReadKind::DeclarationCandidates => "declaration candidates",
            ReadKind::ValidationView => "validation view",
            ReadKind::UniverseErrors => "universe errors",
            ReadKind::HeapRole => "heap role",
            ReadKind::Environment => "environment",
            ReadKind::ValidationUniverse => "validation universe",
        }
    }
}

impl ReadKey {
    pub fn kind(&self) -> ReadKind {
        match self {
            ReadKey::Signature(_) => ReadKind::Signature,
            ReadKey::DeclaredSignature(_) => ReadKind::DeclaredSignature,
            ReadKey::PrivateSummary(_) => ReadKind::PrivateSummary,
            ReadKey::FailedSignature(_) => ReadKind::FailedSignature,
            ReadKey::Lookup(_) => ReadKind::Lookup,
            ReadKey::NameSet(..) => ReadKind::NameSet,
            ReadKey::Representation(_) => ReadKind::Representation,
            ReadKey::ExecutedBody(_) => ReadKind::ExecutedBody,
            ReadKey::LayoutBody(_) => ReadKind::LayoutBody,
            ReadKey::ExecutedSignature(_) => ReadKind::ExecutedSignature,
            ReadKey::CitedText(_) => ReadKind::CitedText,
            ReadKey::DeclarationCandidates(_) => ReadKind::DeclarationCandidates,
            ReadKey::ValidationView(_) => ReadKind::ValidationView,
            ReadKey::UniverseErrors => ReadKind::UniverseErrors,
            ReadKey::HeapRole => ReadKind::HeapRole,
            ReadKey::Environment => ReadKind::Environment,
            ReadKey::ValidationUniverse(_) => ReadKind::ValidationUniverse,
        }
    }
    /// The definition this read names, if any.
    pub fn definition(&self) -> Option<DefId> {
        match *self {
            ReadKey::Signature(d)
            | ReadKey::DeclaredSignature(d)
            | ReadKey::PrivateSummary(d)
            | ReadKey::FailedSignature(d)
            | ReadKey::Representation(d)
            | ReadKey::ExecutedBody(d)
            | ReadKey::LayoutBody(d)
            | ReadKey::ExecutedSignature(d)
            | ReadKey::CitedText(d)
            | ReadKey::DeclarationCandidates(d)
            | ReadKey::ValidationView(d) => Some(d),
            _ => None,
        }
    }
    /// The package whose inputs this read observes; `None` for the environment
    /// and V's universe-wide reads.
    pub fn package(&self) -> Option<PackageKey> {
        match self {
            ReadKey::Lookup(l) => Some(l.package),
            ReadKey::NameSet(p, _) => Some(*p),
            _ => self
                .definition()
                .map(|d| PackageKey::of(&d.path().0.package)),
        }
    }
    /// Exact persisted key length without allocating a byte buffer.
    pub fn encoded_bytes(&self) -> Option<usize> {
        let text = |s: &str| 4usize.checked_add(s.len());
        let payload = match *self {
            Self::Signature(d)
            | Self::DeclaredSignature(d)
            | Self::PrivateSummary(d)
            | Self::FailedSignature(d)
            | Self::Representation(d)
            | Self::ExecutedBody(d)
            | Self::LayoutBody(d)
            | Self::ExecutedSignature(d)
            | Self::CitedText(d)
            | Self::DeclarationCandidates(d)
            | Self::ValidationView(d) => encoded_def_path_bytes(d.path())?,
            Self::Lookup(l) => {
                let mut bytes = 2usize
                    .checked_add(text(l.package.0.as_str())?)?
                    .checked_add(text(l.module.0.as_str())?)?
                    .checked_add(text(l.path.as_str())?)?;
                if let Some(q) = l.qualifier {
                    bytes = bytes.checked_add(text(q.0.as_str())?)?;
                }
                bytes
            }
            Self::NameSet(p, m) => text(p.0.as_str())?.checked_add(text(m.0.as_str())?)?,
            Self::ValidationUniverse(p) => text(p.0.as_str())?,
            Self::UniverseErrors | Self::HeapRole | Self::Environment => 0,
        };
        payload.checked_add(1)
    }
    /// Persisted form: kind tag, then DefPaths and spellings, never session ids.
    pub fn encode(&self, out: &mut Vec<u8>) {
        fn text(out: &mut Vec<u8>, s: &str) {
            out.extend((s.len() as u32).to_le_bytes());
            out.extend(s.as_bytes());
        }
        fn path(out: &mut Vec<u8>, d: DefId) {
            encode_def_path(out, d.path());
        }
        out.push(self.kind() as u8);
        match *self {
            ReadKey::Signature(d)
            | ReadKey::DeclaredSignature(d)
            | ReadKey::PrivateSummary(d)
            | ReadKey::FailedSignature(d)
            | ReadKey::Representation(d)
            | ReadKey::ExecutedBody(d)
            | ReadKey::LayoutBody(d)
            | ReadKey::ExecutedSignature(d)
            | ReadKey::CitedText(d)
            | ReadKey::DeclarationCandidates(d)
            | ReadKey::ValidationView(d) => path(out, d),
            ReadKey::Lookup(l) => {
                out.push(l.source as u8);
                text(out, l.package.0.as_str());
                text(out, l.module.0.as_str());
                match l.qualifier {
                    Some(q) => {
                        out.push(1);
                        text(out, q.0.as_str());
                    }
                    None => out.push(0),
                }
                text(out, l.path.as_str());
            }
            ReadKey::NameSet(p, m) => {
                text(out, p.0.as_str());
                text(out, m.0.as_str());
            }
            ReadKey::ValidationUniverse(package) => text(out, package.0.as_str()),
            ReadKey::UniverseErrors | ReadKey::HeapRole | ReadKey::Environment => {}
        }
    }
}

/// A memo's reads: `(ReadKey, observed value)` sorted by key, one entry per key.
#[derive(Clone, Debug, Default)]
pub struct Reads {
    rows: Rows,
    // A sorted view into the same canonical rows. Normal receipts stay flat;
    // completed predecessor phases can share the final phase's allocation.
    indices: Option<std::sync::Arc<[u32]>>,
}
impl PartialEq for Reads {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl Eq for Reads {}
impl std::hash::Hash for Reads {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.len().hash(state);
        for row in self.iter() {
            row.hash(state);
        }
    }
}

/// Two observations of one key disagreed inside one evaluation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Inconsistent(pub ReadKey);

#[cfg(test)]
thread_local! { static SORT_COMPARISONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

impl Reads {
    pub fn empty() -> Self {
        Self::default()
    }
    /// Sorts and de-duplicates; conflicting observations of one key are an
    /// error naming the least such key.
    pub fn from_vec(reads: Vec<(ReadKey, ContentHash)>) -> Result<Self, Inconsistent> {
        let mut unique: Vec<(ReadKey, ContentHash)>;
        let mut conflict: Option<ReadKey> = None;
        if reads.len() >= 32 {
            use std::collections::{HashMap, hash_map::Entry};
            let mut by_key =
                HashMap::with_capacity_and_hasher(reads.len(), hirsel_intern::IdHasher::default());
            for (key, value) in reads {
                match by_key.entry(key) {
                    Entry::Vacant(entry) => {
                        entry.insert(value);
                    }
                    Entry::Occupied(entry) => {
                        if *entry.get() != value && conflict.is_none_or(|old| key < old) {
                            conflict = Some(key);
                        }
                    }
                }
            }
            unique = by_key.into_iter().collect();
        } else {
            unique = Vec::with_capacity(reads.len());
            for (key, value) in reads {
                if let Some((_, previous)) = unique.iter().find(|(candidate, _)| *candidate == key)
                {
                    if *previous != value && conflict.is_none_or(|old| key < old) {
                        conflict = Some(key);
                    }
                } else {
                    unique.push((key, value));
                }
            }
        }
        if let Some(key) = conflict {
            return Err(Inconsistent(key));
        }
        unique.sort_unstable_by(|a, b| {
            #[cfg(test)]
            SORT_COMPARISONS.with(|n| n.set(n.get() + 1));
            a.0.cmp(&b.0)
        });
        Ok(Self::from_canonical(unique))
    }
    /// Freeze rows already sorted by key with one row per key.
    pub fn from_canonical(rows: Vec<(ReadKey, ContentHash)>) -> Self {
        debug_assert!(rows.windows(2).all(|pair| pair[0].0 < pair[1].0));
        Self {
            rows: rows.into(),
            indices: None,
        }
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &(ReadKey, ContentHash)> {
        (0..self.len())
            .map(|at| &self.rows[self.indices.as_ref().map_or(at, |rows| rows[at] as usize)])
    }
    pub fn len(&self) -> usize {
        self.indices
            .as_ref()
            .map_or(self.rows.len(), |rows| rows.len())
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn get(&self, key: &ReadKey) -> Option<ContentHash> {
        let at = self.rows.binary_search_by(|(k, _)| k.cmp(key)).ok()?;
        if let Some(indices) = &self.indices {
            let index = u32::try_from(at).ok()?;
            indices.binary_search(&index).ok()?;
        }
        Some(self.rows[at].1)
    }
    /// A lossless view of these exact observations in a complete superset.
    /// Standard Arc slice ownership and sorted integer indices keep the flat
    /// binary-search/order/codec contract; no lookup can see an excluded row.
    pub fn share_rows(&self, complete: &Self) -> Option<Self> {
        if complete.indices.is_some() || self.len() > complete.len() {
            return None;
        }
        let mut indices = Vec::with_capacity(self.len());
        for (key, value) in self.iter() {
            let at = complete
                .rows
                .binary_search_by(|(candidate, _)| candidate.cmp(key))
                .ok()?;
            if complete.rows[at].1 != *value {
                return None;
            }
            indices.push(u32::try_from(at).ok()?);
        }
        Some(Self {
            rows: complete.rows.clone(),
            indices: (indices.len() != complete.len()).then(|| indices.into()),
        })
    }
    /// Detach a selected phase from the complete body's backing rows. Exact
    /// observations and encoding stay unchanged.
    pub fn flat_copy(&self) -> Self {
        if self.indices.is_none() {
            return self.clone();
        }
        Self {
            rows: self.iter().copied().collect::<Vec<_>>().into(),
            indices: None,
        }
    }
    pub fn flat_backing_bytes(&self) -> Option<usize> {
        self.indices
            .as_ref()
            .map(|_| 2 * size_of::<usize>() + self.len() * size_of::<(ReadKey, ContentHash)>())
    }
    /// Actual distinct Arc backing allocations; the containing Reads value is
    /// accounted separately by its owner. This exposes no semantic identity.
    pub fn allocations(&self) -> impl Iterator<Item = (usize, usize)> {
        let rows = Some((self.rows.as_ptr() as usize, self.rows.allocation_bytes()));
        let indices = self.indices.as_ref().map(|indices| {
            (
                indices.as_ptr() as usize,
                hirsel_engine_admission::arc_bytes(&**indices),
            )
        });
        [rows, indices].into_iter().flatten()
    }
    /// The union of two read sets (execution closure, design §1b).
    pub fn union(&self, other: &Reads) -> Result<Reads, Inconsistent> {
        let mut conflict = None;
        let rows = merge_canonical(self.iter().copied(), other.iter().copied(), &mut conflict);
        match conflict {
            Some(key) => Err(Inconsistent(key)),
            None => Ok(Self::from_canonical(rows)),
        }
    }
    /// The first read, in key order, whose current value differs by `current`.
    pub fn first_changed(
        &self,
        mut current: impl FnMut(&ReadKey) -> Option<ContentHash>,
    ) -> Option<ReadKey> {
        self.iter()
            .find(|(k, v)| current(k) != Some(*v))
            .map(|(k, _)| *k)
    }
    /// The canonical persisted encoding.
    pub fn encode(&self, out: &mut Vec<u8>) {
        // HRD4 is canonical rows only; earlier framings carried an
        // observation permutation and are rejected.
        out.extend(b"HRD4");
        out.extend((self.len() as u32).to_le_bytes());
        for (key, value) in self.iter() {
            key.encode(out);
            out.extend(value.0);
        }
    }
    pub fn encoded_bytes(&self) -> Option<u64> {
        let bytes = self.iter().try_fold(8u64, |bytes, (key, value)| {
            bytes
                .checked_add(u64::try_from(key.encoded_bytes()?).ok()?)?
                .checked_add(value.0.len() as u64)
        })?;
        Some(bytes)
    }
    pub fn encoding_working_bytes(&self) -> Option<u64> {
        self.encoded_bytes()?
            .max(8)
            .checked_mul(4)?
            .checked_add(size_of::<Vec<u8>>() as u64)
    }
    /// Visit HRD4's canonical rows with the same framing and row-order
    /// checks as decode.
    pub fn visit_encoded(
        mut input: &[u8],
        mut visit: impl FnMut(usize, (ReadKey, ContentHash)) -> Option<()>,
    ) -> Option<usize> {
        if take(&mut input, 4)? != b"HRD4" {
            return None;
        }
        let count = take_u32(&mut input)? as usize;
        if count > input.len() / (1 + size_of::<ContentHash>()) {
            return None;
        }
        let mut previous = None;
        for index in 0..count {
            let key = decode_key(&mut input)?;
            if previous.is_some_and(|old| old >= key) {
                return None;
            }
            previous = Some(key);
            let value = ContentHash(take(&mut input, 32)?.try_into().ok()?);
            visit(index, (key, value))?;
        }
        input.is_empty().then_some(count)
    }
    pub fn decode(input: &mut &[u8]) -> Option<Reads> {
        if take(input, 4)? != b"HRD4" {
            return None;
        }
        let count = take_u32(input)? as usize;
        if count > input.len() / (1 + size_of::<ContentHash>()) {
            return None;
        }
        let mut reads = Vec::with_capacity(count.min(1 << 16));
        for _ in 0..count {
            let key = decode_key(input)?;
            if reads.last().is_some_and(|(previous, _)| previous >= &key) {
                return None;
            }
            let value = ContentHash(take(input, 32)?.try_into().ok()?);
            reads.push((key, value));
        }
        Some(Self::from_canonical(reads))
    }
    /// The content hash of the canonical encoding.
    pub fn digest(&self) -> ContentHash {
        let mut bytes = vec![];
        bytes.extend(b"HRD2");
        bytes.extend((self.len() as u32).to_le_bytes());
        for (key, value) in self.iter() {
            key.encode(&mut bytes);
            bytes.extend(value.0);
        }
        ContentHash::of_parts("hirsel.reads.v2", [bytes.as_slice()])
    }
}

/// Records reads during one evaluation (a task-local accumulator), then
/// freezes them into `Reads`. Merged complete read sets stay canonical and
/// join by linear merges; only the evaluation's own rows are sorted.
#[derive(Clone, Debug, Default)]
pub struct ReadsBuilder {
    rows: Vec<(ReadKey, ContentHash)>,
    sets: Vec<Reads>,
}
impl ReadsBuilder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn record(&mut self, key: ReadKey, value: ContentHash) {
        self.rows.push((key, value));
    }
    /// Merges another memo's complete reads (execution closure by union).
    pub fn merge(&mut self, reads: &Reads) {
        if !reads.is_empty() {
            self.sets.push(reads.clone());
        }
    }
    /// Merges another evaluation's recorded reads.
    pub fn extend(&mut self, other: ReadsBuilder) {
        self.rows.extend(other.rows);
        self.sets.extend(other.sets);
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.sets.is_empty()
    }
    /// The union of every recorded row and merged set. Conflicting
    /// observations of one key are an error naming the least such key.
    pub fn finish(self) -> Result<Reads, Inconsistent> {
        let mut conflict = None;
        let own = match Reads::from_vec(self.rows) {
            Ok(own) => own,
            Err(Inconsistent(key)) => {
                conflict = Some(key);
                Reads::empty()
            }
        };
        let mut level: Vec<Reads> = std::iter::once(own)
            .chain(self.sets)
            .filter(|reads| !reads.is_empty())
            .collect();
        if level.len() == 1 && conflict.is_none() {
            return Ok(level.pop().expect("one set"));
        }
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(2));
            let mut sets = level.into_iter();
            while let Some(a) = sets.next() {
                next.push(match sets.next() {
                    Some(b) => Reads::from_canonical(merge_canonical(
                        a.iter().copied(),
                        b.iter().copied(),
                        &mut conflict,
                    )),
                    None => a,
                });
            }
            level = next;
        }
        match conflict {
            Some(key) => Err(Inconsistent(key)),
            None => Ok(level.pop().unwrap_or_default()),
        }
    }
}

/// Merge two canonical row sequences, keeping the first value of a shared
/// key and lowering `conflict` to the least key whose values disagree.
fn merge_canonical(
    a: impl Iterator<Item = (ReadKey, ContentHash)>,
    b: impl Iterator<Item = (ReadKey, ContentHash)>,
    conflict: &mut Option<ReadKey>,
) -> Vec<(ReadKey, ContentHash)> {
    let (mut a, mut b) = (a.peekable(), b.peekable());
    let mut out = Vec::with_capacity(a.size_hint().0 + b.size_hint().0);
    loop {
        match (a.peek(), b.peek()) {
            (Some(x), Some(y)) => match x.0.cmp(&y.0) {
                std::cmp::Ordering::Less => out.push(a.next().expect("peeked")),
                std::cmp::Ordering::Greater => out.push(b.next().expect("peeked")),
                std::cmp::Ordering::Equal => {
                    let (x, y) = (a.next().expect("peeked"), b.next().expect("peeked"));
                    if x.1 != y.1 && conflict.is_none_or(|old| x.0 < old) {
                        *conflict = Some(x.0);
                    }
                    out.push(x);
                }
            },
            (Some(_), None) => out.extend(a.by_ref()),
            (None, Some(_)) => out.extend(b.by_ref()),
            (None, None) => break,
        }
    }
    out
}

impl serde::Serialize for Reads {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut bytes = vec![];
        self.encode(&mut bytes);
        s.serialize_bytes(&bytes)
    }
}
impl<'de> serde::Deserialize<'de> for Reads {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bytes = hirsel_product_codec::deserialize_payload(d)?;
        let mut input = bytes.as_slice();
        Reads::decode(&mut input)
            .filter(|_| input.is_empty())
            .ok_or_else(|| serde::de::Error::custom("invalid reads"))
    }
}

fn take<'a>(input: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    if input.len() < n {
        return None;
    }
    let (head, tail) = input.split_at(n);
    *input = tail;
    Some(head)
}
fn take_u32(input: &mut &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(take(input, 4)?.try_into().ok()?))
}
fn take_text<'a>(input: &mut &'a [u8]) -> Option<&'a str> {
    let n = take_u32(input)? as usize;
    std::str::from_utf8(take(input, n)?).ok()
}
fn take_def(input: &mut &[u8]) -> Option<DefId> {
    Some(DefId::intern(&decode_def_path(input)?))
}
fn put_text(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u32).to_le_bytes());
    out.extend(s.as_bytes());
}
/// A DefPath's persisted form: package name and version, module count, modules, name.
pub fn encode_def_path(out: &mut Vec<u8>, path: &DefPath) {
    let item = &path.0;
    put_text(out, &item.package.name);
    put_text(out, &item.package.version);
    out.extend((item.modules.len() as u32).to_le_bytes());
    for module in &item.modules {
        put_text(out, module);
    }
    put_text(out, &item.name);
}
/// Exact persisted path length; framing follows encode_def_path.
pub fn encoded_def_path_bytes(path: &DefPath) -> Option<usize> {
    let item = &path.0;
    std::iter::once(&item.package.name)
        .chain(std::iter::once(&item.package.version))
        .chain(&item.modules)
        .chain(std::iter::once(&item.name))
        .try_fold(4usize, |bytes, text| {
            bytes.checked_add(4)?.checked_add(text.len())
        })
}
pub fn decode_def_path(input: &mut &[u8]) -> Option<DefPath> {
    let name = take_text(input)?.to_owned();
    let version = take_text(input)?.to_owned();
    let count = take_u32(input)? as usize;
    let mut modules = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        modules.push(take_text(input)?.to_owned());
    }
    Some(DefPath(hirsel_syntax::ItemPath {
        package: PackageId { name, version },
        modules,
        name: take_text(input)?.to_owned(),
    }))
}
fn decode_key(input: &mut &[u8]) -> Option<ReadKey> {
    let tag = *take(input, 1)?.first()?;
    let kind = *ReadKind::ALL.get(tag as usize)?;
    Some(match kind {
        ReadKind::Signature => ReadKey::Signature(take_def(input)?),
        ReadKind::DeclaredSignature => ReadKey::DeclaredSignature(take_def(input)?),
        ReadKind::PrivateSummary => ReadKey::PrivateSummary(take_def(input)?),
        ReadKind::FailedSignature => ReadKey::FailedSignature(take_def(input)?),
        ReadKind::Representation => ReadKey::Representation(take_def(input)?),
        ReadKind::ExecutedBody => ReadKey::ExecutedBody(take_def(input)?),
        ReadKind::LayoutBody => ReadKey::LayoutBody(take_def(input)?),
        ReadKind::ExecutedSignature => ReadKey::ExecutedSignature(take_def(input)?),
        ReadKind::CitedText => ReadKey::CitedText(take_def(input)?),
        ReadKind::DeclarationCandidates => ReadKey::DeclarationCandidates(take_def(input)?),
        ReadKind::ValidationView => ReadKey::ValidationView(take_def(input)?),
        ReadKind::Lookup => {
            let source = match *take(input, 1)?.first()? {
                0 => LookupSource::Provider,
                1 => LookupSource::Manifest,
                2 => LookupSource::Head,
                3 => LookupSource::Candidates,
                _ => return None,
            };
            let package = PackageKey(Symbol::intern(take_text(input)?));
            let module = ModuleKey(Symbol::intern(take_text(input)?));
            let qualifier = match *take(input, 1)?.first()? {
                0 => None,
                1 => Some(PackageKey(Symbol::intern(take_text(input)?))),
                _ => return None,
            };
            let path = Symbol::intern(take_text(input)?);
            ReadKey::Lookup(LookupKey {
                source,
                package,
                module,
                qualifier,
                path,
            })
        }
        ReadKind::NameSet => ReadKey::NameSet(
            PackageKey(Symbol::intern(take_text(input)?)),
            ModuleKey(Symbol::intern(take_text(input)?)),
        ),
        ReadKind::UniverseErrors => ReadKey::UniverseErrors,
        ReadKind::HeapRole => ReadKey::HeapRole,
        ReadKind::Environment => ReadKey::Environment,
        ReadKind::ValidationUniverse => {
            ReadKey::ValidationUniverse(PackageKey(Symbol::intern(take_text(input)?)))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_syntax::ItemPath;
    fn def(name: &str) -> DefId {
        DefId::intern(&DefPath(ItemPath {
            package: PackageId {
                name: "p".into(),
                version: "1".into(),
            },
            modules: vec!["m".into()],
            name: name.into(),
        }))
    }
    fn h(s: &str) -> ContentHash {
        ContentHash::of_bytes(s.as_bytes())
    }

    #[test]
    fn reads_sort_by_spelling_union_and_reject_conflicts() {
        use std::hash::{Hash, Hasher};
        let (a, b) = (def("a"), def("b"));
        let one = Reads::from_vec(vec![
            (ReadKey::Signature(b), h("b")),
            (ReadKey::Signature(a), h("a")),
            (ReadKey::Signature(a), h("a")),
        ])
        .unwrap();
        assert_eq!(one.len(), 2);
        assert_eq!(one.iter().next().unwrap().0, ReadKey::Signature(a));
        let same_set = Reads::from_vec(vec![
            (ReadKey::Signature(a), h("a")),
            (ReadKey::Signature(b), h("b")),
        ])
        .unwrap();
        assert_eq!(one, same_set);
        let hash = |reads: &Reads| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            reads.hash(&mut hasher);
            hasher.finish()
        };
        assert_eq!(hash(&one), hash(&same_set));
        assert_eq!(one.digest(), same_set.digest());
        let mut canonical_encoding = b"HRD2".to_vec();
        canonical_encoding.extend((one.len() as u32).to_le_bytes());
        for (key, value) in one.iter() {
            key.encode(&mut canonical_encoding);
            canonical_encoding.extend(value.0);
        }
        assert_eq!(
            one.digest(),
            ContentHash::of_parts("hirsel.reads.v2", [canonical_encoding.as_slice()])
        );
        let mut visited = Vec::new();
        assert_eq!(
            one.first_changed(|key| {
                visited.push(*key);
                if *key == ReadKey::Signature(b) {
                    None
                } else {
                    one.get(key)
                }
            }),
            Some(ReadKey::Signature(b))
        );
        assert_eq!(visited, vec![ReadKey::Signature(a), ReadKey::Signature(b)]);
        let two = Reads::from_vec(vec![
            (ReadKey::Signature(a), h("a")),
            (ReadKey::ExecutedBody(a), h("body")),
        ])
        .unwrap();
        let both = one.union(&two).unwrap();
        assert_eq!(both.len(), 3);
        assert_eq!(both, two.union(&one).unwrap());
        let conflict = Reads::from_vec(vec![(ReadKey::Signature(a), h("other"))]).unwrap();
        assert_eq!(
            one.union(&conflict),
            Err(Inconsistent(ReadKey::Signature(a)))
        );
        assert!(
            Reads::from_vec(vec![
                (ReadKey::Signature(a), h("x")),
                (ReadKey::Signature(a), h("y"))
            ])
            .is_err()
        );
    }

    #[test]
    fn duplicate_read_observations_preserve_canonical_results_with_unique_sort_work() {
        fn original(rows: Vec<(ReadKey, ContentHash)>) -> Result<Reads, Inconsistent> {
            let mut unique = Vec::new();
            let mut conflict = None;
            for (key, value) in rows {
                if let Some((_, previous)) = unique.iter().find(|(candidate, _)| *candidate == key)
                {
                    if *previous != value && conflict.is_none_or(|old| key < old) {
                        conflict = Some(key);
                    }
                } else {
                    unique.push((key, value));
                }
            }
            if let Some(key) = conflict {
                return Err(Inconsistent(key));
            }
            unique.sort_unstable_by_key(|(key, _)| *key);
            Ok(Reads {
                rows: unique.into(),
                indices: None,
            })
        }
        let (a, b) = (def("read_z"), def("read_a"));
        let ctx = FileContext {
            package: PackageId {
                name: "p".into(),
                version: "1".into(),
            },
            modules: vec!["m".into()],
        };
        let package = PackageKey::of(&ctx.package);
        let keys = [
            ReadKey::Signature(a),
            ReadKey::Signature(b),
            ReadKey::FailedSignature(b),
            ReadKey::Lookup(LookupKey::of(
                LookupSource::Provider,
                &ctx,
                None,
                &["x".into()],
            )),
            ReadKey::Lookup(LookupKey::of(
                LookupSource::Manifest,
                &ctx,
                Some(&ctx.package),
                &["y".into()],
            )),
            ReadKey::Lookup(LookupKey::of(LookupSource::Head, &ctx, None, &["m".into()])),
            ReadKey::NameSet(package, ModuleKey::of(&ctx.modules)),
            ReadKey::Representation(a),
            ReadKey::ExecutedBody(a),
            ReadKey::LayoutBody(a),
            ReadKey::ExecutedSignature(a),
            ReadKey::CitedText(b),
            ReadKey::DeclarationCandidates(b),
            ReadKey::ValidationView(a),
            ReadKey::UniverseErrors,
            ReadKey::HeapRole,
            ReadKey::Environment,
            ReadKey::ValidationUniverse(package),
        ];
        let base: Vec<_> = keys
            .into_iter()
            .enumerate()
            .map(|(i, k)| (k, h(&i.to_string())))
            .collect();
        for seed in 0..24u64 {
            let mut rows: Vec<_> = (0..128).flat_map(|_| base.iter().copied()).collect();
            if seed % 2 != 0 {
                // Discovering a later conflict first must still report the
                // least key in the original canonical ordering.
                rows.extend([
                    (keys[12], h("bad-v")),
                    (keys[0], h("bad-z")),
                    (keys[1], h("bad-a")),
                    (keys[16], h("bad-u")),
                ]);
            }
            let mut random = seed + 1;
            for i in (1..rows.len()).rev() {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                rows.swap(i, random as usize % (i + 1));
            }
            let expected = original(rows.clone());
            let got = Reads::from_vec(rows.clone());
            assert_eq!(got, expected, "seed {seed}");
            // A builder over canonical sets, split anywhere, is the flat
            // union of what it was fed: the same rows and the same least
            // conflicting key.
            for parts in [2usize, 3, 7] {
                let mut builder = ReadsBuilder::new();
                let mut fed = Vec::new();
                let chunk = rows.len().div_ceil(parts);
                for (at, part) in rows.chunks(chunk).enumerate() {
                    if at == 0 {
                        for &(key, value) in part {
                            builder.record(key, value);
                        }
                        fed.extend_from_slice(part);
                    } else {
                        // Keep each set internally consistent, so every
                        // conflict meets only across merged sets.
                        let mut first = Vec::<(ReadKey, ContentHash)>::new();
                        for &(key, value) in part {
                            if !first.iter().any(|(seen, _)| *seen == key) {
                                first.push((key, value));
                            }
                        }
                        fed.extend_from_slice(&first);
                        builder.merge(&Reads::from_vec(first).unwrap());
                    }
                }
                assert_eq!(builder.finish(), original(fed), "seed {seed} parts {parts}");
            }
            if let (Ok(got), Ok(expected)) = (got, expected) {
                let mut bytes = vec![];
                got.encode(&mut bytes);
                let mut reference = vec![];
                expected.encode(&mut reference);
                assert_eq!(bytes, reference);
                assert_eq!(got.digest(), expected.digest());
                for (key, value) in &base {
                    assert_eq!(got.get(key), Some(*value));
                }
                let mut builder = ReadsBuilder::new();
                builder.merge(&got);
                builder.merge(&got);
                assert_eq!(builder.clone().finish(), Ok(expected));
                assert_eq!(builder.finish(), Ok(got));
            }
        }
        assert_eq!(Reads::from_vec(vec![]), Ok(Reads::empty()));
        let repeated = (0..128).flat_map(|_| base.iter().copied()).collect();
        let before = SORT_COMPARISONS.with(std::cell::Cell::get);
        assert_eq!(Reads::from_vec(repeated), original(base));
        let comparisons = SORT_COMPARISONS.with(std::cell::Cell::get) - before;
        assert!(
            comparisons < 256,
            "sort unique read keys, not duplicate observations: {comparisons}"
        );
    }

    #[test]
    fn read_allocation_debit_matches_live_aligned_buffers_and_last_owner_release() {
        let definitions = (0..32)
            .map(|index| def(&format!("allocation{index}")))
            .collect::<Vec<_>>();
        for count in [0, 1, 2, 3, 32] {
            let rows = if count == 2 {
                vec![
                    (ReadKey::Environment, ContentHash::default()),
                    (ReadKey::Signature(definitions[0]), ContentHash::default()),
                ]
            } else {
                definitions[..count]
                    .iter()
                    .map(|&id| (ReadKey::Signature(id), ContentHash::default()))
                    .collect::<Vec<_>>()
            };
            let (reads, allocated) =
                crate::test_alloc::measured(|| Reads::from_vec(rows.clone()).unwrap());
            let charged = reads.allocations().map(|(_, bytes)| bytes).sum::<usize>();
            assert_eq!(charged as isize, allocated.live, "read count={count}");
            let (_, released) = crate::test_alloc::measured(|| drop(reads));
            assert_eq!(released.live, -allocated.live, "read count={count}");
        }
    }

    #[test]
    fn shared_subsets_keep_order_get_encoding_hash_and_exclude_unobserved_rows() {
        use std::hash::{Hash, Hasher};
        let a = ReadKey::Signature(def("a"));
        let b = ReadKey::ExecutedBody(def("b"));
        let c = ReadKey::Signature(def("c"));
        let complete = Reads::from_vec(vec![(a, h("a")), (b, h("b")), (c, h("c"))]).unwrap();
        let hash = |reads: &Reads| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            reads.hash(&mut h);
            h.finish()
        };
        for values in [
            vec![],
            vec![(a, h("a"))],
            vec![(a, h("a")), (b, h("b"))],
            complete.iter().copied().collect(),
        ] {
            let original = Reads::from_vec(values).unwrap();
            let shared = original.share_rows(&complete).unwrap();
            assert_eq!(shared, original);
            assert_eq!(shared.len(), original.len());
            assert_eq!(shared.iter().len(), original.len());
            assert_eq!(hash(&shared), hash(&original));
            assert_eq!(shared.digest(), original.digest());
            let mut bytes = vec![];
            shared.encode(&mut bytes);
            let decoded = Reads::decode(&mut bytes.as_slice()).unwrap();
            assert_eq!(decoded, original);
            for key in [a, b, c] {
                assert_eq!(shared.get(&key), original.get(&key));
            }
            assert_eq!(shared.union(&complete), original.union(&complete));
            assert_eq!(shared.rows.as_ptr(), complete.rows.as_ptr());
        }
        let reversed = Reads::from_vec(vec![(b, h("b")), (a, h("a"))]).unwrap();
        let shared = reversed.share_rows(&complete).unwrap();
        let expected = vec![(a, h("a")), (b, h("b"))];
        assert_eq!(shared.iter().copied().collect::<Vec<_>>(), expected);
        let row_bytes = std::alloc::Layout::new::<[usize; 2]>()
            .extend(std::alloc::Layout::array::<(ReadKey, ContentHash)>(2).unwrap())
            .unwrap()
            .0
            .pad_to_align()
            .size();
        assert_eq!(shared.flat_backing_bytes(), Some(row_bytes));
        let flat = shared.flat_copy();
        assert_eq!(flat.iter().copied().collect::<Vec<_>>(), expected);
        assert_eq!(
            flat.allocations().map(|(_, bytes)| bytes).sum::<usize>(),
            row_bytes
        );
        let inconsistent = Reads::from_vec(vec![(a, h("other"))]).unwrap();
        assert!(inconsistent.share_rows(&complete).is_none());
        let missing = Reads::from_vec(vec![(ReadKey::Environment, h("e"))]).unwrap();
        assert!(missing.share_rows(&complete).is_none());
    }
    #[test]
    fn encoding_round_trips_through_paths_not_ids() {
        let context = FileContext {
            package: PackageId {
                name: "p".into(),
                version: "1".into(),
            },
            modules: vec!["m".into(), "n".into()],
        };
        let observations = vec![
            (ReadKey::Signature(def("a")), h("1")),
            (ReadKey::DeclarationCandidates(def("a")), h("2")),
            (
                ReadKey::Lookup(LookupKey::of(
                    LookupSource::Manifest,
                    &context,
                    None,
                    &["x".into(), "y".into()],
                )),
                h("3"),
            ),
            (
                ReadKey::NameSet(PackageKey::of(&context.package), ModuleKey::of(&[])),
                h("4"),
            ),
            (ReadKey::Environment, h("5")),
            (ReadKey::UniverseErrors, h("6")),
            (
                ReadKey::ValidationUniverse(PackageKey::of(&context.package)),
                h("7"),
            ),
        ];
        let reads = Reads::from_vec(observations.clone()).unwrap();
        let mut bytes = vec![];
        reads.encode(&mut bytes);
        let mut input = bytes.as_slice();
        let decoded = Reads::decode(&mut input).unwrap();
        assert_eq!(decoded, reads);
        assert!(input.is_empty());
        let ReadKey::Lookup(lookup) = reads
            .iter()
            .find(|(k, _)| k.kind() == ReadKind::Lookup)
            .unwrap()
            .0
        else {
            unreachable!()
        };
        assert_eq!(lookup.context(), context);
        assert_eq!(lookup.segments(), vec!["x".to_string(), "y".to_string()]);
        assert_eq!(lookup.qualifier(), None);
    }

    #[test]
    fn diagnostic_lookup_reads_are_distinct_and_reject_old_encodings() {
        let context = FileContext {
            package: def("owner").path().0.package.clone(),
            modules: vec!["m".into()],
        };
        let segments = ["missing".into()];
        let identity = ReadKey::Lookup(LookupKey::of(
            LookupSource::Manifest,
            &context,
            None,
            &segments,
        ));
        let candidates = ReadKey::Lookup(LookupKey::of(
            LookupSource::Candidates,
            &context,
            None,
            &segments,
        ));
        let reads =
            Reads::from_vec(vec![(identity, h("absent")), (candidates, h("repairs"))]).unwrap();
        assert_eq!(reads.len(), 2);
        let mut bytes = vec![];
        reads.encode(&mut bytes);
        assert_eq!(&bytes[..4], b"HRD4");
        assert_eq!(Reads::decode(&mut bytes.as_slice()), Some(reads.clone()));
        for old_magic in [b"HRD2", b"HRD3"] {
            let mut old = bytes.clone();
            old[..4].copy_from_slice(old_magic);
            assert!(Reads::decode(&mut old.as_slice()).is_none());
        }
        let mut truncated = bytes.clone();
        truncated.pop();
        assert!(Reads::decode(&mut truncated.as_slice()).is_none());
        let mut noncanonical = b"HRD4".to_vec();
        noncanonical.extend(2u32.to_le_bytes());
        for (key, value) in [
            (ReadKey::Environment, h("environment")),
            (ReadKey::UniverseErrors, h("errors")),
        ] {
            key.encode(&mut noncanonical);
            noncanonical.extend(value.0);
        }
        assert!(Reads::decode(&mut noncanonical.as_slice()).is_none());
        // The old encoding began directly with the row count.
        assert!(Reads::decode(&mut &bytes[4..]).is_none());
        assert!(Reads::decode(&mut &[0u8; 4][..]).is_none());
        let mut unknown = [ReadKind::Lookup as u8, 4];
        assert!(decode_key(&mut unknown.as_slice()).is_none());
        unknown[1] = LookupSource::Candidates as u8;
        assert!(
            decode_key(&mut unknown.as_slice()).is_none(),
            "truncated lookup"
        );
    }

    #[test]
    fn changed_reads_are_found() {
        let reads = Reads::from_vec(vec![
            (ReadKey::Signature(def("a")), h("a")),
            (ReadKey::Environment, h("e")),
        ])
        .unwrap();
        assert_eq!(
            reads.first_changed(|k| reads.get(k)),
            None,
            "unchanged reads verify"
        );
        assert_eq!(
            reads.first_changed(|k| match k {
                ReadKey::Environment => Some(h("e2")),
                k => reads.get(k),
            }),
            Some(ReadKey::Environment)
        );
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    fn persisted_key_lengths_cover_every_read_kind_and_unicode_lookup_frame() {
        let path = DefPath(hirsel_syntax::ItemPath {
            package: PackageId {
                name: "pkg🐑".into(),
                version: "2".into(),
            },
            modules: vec!["α".into(), "module".into()],
            name: "field\"".into(),
        });
        let id = DefId::intern(&path);
        let ctx = FileContext {
            package: path.0.package.clone(),
            modules: path.0.modules.clone(),
        };
        let package = PackageKey::of(&ctx.package);
        let mut keys = vec![
            ReadKey::Signature(id),
            ReadKey::FailedSignature(id),
            ReadKey::Representation(id),
            ReadKey::ExecutedBody(id),
            ReadKey::LayoutBody(id),
            ReadKey::DeclaredSignature(id),
            ReadKey::PrivateSummary(id),
            ReadKey::ExecutedSignature(id),
            ReadKey::CitedText(id),
            ReadKey::DeclarationCandidates(id),
            ReadKey::ValidationView(id),
            ReadKey::NameSet(package, ModuleKey::of(&ctx.modules)),
            ReadKey::ValidationUniverse(package),
            ReadKey::UniverseErrors,
            ReadKey::HeapRole,
            ReadKey::Environment,
        ];
        for source in [
            LookupSource::Provider,
            LookupSource::Manifest,
            LookupSource::Head,
            LookupSource::Candidates,
        ] {
            for qualifier in [None, Some(&ctx.package)] {
                keys.push(ReadKey::Lookup(LookupKey::of(
                    source,
                    &ctx,
                    qualifier,
                    &["δ".into(), "x🐑".into()],
                )));
            }
        }
        for key in keys {
            let mut bytes = vec![];
            key.encode(&mut bytes);
            assert_eq!(key.encoded_bytes(), Some(bytes.len()), "{key:?}");
        }
        let mut bytes = vec![];
        encode_def_path(&mut bytes, &path);
        assert_eq!(encoded_def_path_bytes(&path), Some(bytes.len()));
    }
}

impl hirsel_product_codec::decode_schema::Walk for Reads {
    fn walk(scan: &mut hirsel_product_codec::decode_schema::Scan<'_>) -> Option<()> {
        use hirsel_product_codec::decode_schema::Scan;
        fn text(scan: &mut Scan<'_>) -> Option<()> {
            let n = scan.word()? as usize;
            std::str::from_utf8(scan.take(n)?).ok()?;
            scan.add(n)
        }
        fn path(scan: &mut Scan<'_>) -> Option<()> {
            text(scan)?;
            text(scan)?;
            let modules = scan.word()? as usize;
            scan.add(modules.checked_mul(3 * size_of::<String>())?)?;
            for _ in 0..modules {
                text(scan)?;
            }
            text(scan)?;
            scan.add(size_of::<DefPath>())
        }
        let n = scan.length()?;
        // Raw payload plus row Vec growth, Arc conversion and path scopes.
        scan.add(n.checked_mul(2)?)?;
        let bytes = scan.take(n)?;
        let mut rows = Scan::raw_reads(bytes);
        if rows.take(4)? != b"HRD4" {
            return None;
        }
        let count = rows.word()? as usize;
        if count > bytes.len() / (1 + size_of::<ContentHash>()) {
            return None;
        }
        scan.add(count.checked_mul(4 * size_of::<(ReadKey, ContentHash)>() + 1)?)?;
        for _ in 0..count {
            let kind = *ReadKind::ALL.get(rows.read_u8()? as usize)?;
            match kind {
                ReadKind::Lookup => {
                    if rows.read_u8()? > 3 {
                        return None;
                    }
                    text(&mut rows)?;
                    text(&mut rows)?;
                    match rows.read_u8()? {
                        0 => (),
                        1 => text(&mut rows)?,
                        _ => return None,
                    }
                    text(&mut rows)?;
                }
                ReadKind::NameSet => {
                    text(&mut rows)?;
                    text(&mut rows)?;
                }
                ReadKind::ValidationUniverse => text(&mut rows)?,
                ReadKind::UniverseErrors | ReadKind::HeapRole | ReadKind::Environment => (),
                ReadKind::Signature
                | ReadKind::FailedSignature
                | ReadKind::Representation
                | ReadKind::ExecutedBody
                | ReadKind::ExecutedSignature
                | ReadKind::CitedText
                | ReadKind::DeclarationCandidates
                | ReadKind::ValidationView
                | ReadKind::LayoutBody
                | ReadKind::DeclaredSignature
                | ReadKind::PrivateSummary => path(&mut rows)?,
            }
            rows.take(32)?;
        }
        scan.add(usize::try_from(rows.finish()?).ok()?)
    }
}
