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
    contents: Box<[(Reference, Bytes)]>,
}
impl VerifiedRecords {
    pub fn get(&self, reference: Reference) -> Option<Bytes> {
        let index = self
            .contents
            .binary_search_by_key(&reference, |(r, _)| *r)
            .ok()?;
        Some(self.contents[index].1.clone())
    }

    /// Include the handle table and every retained backing in the caller's
    /// existing shared-allocation charge. Repeated identities are shared.
    pub fn visit_retained_allocations(&self, mut visit: impl FnMut(usize, usize)) {
        if !self.contents.is_empty() {
            visit(
                self.contents.as_ptr() as usize,
                size_of_val(&*self.contents),
            );
        }
        for (_, bytes) in &self.contents {
            bytes.visit_retained_allocations(&mut visit);
        }
    }
}

#[derive(Clone, Copy)]
enum Expected {
    Content(Reference),
    Row(Address, Association),
}
impl Expected {
    fn check(self, bytes: Bytes, contents: &mut Vec<(Reference, Bytes)>) -> Option<()> {
        match self {
            Self::Content(reference) => {
                let bytes = checked_container(bytes, reference)?;
                let bytes = if bytes.has_reference(reference) {
                    bytes
                } else {
                    bytes.with_reference(reference)
                };
                contents.push((reference, bytes));
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
    ) -> Option<VerifiedRecords> {
        let _scope = self.batch();
        let mut unique: BTreeSet<_> = references.into_iter().collect();
        let mut files: BTreeMap<(PathBuf, bool), Vec<(u64, u64, Expected)>> = BTreeMap::new();
        let mut contents = Vec::new();
        let mut route = |source, expected: Expected, packed| -> Option<()> {
            match source {
                pack::Source::Pending(bytes) => expected.check(bytes, &mut contents)?,
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
            let source = if reference.len <= pack::SMALL as u64 {
                self.packed_source(0, reference.output.0)?
            } else {
                pack::Source::File {
                    path: self.output_path(reference),
                    offset: 0,
                    len: u32::try_from(reference.len.checked_add(HEADER as u64)?).ok()?,
                }
            };
            route(
                source,
                Expected::Content(reference),
                reference.len <= pack::SMALL as u64,
            )?;
        }
        for ((path, packed), mut records) in files {
            records.sort_unstable_by_key(|(offset, _, _)| *offset);
            let bytes = if packed {
                self.open_segment(&path)?
            } else {
                let region = self.open_region(&path, Counter::StandaloneOpens).ok()?;
                let len = region.bytes().len();
                Bytes {
                    region,
                    range: 0..len,
                }
            };
            for (offset, len, expected) in records {
                if !packed && len != bytes.len() as u64 {
                    return None;
                }
                expected.check(
                    bytes.range(offset, len, if packed { 8 } else { 1 })?,
                    &mut contents,
                )?;
            }
        }
        contents.sort_unstable_by_key(|(reference, _)| *reference);
        Some(VerifiedRecords {
            contents: contents.into_boxed_slice(),
        })
    }
}
