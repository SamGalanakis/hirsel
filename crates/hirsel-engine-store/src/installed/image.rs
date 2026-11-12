//! The installed-package image: one header (`PACKAGE`, schema 5) and the
//! content-addressed sections it lists. The verifier registration MACs the
//! header association and the header reference digests the header bytes, so
//! every section reference below is authenticated through the header.
//!
//! Construction runs once, at publication, over the new in-memory package and
//! performs every whole-package check. Open reads the header, the row,
//! content and index sections and the source texts, each checked against its
//! reference. A definition chunk is read, and checked against its reference,
//! when a definition in it is first demanded. That definition then runs its
//! own descriptor checks. A bad descriptor nobody demands is therefore no
//! longer refused at open; construction refuses it, so only a publisher that
//! skips construction can produce one.
use super::semantic::check_definition;
use super::*;
use bincode::Options;
use std::sync::OnceLock;

/// Association rows, deduplicated and sorted by address: 112 bytes each.
const ROWS: Domain = Domain {
    kind: 0x4601_0010,
    schema: 1,
};
/// Every listed content reference, deduplicated and sorted: 48 bytes each.
const CONTENTS: Domain = Domain {
    kind: 0x4601_0011,
    schema: 1,
};
/// The definition index: rows in path order, then their positions in
/// fingerprint order.
const INDEX: Domain = Domain {
    kind: 0x4601_0012,
    schema: 1,
};
/// The definition descriptors of one source file.
const CHUNK: Domain = Domain {
    kind: 0x4601_0013,
    schema: 1,
};
/// The text of one source file.
const TEXT: Domain = Domain {
    kind: 0x4601_0014,
    schema: 1,
};
// The sections later lands fill, each separately versioned and absent until
// then: the name index, signature and representation edges, conventions,
// and signature rows.
const NAMES: Domain = Domain {
    kind: 0x4601_0015,
    schema: 1,
};
const EDGES: Domain = Domain {
    kind: 0x4601_0016,
    schema: 1,
};
const CONVENTIONS: Domain = Domain {
    kind: 0x4601_0017,
    schema: 1,
};
const SIGNATURES: Domain = Domain {
    kind: 0x4601_0018,
    schema: 1,
};

const ROW: usize = 112;
const REFERENCE: usize = 48;
/// Fingerprint, chunk, offset and length.
const ENTRY: usize = 44;

/// The section directory, frozen for every land of the cutover.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Sections {
    pub(super) rows: Reference,
    pub(super) contents: Reference,
    pub(super) index: Reference,
    pub(super) chunks: Vec<Reference>,
    /// One per source, in source order.
    texts: Vec<Reference>,
    names: Option<Reference>,
    edges: Option<Reference>,
    conventions: Option<Reference>,
    signatures: Option<Reference>,
}
impl Sections {
    /// The sections open does not read: the definition chunks and the
    /// sections of later lands. An audit verifies these; open checks the rest.
    pub(super) fn unread(&self) -> impl Iterator<Item = Reference> + '_ {
        self.chunks.iter().copied().chain(
            [self.names, self.edges, self.conventions, self.signatures]
                .into_iter()
                .flatten(),
        )
    }
    pub(super) fn references(&self) -> impl Iterator<Item = Reference> + '_ {
        [self.rows, self.contents, self.index]
            .into_iter()
            .chain(self.chunks.iter().copied())
            .chain(self.texts.iter().copied())
            .chain(
                [self.names, self.edges, self.conventions, self.signatures]
                    .into_iter()
                    .flatten(),
            )
    }
    fn domains(&self) -> bool {
        let all = |references: &[Reference], domain| references.iter().all(|r| r.domain == domain);
        let optional =
            |reference: Option<Reference>, domain| reference.is_none_or(|r| r.domain == domain);
        self.rows.domain == ROWS
            && self.contents.domain == CONTENTS
            && self.index.domain == INDEX
            && all(&self.chunks, CHUNK)
            && all(&self.texts, TEXT)
            && optional(self.names, NAMES)
            && optional(self.edges, EDGES)
            && optional(self.conventions, CONVENTIONS)
            && optional(self.signatures, SIGNATURES)
    }
}

#[derive(Serialize, Deserialize)]
struct Source {
    path: PathBuf,
    logical: PathBuf,
    context: FileContext,
}

#[derive(Serialize, Deserialize)]
struct Header {
    compiler: ContentHash,
    package: PackageId,
    manifest: Vec<u8>,
    sources: Vec<Source>,
    dependencies: Vec<PackageId>,
    inputs: InstalledInputs,
    sections: Sections,
}

fn options() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_RECORD as u64)
        .reject_trailing_bytes()
}

fn put_reference(out: &mut Vec<u8>, reference: &Reference) {
    out.extend_from_slice(&reference.output.0.0);
    out.extend_from_slice(&reference.domain.kind.to_le_bytes());
    out.extend_from_slice(&reference.domain.schema.to_le_bytes());
    out.extend_from_slice(&reference.len.to_le_bytes());
}
fn get_reference(bytes: &[u8]) -> Option<Reference> {
    Some(Reference {
        output: hirsel_artifact_store::Output(ContentHash(bytes.get(..32)?.try_into().ok()?)),
        domain: Domain {
            kind: u32::from_le_bytes(bytes.get(32..36)?.try_into().ok()?),
            schema: u32::from_le_bytes(bytes.get(36..40)?.try_into().ok()?),
        },
        len: u64::from_le_bytes(bytes.get(40..48)?.try_into().ok()?),
    })
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}
/// A counted table of fixed-width records filling `bytes` exactly.
fn table(bytes: &[u8], width: usize) -> Option<&[u8]> {
    let (count, rest) = bytes.split_at_checked(8)?;
    let count = usize::try_from(u64::from_le_bytes(count.try_into().ok()?)).ok()?;
    (count.checked_mul(width)? == rest.len()).then_some(rest)
}

/// Everything one definition's descriptor is checked against: the package's
/// identity and its row and content sections.
pub(super) struct Context<'a> {
    pub package: &'a PackageId,
    pub compiler: ContentHash,
    pub manifest: ContentHash,
    pub rows: &'a [(ContentHash, Association)],
    pub contents: &'a [Reference],
}

/// The whole-package checks, over the new in-memory package: every
/// descriptor's own checks, and fingerprints that never alias two paths.
/// Rows and contents come from maps, so they are unique and sorted by
/// construction, and `index_definitions` already refused duplicate semantic
/// paths.
pub(super) fn check(
    context: &Context<'_>,
    definitions: &BTreeMap<DefPath, InstalledDefinition>,
) -> Result<(), String> {
    for (path, definition) in definitions {
        check_definition(context, path, definition)?;
    }
    let mut fingerprints = definitions
        .keys()
        .map(|path| path.fingerprint().0)
        .collect::<Vec<_>>();
    fingerprints.sort_unstable();
    if fingerprints.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("two installed definition paths share a fingerprint".into());
    }
    Ok(())
}

/// Write every section and the header, and return the header's reference.
/// Nothing is associated: the caller publishes the header after checking that
/// every listed row, content and section is present.
pub(super) fn build(
    store: &hirsel_artifact_store::Store,
    installed: &mut InstalledPackage,
    definitions: &BTreeMap<DefPath, InstalledDefinition>,
) -> std::io::Result<Reference> {
    let mut rows = Vec::with_capacity(8 + installed.rows.len() * ROW);
    rows.extend_from_slice(&(installed.rows.len() as u64).to_le_bytes());
    for (address, row) in installed.rows.iter() {
        rows.extend_from_slice(&address.0);
        rows.extend_from_slice(&row.action.0.0);
        put_reference(&mut rows, &row.reference);
    }
    let mut contents = Vec::with_capacity(8 + installed.contents.len() * REFERENCE);
    contents.extend_from_slice(&(installed.contents.len() as u64).to_le_bytes());
    for reference in installed.contents.iter() {
        put_reference(&mut contents, reference);
    }
    // One chunk per source file; the index lists definitions in path order.
    let mut files = BTreeMap::<&Path, Vec<u8>>::new();
    let mut entries = Vec::with_capacity(definitions.len());
    for (path, definition) in definitions {
        let bytes = options()
            .serialize(definition)
            .map_err(std::io::Error::other)?;
        let chunk = files
            .entry(&definition.ownership.source_logical)
            .or_default();
        let offset = u32::try_from(chunk.len()).map_err(std::io::Error::other)?;
        let len = u32::try_from(bytes.len()).map_err(std::io::Error::other)?;
        chunk.extend_from_slice(&bytes);
        entries.push((
            path.fingerprint().0,
            &definition.ownership.source_logical,
            offset,
            len,
        ));
    }
    let numbers = files
        .keys()
        .enumerate()
        .map(|(number, file)| (*file, number as u32))
        .collect::<BTreeMap<_, _>>();
    let mut index = Vec::with_capacity(8 + entries.len() * (ENTRY + 4));
    index.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for (fingerprint, file, offset, len) in &entries {
        index.extend_from_slice(fingerprint);
        index.extend_from_slice(&numbers[file.as_path()].to_le_bytes());
        index.extend_from_slice(&offset.to_le_bytes());
        index.extend_from_slice(&len.to_le_bytes());
    }
    let mut order = (0..entries.len() as u32).collect::<Vec<_>>();
    order.sort_unstable_by_key(|&at| entries[at as usize].0);
    for at in order {
        index.extend_from_slice(&at.to_le_bytes());
    }
    let sections = Sections {
        rows: store.put(ROWS, &rows)?,
        contents: store.put(CONTENTS, &contents)?,
        index: store.put(INDEX, &index)?,
        chunks: files
            .values()
            .map(|chunk| store.put(CHUNK, chunk))
            .collect::<std::io::Result<_>>()?,
        texts: installed
            .sources
            .iter()
            .map(|source| store.put(TEXT, source.text.as_bytes()))
            .collect::<std::io::Result<_>>()?,
        names: None,
        edges: None,
        conventions: None,
        signatures: None,
    };
    let header = options()
        .serialize(&Header {
            compiler: installed.compiler,
            package: installed.package.clone(),
            manifest: installed.manifest.clone(),
            sources: installed
                .sources
                .iter()
                .map(|source| Source {
                    path: source.path.clone(),
                    logical: source.logical.clone(),
                    context: source.context.clone(),
                })
                .collect(),
            dependencies: installed.dependencies.clone(),
            inputs: installed.inputs.clone(),
            sections: sections.clone(),
        })
        .map_err(std::io::Error::other)?;
    installed.sections = Some(sections);
    store.put(PACKAGE, &header)
}

/// Open the header `bytes` of an installation: read its row, content and
/// index sections and source texts, each checked against its reference.
/// Definition chunks are left to first demand.
pub(super) fn read(
    store: &hirsel_artifact_store::Store,
    bytes: &[u8],
) -> Result<InstalledPackage, String> {
    let header: Header = options()
        .deserialize(bytes)
        .map_err(|error| format!("corrupt installed package: {error}"))?;
    let sections = header.sections;
    if !sections.domains() || sections.texts.len() != header.sources.len() {
        return Err("invalid installed package sections".into());
    }
    let section = |reference: Reference| {
        store
            .get_owned(reference)
            .map_err(|_| "corrupt installed package section".to_owned())
    };
    let invalid = || "invalid installed package section".to_owned();
    let rows = section(sections.rows)?;
    let rows = table(&rows, ROW)
        .ok_or_else(invalid)?
        .as_chunks::<ROW>()
        .0
        .iter()
        .map(|row| {
            Some((
                ContentHash(row.get(..32)?.try_into().ok()?),
                Association {
                    action: Action(ContentHash(row.get(32..64)?.try_into().ok()?)),
                    reference: get_reference(row.get(64..)?)?,
                },
            ))
        })
        .collect::<Option<Arc<[_]>>>()
        .ok_or_else(invalid)?;
    let contents = section(sections.contents)?;
    let contents = table(&contents, REFERENCE)
        .ok_or_else(invalid)?
        .as_chunks::<REFERENCE>()
        .0
        .iter()
        .map(|reference| get_reference(reference))
        .collect::<Option<Arc<[_]>>>()
        .ok_or_else(invalid)?;
    let index = section(sections.index)?;
    let count = table(&index, ENTRY + 4).ok_or_else(invalid)?.len() / (ENTRY + 4);
    // Every lookup binary-searches these, so a misordered section would
    // report a listed row, content or definition absent. Strict order also
    // refuses two rows at one address, as `attach` did.
    let fingerprints = &index[8..8 + count * ENTRY];
    let fingerprint = |k: usize| {
        let at = u32_at(&index, 8 + count * ENTRY + 4 * k) as usize;
        (at < count).then(|| &fingerprints[at * ENTRY..at * ENTRY + 32])
    };
    if !rows.windows(2).all(|pair| pair[0].0 < pair[1].0)
        || !contents.windows(2).all(|pair| pair[0] < pair[1])
        || !(0..count)
            .map(fingerprint)
            .collect::<Option<Vec<_>>>()
            .is_some_and(|order| order.windows(2).all(|pair| pair[0] < pair[1]))
    {
        return Err(invalid());
    }
    // The entries tile every chunk: each range in bounds, none overlapping
    // another, and no chunk byte left to no definition. A range redirected
    // onto another definition's bytes is refused here; a swap of two ranges
    // keeps the tiling and is refused by the fingerprint check on load.
    let mut ranges = (0..count)
        .map(|at| {
            let entry = &fingerprints[at * ENTRY..(at + 1) * ENTRY];
            (
                u32_at(entry, 32),
                u64::from(u32_at(entry, 36)),
                u64::from(u32_at(entry, 40)),
            )
        })
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    let mut ends = vec![0u64; sections.chunks.len()];
    for (chunk, offset, len) in ranges {
        let end = ends.get_mut(chunk as usize).ok_or_else(invalid)?;
        if offset != *end {
            return Err(invalid());
        }
        *end = offset + len;
    }
    if !ends
        .iter()
        .eq(sections.chunks.iter().map(|chunk| &chunk.len))
    {
        return Err(invalid());
    }
    let sources = header
        .sources
        .into_iter()
        .zip(&sections.texts)
        .map(|(source, &text)| {
            Ok(InstalledSource {
                path: source.path,
                logical: source.logical,
                context: source.context,
                text: String::from_utf8(section(text)?.to_vec())
                    .map_err(|_| "invalid installed source text".to_owned())?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let definitions = Definitions {
        store: store.clone(),
        index,
        count,
        chunks: sections.chunks.clone().into(),
        loaded: (0..sections.chunks.len())
            .map(|_| OnceLock::new())
            .collect(),
        slots: (0..count).map(|_| OnceLock::new()).collect(),
        package: header.package.clone(),
        compiler: header.compiler,
        manifest: ContentHash::of_bytes(&header.manifest),
        rows: rows.clone(),
        contents: contents.clone(),
    };
    Ok(InstalledPackage {
        compiler: header.compiler,
        identity: ContentHash::default(),
        package: header.package,
        manifest: header.manifest,
        sources,
        dependencies: header.dependencies,
        inputs: header.inputs,
        sections: Some(sections),
        rows,
        contents,
        definitions: Some(Arc::new(definitions)),
        records: Default::default(),
        sources_owner: None,
    })
}

/// The definitions of an opened image, each decoded and checked on its first
/// demand from the chunk that holds it.
pub(crate) struct Definitions {
    store: hirsel_artifact_store::Store,
    index: Bytes,
    count: usize,
    chunks: Box<[Reference]>,
    loaded: Box<[OnceLock<Result<Bytes, String>>]>,
    slots: Box<[OnceLock<Result<InstalledDefinition, String>>]>,
    package: PackageId,
    compiler: ContentHash,
    manifest: ContentHash,
    rows: Arc<[(ContentHash, Association)]>,
    contents: Arc<[Reference]>,
}
impl std::fmt::Debug for Definitions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Definitions({} in {} chunks)",
            self.count,
            self.chunks.len()
        )
    }
}
impl Definitions {
    fn entry(&self, at: usize) -> &[u8] {
        &self.index[8 + at * ENTRY..8 + (at + 1) * ENTRY]
    }
    /// The index entry whose fingerprint is `path`'s. Open checked the order.
    fn find(&self, path: &DefPath) -> Option<usize> {
        let fingerprint = path.fingerprint().0;
        let order = &self.index[8 + self.count * ENTRY..];
        let position = |k: usize| u32_at(order, 4 * k) as usize;
        let mut range = 0..self.count;
        loop {
            if range.is_empty() {
                return None;
            }
            let middle = range.start + range.len() / 2;
            let at = position(middle);
            match self.entry(at)[..32].cmp(&fingerprint) {
                std::cmp::Ordering::Equal => return Some(at),
                std::cmp::Ordering::Less => range.start = middle + 1,
                std::cmp::Ordering::Greater => range.end = middle,
            }
        }
    }
    /// Whether the image lists a definition at `path`, without reading or
    /// checking it. The fingerprint is a domain-separated digest of the full
    /// path and construction refused two paths sharing one.
    pub(crate) fn installs(&self, path: &DefPath) -> bool {
        self.find(path).is_some()
    }
    /// The definition at `path`, or none when the image lists no definition
    /// there. A definition whose chunk or descriptor fails its checks is
    /// refused, never reported absent.
    pub(crate) fn get(&self, path: &DefPath) -> Result<Option<&InstalledDefinition>, String> {
        let Some(at) = self.find(path) else {
            return Ok(None);
        };
        let definition = self.slots[at]
            .get_or_init(|| self.load(at))
            .as_ref()
            .map_err(Clone::clone)?;
        // Construction refuses two paths with one fingerprint, so another
        // path here means `path` itself is not installed.
        Ok((definition.key.definition == *path).then_some(definition))
    }
    fn load(&self, at: usize) -> Result<InstalledDefinition, String> {
        hirsel_intern::work::count(hirsel_intern::work::Counter::InstalledDefinitionDecodes);
        let entry = self.entry(at);
        let (chunk, offset, len) = (
            u32_at(entry, 32) as usize,
            u32_at(entry, 36) as usize,
            u32_at(entry, 40) as usize,
        );
        let reference = *self
            .chunks
            .get(chunk)
            .ok_or("invalid installed definition index")?;
        let bytes = self.loaded[chunk]
            .get_or_init(|| {
                self.store
                    .get_owned(reference)
                    .map_err(|_| "corrupt installed definition chunk".to_owned())
            })
            .as_ref()
            .map_err(Clone::clone)?;
        let bytes = offset
            .checked_add(len)
            .and_then(|end| bytes.get(offset..end))
            .ok_or("invalid installed definition index")?;
        let definition: InstalledDefinition = options()
            .deserialize(bytes)
            .map_err(|error| format!("invalid installed definition descriptor: {error}"))?;
        if definition.key.definition.fingerprint().0 != entry[..32] {
            return Err("invalid installed definition index".into());
        }
        check_definition(
            &Context {
                package: &self.package,
                compiler: self.compiler,
                manifest: self.manifest,
                rows: &self.rows,
                contents: &self.contents,
            },
            &definition.key.definition,
            &definition,
        )?;
        Ok(definition)
    }
    /// Decode and check every listed definition, keeping each for its first
    /// demand: the whole-package admission an authenticated open defers.
    pub(crate) fn audit(&self) -> Result<(), String> {
        (0..self.count).try_for_each(|at| {
            self.slots[at]
                .get_or_init(|| self.load(at))
                .as_ref()
                .map(drop)
                .map_err(Clone::clone)
        })
    }
    /// The most this owner can come to hold: the index, every chunk, and the
    /// decoded definitions, whose heap parts are no larger than their encoding.
    pub(crate) fn bytes(&self) -> usize {
        let chunks = self
            .chunks
            .iter()
            .map(|chunk| usize::try_from(chunk.len).unwrap_or(usize::MAX))
            .fold(0usize, usize::saturating_add);
        std::mem::size_of::<Self>()
            + self.index.len()
            + std::mem::size_of_val(&*self.chunks)
            + std::mem::size_of_val(&*self.loaded)
            + std::mem::size_of_val(&*self.slots)
            + chunks.saturating_mul(2)
    }
}

/// Replace the sections of the header at `header` as `edit` says and store the
/// new header: a test publisher's image that construction would not write.
#[cfg(test)]
pub(super) fn republish(
    store: &hirsel_artifact_store::Store,
    header: Reference,
    edit: impl FnOnce(&mut Sections),
) -> Reference {
    let mut header: Header = options()
        .deserialize(&store.get_owned(header).unwrap())
        .unwrap();
    edit(&mut header.sections);
    store
        .put(PACKAGE, &options().serialize(&header).unwrap())
        .unwrap()
}
