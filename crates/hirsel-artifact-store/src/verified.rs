//! Complete installation checks in physical record order. Git
//! 7e34d8174dcda3435d1404c9a30bef46afe6c13e, packfile.c:1966-2007:
//! traverse one pack in physical order. Here selected offsets are sorted from
//! our portable index, and each complete reference is checked independently.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// Operation-owned integrity handles. These supply no action or V authority.
/// Every new operation constructs fresh handles from its selected store.
#[derive(Debug)]
pub struct VerifiedRecords {
    store: Store,
    scope: Arc<()>,
    contents: Box<[(Reference, VerifiedBytes)]>,
}
impl VerifiedRecords {
    pub fn get(&self, reference: Reference) -> Option<Bytes> {
        let index = self
            .contents
            .binary_search_by_key(&reference, |(r, _)| *r)
            .ok()?;
        match &self.contents[index].1 {
            VerifiedBytes::Memory(bytes) => Some(bytes.clone()),
            #[cfg(unix)]
            VerifiedBytes::Mapped { file, range } => {
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
                Some(
                    Bytes {
                        region,
                        range: range.clone(),
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
        for (_, bytes) in &self.contents {
            match bytes {
                VerifiedBytes::Memory(bytes) => bytes.visit_retained_allocations(&mut visit),
                #[cfg(unix)]
                VerifiedBytes::Mapped { file, .. } => {
                    visit(
                        Arc::as_ptr(file) as usize,
                        size_of::<PinnedFile>() + 2 * size_of::<usize>(),
                    );
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

#[derive(Debug)]
enum VerifiedBytes {
    Memory(Bytes),
    #[cfg(unix)]
    Mapped {
        file: Arc<PinnedFile>,
        range: Range<usize>,
    },
}
#[cfg(unix)]
#[derive(Debug)]
struct PinnedFile {
    path: PathBuf,
    file: Arc<File>,
    len: usize,
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
        contents: &mut Vec<(Reference, VerifiedBytes)>,
        #[cfg(unix)] file: Option<&Arc<PinnedFile>>,
        #[cfg(not(unix))] _file: Option<&()>,
    ) -> Option<()> {
        match self {
            Self::Content(reference) => {
                let bytes = checked_container(bytes, reference)?;
                #[cfg(unix)]
                if let Some(file) = file {
                    contents.push((
                        reference,
                        VerifiedBytes::Mapped {
                            file: file.clone(),
                            range: bytes.range.clone(),
                        },
                    ));
                    return Some(());
                }
                let bytes = if bytes.has_reference(reference) {
                    bytes
                } else {
                    bytes.with_reference(reference)
                };
                contents.push((reference, VerifiedBytes::Memory(bytes)));
            }
            Self::Row(address, expected) => {
                if checked_association(&bytes, address)? != expected {
                    return None;
                }
            }
        }
        Some(())
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
        let (files, mut contents) = (|| {
            let mut unique: BTreeSet<_> = references.into_iter().collect();
            let mut files: BTreeMap<(PathBuf, bool), Vec<(u64, u64, Expected)>> = BTreeMap::new();
            let mut contents = Vec::new();
            let mut route = |source, expected: Expected, packed| -> Option<()> {
                match source {
                    pack::Source::Pending(bytes) => expected.check(bytes, &mut contents, None)?,
                    pack::Source::File { path, offset, len } => {
                        files.entry((path, packed)).or_default().push((
                            u64::from(offset),
                            u64::from(len),
                            expected,
                        ));
                    }
                }
                Some(())
            };
            for (address, association) in rows {
                unique.insert(association.reference);
                route(
                    self.packed_source(1, address.0)?,
                    Expected::Row(address, association),
                    true,
                )?;
            }
            for reference in unique {
                let (source, packed) = if let Some(source) =
                    self.packed_source(0, reference.output.0)
                {
                    (source, true)
                } else if reference.len > pack::SMALL as u64 {
                    (
                        pack::Source::File {
                            path: self.output_path(reference),
                            offset: 0,
                            len: u32::try_from(reference.len.checked_add(HEADER as u64)?).ok()?,
                        },
                        false,
                    )
                } else {
                    return None;
                };
                route(source, Expected::Content(reference), packed)?;
            }
            Some((files, contents))
        })()
        .ok_or_else(invalid)?;
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
            let file = bytes.region.file().map(|file| {
                Arc::new(PinnedFile {
                    path,
                    file: file.clone(),
                    len: bytes.len(),
                })
            });
            #[cfg(not(unix))]
            let file = None;
            for (offset, len, expected) in records {
                if !packed && len != bytes.len() as u64 {
                    return Err(invalid());
                }
                expected
                    .check(
                        bytes
                            .range(offset, len, if packed { 8 } else { 1 })
                            .ok_or_else(invalid)?,
                        &mut contents,
                        file.as_ref(),
                    )
                    .ok_or_else(invalid)?;
            }
        }
        contents.sort_unstable_by_key(|(reference, _)| *reference);
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
        })
    }
}
