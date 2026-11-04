//! Complete installation checks in physical record order. Git
//! 7e34d8174dcda3435d1404c9a30bef46afe6c13e, packfile.c:1966-2007:
//! traverse one pack in physical order. Here selected offsets are sorted from
//! our portable index, and each complete reference is checked independently.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// An index into one VerifiedRecords owner, never a portable reference.
#[derive(Clone, Copy, Debug)]
pub struct VerifiedReference(u32);

/// Operation-owned integrity handles. These supply no action or V authority.
/// Every new operation constructs fresh handles from its selected store.
#[derive(Debug)]
pub struct VerifiedRecords {
    store: Store,
    scope: Arc<()>,
    contents: Box<[(Reference, Location)]>,
    backings: Box<[Backing]>,
}
#[derive(Clone, Copy, Debug)]
struct Location {
    backing: u32,
    start: u32,
}
#[derive(Debug)]
enum Backing {
    Memory(Bytes),
    #[cfg(unix)]
    Mapped(PinnedFile),
}
#[cfg(unix)]
#[derive(Debug)]
struct PinnedFile {
    path: PathBuf,
    file: Arc<File>,
    len: usize,
}
impl VerifiedRecords {
    pub fn handle(&self, reference: Reference) -> Option<VerifiedReference> {
        self.contents
            .binary_search_by_key(&reference, |(r, _)| *r)
            .ok()
            .and_then(|index| u32::try_from(index).ok())
            .map(VerifiedReference)
    }
    pub fn reference(&self, handle: VerifiedReference) -> Option<Reference> {
        self.contents
            .get(handle.0 as usize)
            .map(|(reference, _)| *reference)
    }
    pub fn get(&self, reference: Reference) -> Option<Bytes> {
        let handle = self.handle(reference)?;
        let (_, location) = self.contents.get(handle.0 as usize)?;
        match self.backings.get(location.backing as usize)? {
            Backing::Memory(bytes) => Some(bytes.clone()),
            #[cfg(unix)]
            Backing::Mapped(file) => {
                let region = self
                    .store
                    .load_region(
                        &file.path,
                        Counter::RegionRemaps,
                        Some((&file.file, &self.scope)),
                        true,
                        || Region::map_file(file.file.clone(), file.len),
                    )
                    .ok()?;
                let start = location.start as usize;
                let end = start.checked_add(usize::try_from(reference.len).ok()?)?;
                if end > region.bytes().len() {
                    return None;
                }
                Some(
                    Bytes {
                        region,
                        range: start..end,
                    }
                    .with_reference(reference),
                )
            }
        }
    }
    /// Charge retained handles and owned buffers. Unix file mappings stay in
    /// the physical store's existing bounded cache; demanded views carry their
    /// complete mapping charge through Bytes::visit_retained_allocations.
    pub fn visit_retained_allocations(&self, mut visit: impl FnMut(usize, usize)) {
        visit(
            self.store.root.as_os_str().as_encoded_bytes().as_ptr() as usize,
            self.store.root.as_os_str().len(),
        );
        visit(Arc::as_ptr(&self.scope) as usize, 2 * size_of::<usize>());
        if !self.contents.is_empty() {
            visit(
                self.contents.as_ptr() as usize,
                size_of_val(&*self.contents),
            );
        }
        if !self.backings.is_empty() {
            visit(
                self.backings.as_ptr() as usize,
                size_of_val(&*self.backings),
            );
        }
        for backing in &self.backings {
            match backing {
                Backing::Memory(bytes) => bytes.visit_retained_allocations(&mut visit),
                #[cfg(unix)]
                Backing::Mapped(file) => {
                    visit(
                        file.path.as_os_str().as_encoded_bytes().as_ptr() as usize,
                        file.path.as_os_str().len(),
                    );
                    visit(
                        Arc::as_ptr(&file.file) as usize,
                        size_of::<File>() + 2 * size_of::<usize>(),
                    );
                }
            }
        }
    }
}
fn retain_backing(backings: &mut Vec<Backing>, backing: Backing) -> io::Result<u32> {
    let index = u32::try_from(backings.len())
        .map_err(|_| io::Error::other("installation handle table exceeds addressable memory"))?;
    backings.push(backing);
    Ok(index)
}
#[derive(Clone, Copy)]
enum Expected {
    Content(Reference),
    Row(Address, Association),
}
impl Expected {
    fn check(
        self,
        bytes: Bytes,
        contents: &mut Vec<(Reference, Location)>,
        backings: &mut Vec<Backing>,
        backing: &mut Option<u32>,
        #[cfg(unix)] file: &mut Option<PinnedFile>,
        #[cfg(not(unix))] _file: &mut Option<()>,
    ) -> io::Result<()> {
        match self {
            Self::Content(reference) => {
                let bytes = checked_container(bytes, reference).ok_or_else(invalid)?;
                #[cfg(unix)]
                {
                    let start = u32::try_from(bytes.range.start).map_err(|_| invalid())?;
                    let mapped = if let Some(index) = *backing {
                        Some(index)
                    } else if let Some(file) = file.take() {
                        let index = retain_backing(backings, Backing::Mapped(file))?;
                        *backing = Some(index);
                        Some(index)
                    } else {
                        None
                    };
                    if let Some(index) = mapped {
                        contents.push((
                            reference,
                            Location {
                                backing: index,
                                start,
                            },
                        ));
                        return Ok(());
                    }
                }
                #[cfg(not(unix))]
                let _ = backing;
                let bytes = if bytes.has_reference(reference) {
                    bytes
                } else {
                    bytes.with_reference(reference)
                };
                let index = retain_backing(backings, Backing::Memory(bytes))?;
                contents.push((
                    reference,
                    Location {
                        backing: index,
                        start: 0,
                    },
                ));
            }
            Self::Row(address, expected) => {
                if checked_association(&bytes, address) != Some(expected) {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
}
impl Store {
    /// Check all installation associations and unique complete references.
    /// Read one pack at a time in increasing offset order, retaining checked
    /// payload handles for consumption within this operation. Missing rows,
    /// outputs and malformed references all refuse the complete installation.
    pub fn verify_records(
        &self,
        rows: impl IntoIterator<Item = (Address, Association)>,
        references: impl IntoIterator<Item = Reference>,
    ) -> io::Result<VerifiedRecords> {
        let _scope = self.batch();
        let mut unique: BTreeSet<_> = references.into_iter().collect();
        let mut files: BTreeMap<(PathBuf, bool), Vec<(u64, u64, Expected)>> = BTreeMap::new();
        let mut contents = Vec::new();
        let mut backings = Vec::new();
        let mut route = |source, expected: Expected, packed| -> io::Result<()> {
            match source {
                pack::Source::Pending(bytes) => {
                    expected.check(bytes, &mut contents, &mut backings, &mut None, &mut None)?
                }
                pack::Source::File { path, offset, len } => {
                    files.entry((path, packed)).or_default().push((
                        u64::from(offset),
                        u64::from(len),
                        expected,
                    ));
                }
            }
            Ok(())
        };
        for (address, association) in rows {
            unique.insert(association.reference);
            route(
                self.packed_source(1, address.0).ok_or_else(invalid)?,
                Expected::Row(address, association),
                true,
            )?;
        }
        for reference in unique {
            let (source, packed) = if let Some(source) = self.packed_source(0, reference.output.0) {
                (source, true)
            } else if reference.len > pack::SMALL as u64 {
                (
                    pack::Source::File {
                        path: self.output_path(reference),
                        offset: 0,
                        len: u32::try_from(
                            reference
                                .len
                                .checked_add(HEADER as u64)
                                .ok_or_else(invalid)?,
                        )
                        .map_err(|_| invalid())?,
                    },
                    false,
                )
            } else {
                return Err(invalid());
            };
            route(source, Expected::Content(reference), packed)?;
        }
        for ((path, packed), mut records) in files {
            records.sort_unstable_by_key(|(offset, _, _)| *offset);
            let bytes = if packed {
                self.open_segment(&path, true)?
            } else {
                let region = self.open_verification_region(&path, Counter::StandaloneOpens)?;
                let len = region.bytes().len();
                Bytes {
                    region,
                    range: 0..len,
                }
            };
            #[cfg(unix)]
            let mut file = bytes.region.file().map(|file| PinnedFile {
                path,
                file: file.clone(),
                len: bytes.len(),
            });
            #[cfg(not(unix))]
            let mut file = None;
            let mut backing = None;
            for (offset, len, expected) in records {
                if !packed && len != bytes.len() as u64 {
                    return Err(invalid());
                }
                expected.check(
                    bytes
                        .range(offset, len, if packed { 8 } else { 1 })
                        .ok_or_else(invalid)?,
                    &mut contents,
                    &mut backings,
                    &mut backing,
                    &mut file,
                )?;
            }
        }
        contents.sort_unstable_by_key(|(reference, _)| *reference);
        if u32::try_from(contents.len()).is_err() {
            return Err(io::Error::other(
                "installation reference table exceeds addressable memory",
            ));
        }
        let scope = self
            .regions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .scope
            .get_or_insert_with(|| Arc::new(()))
            .clone();
        Ok(VerifiedRecords {
            store: self.clone(),
            scope,
            contents: contents.into_boxed_slice(),
            backings: backings.into_boxed_slice(),
        })
    }
}
