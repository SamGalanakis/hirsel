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
    /// V's digest of a definition's view in the named package's base V
    /// universe. The universe's package is explicit, as for
    /// `ValidationUniverse`, because execution-read unions cross packages.
    ValidationView(PackageKey, DefId),
    /// The aggregate digest of the named package's V universe errors.
    UniverseErrors(PackageKey),
    /// The heap role selected in the named package's V universe.
    HeapRole(PackageKey),
    /// The query environment's hash.
    Environment,
    /// All V input views in the originating package's private universe.
    /// The package is explicit because execution-read unions cross packages.
    ValidationUniverse(PackageKey),
    /// A representation body consumed by H, without interpreting its code.
    LayoutBody(DefId),
    /// Normalized declaration structure.
    DeclaredSignature(DefId),
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
}
impl ReadKind {
    pub const ALL: [ReadKind; 16] = [
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
    ];
    pub fn name(self) -> &'static str {
        match self {
            ReadKind::Signature => "signature",
            ReadKind::DeclaredSignature => "declared signature",
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
            ReadKey::FailedSignature(_) => ReadKind::FailedSignature,
            ReadKey::Lookup(_) => ReadKind::Lookup,
            ReadKey::NameSet(..) => ReadKind::NameSet,
            ReadKey::Representation(_) => ReadKind::Representation,
            ReadKey::ExecutedBody(_) => ReadKind::ExecutedBody,
            ReadKey::LayoutBody(_) => ReadKind::LayoutBody,
            ReadKey::ExecutedSignature(_) => ReadKind::ExecutedSignature,
            ReadKey::CitedText(_) => ReadKind::CitedText,
            ReadKey::DeclarationCandidates(_) => ReadKind::DeclarationCandidates,
            ReadKey::ValidationView(..) => ReadKind::ValidationView,
            ReadKey::UniverseErrors(_) => ReadKind::UniverseErrors,
            ReadKey::HeapRole(_) => ReadKind::HeapRole,
            ReadKey::Environment => ReadKind::Environment,
            ReadKey::ValidationUniverse(_) => ReadKind::ValidationUniverse,
        }
    }
    /// The definition this read names, if any.
    pub fn definition(&self) -> Option<DefId> {
        match *self {
            ReadKey::Signature(d)
            | ReadKey::DeclaredSignature(d)
            | ReadKey::FailedSignature(d)
            | ReadKey::Representation(d)
            | ReadKey::ExecutedBody(d)
            | ReadKey::LayoutBody(d)
            | ReadKey::ExecutedSignature(d)
            | ReadKey::CitedText(d)
            | ReadKey::DeclarationCandidates(d)
            | ReadKey::ValidationView(_, d) => Some(d),
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
            | Self::FailedSignature(d)
            | Self::Representation(d)
            | Self::ExecutedBody(d)
            | Self::LayoutBody(d)
            | Self::ExecutedSignature(d)
            | Self::CitedText(d)
            | Self::DeclarationCandidates(d) => encoded_def_path_bytes(d.path())?,
            Self::ValidationView(p, d) => {
                text(p.0.as_str())?.checked_add(encoded_def_path_bytes(d.path())?)?
            }
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
            Self::ValidationUniverse(p) | Self::UniverseErrors(p) | Self::HeapRole(p) => {
                text(p.0.as_str())?
            }
            Self::Environment => 0,
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
            | ReadKey::FailedSignature(d)
            | ReadKey::Representation(d)
            | ReadKey::ExecutedBody(d)
            | ReadKey::LayoutBody(d)
            | ReadKey::ExecutedSignature(d)
            | ReadKey::CitedText(d)
            | ReadKey::DeclarationCandidates(d) => path(out, d),
            ReadKey::ValidationView(package, d) => {
                text(out, package.0.as_str());
                path(out, d);
            }
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
            ReadKey::ValidationUniverse(package)
            | ReadKey::UniverseErrors(package)
            | ReadKey::HeapRole(package) => text(out, package.0.as_str()),
            ReadKey::Environment => {}
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
        // HRD5 is canonical rows only, with V's universe reads naming their
        // package; earlier framings are rejected.
        out.extend(b"HRD5");
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
    /// Visit HRD5's canonical rows with the same framing and row-order
    /// checks as decode.
    pub fn visit_encoded(
        mut input: &[u8],
        mut visit: impl FnMut(usize, (ReadKey, ContentHash)) -> Option<()>,
    ) -> Option<usize> {
        if take(&mut input, 4)? != b"HRD5" {
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
        if take(input, 4)? != b"HRD5" {
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
        ContentHash::of_parts("hirsel.reads.v3", [bytes.as_slice()])
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
        ReadKind::FailedSignature => ReadKey::FailedSignature(take_def(input)?),
        ReadKind::Representation => ReadKey::Representation(take_def(input)?),
        ReadKind::ExecutedBody => ReadKey::ExecutedBody(take_def(input)?),
        ReadKind::LayoutBody => ReadKey::LayoutBody(take_def(input)?),
        ReadKind::ExecutedSignature => ReadKey::ExecutedSignature(take_def(input)?),
        ReadKind::CitedText => ReadKey::CitedText(take_def(input)?),
        ReadKind::DeclarationCandidates => ReadKey::DeclarationCandidates(take_def(input)?),
        ReadKind::ValidationView => {
            let package = PackageKey(Symbol::intern(take_text(input)?));
            ReadKey::ValidationView(package, take_def(input)?)
        }
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
        ReadKind::UniverseErrors => {
            ReadKey::UniverseErrors(PackageKey(Symbol::intern(take_text(input)?)))
        }
        ReadKind::HeapRole => ReadKey::HeapRole(PackageKey(Symbol::intern(take_text(input)?))),
        ReadKind::Environment => ReadKey::Environment,
        ReadKind::ValidationUniverse => {
            ReadKey::ValidationUniverse(PackageKey(Symbol::intern(take_text(input)?)))
        }
    })
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
        if rows.take(4)? != b"HRD5" {
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
                ReadKind::ValidationUniverse | ReadKind::UniverseErrors | ReadKind::HeapRole => {
                    text(&mut rows)?
                }
                ReadKind::ValidationView => {
                    text(&mut rows)?;
                    path(&mut rows)?;
                }
                ReadKind::Environment => (),
                ReadKind::Signature
                | ReadKind::FailedSignature
                | ReadKind::Representation
                | ReadKind::ExecutedBody
                | ReadKind::ExecutedSignature
                | ReadKind::CitedText
                | ReadKind::DeclarationCandidates
                | ReadKind::LayoutBody
                | ReadKind::DeclaredSignature => path(&mut rows)?,
            }
            rows.take(32)?;
        }
        scan.add(usize::try_from(rows.finish()?).ok()?)
    }
}
