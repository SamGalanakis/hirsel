//! Read PROG-9 metadata before relocation, while all offsets still name file bytes.
use super::{Malformed, Module};
use crate::{DESCRIPTOR, Entry, Kind, NO_DESCRIPTOR};

/// The source identity and optional release revision of a native export.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Descriptor<'a> {
    pub identity: &'a [u8],
    pub revision: Option<&'a [u8; 32]>,
}

impl<'a> Module<'a> {
    /// Read the descriptor from the module file without following native pointers.
    pub fn descriptor(&self, entry: &Entry) -> Result<Option<Descriptor<'a>>, Malformed> {
        if entry.descriptor == NO_DESCRIPTOR {
            return Ok(None);
        }
        let at = entry.descriptor as usize;
        if !self.literals().contains(&at) || !at.is_multiple_of(8) {
            return Err(Malformed::Entries);
        }
        let word = |offset: usize| -> Result<usize, Malformed> {
            let bytes = self.fixed_data(offset, 8)?.first_chunk().unwrap();
            usize::try_from(u64::from_le_bytes(*bytes)).map_err(|_| Malformed::Entries)
        };
        let start = at.checked_add(DESCRIPTOR).ok_or(Malformed::Entries)?;
        let len = word(at + 8)?;
        let identity = self.fixed_data(start, len)?;
        let pointer = |offset: usize| -> Result<Option<usize>, Malformed> {
            let Some(reloc) = self.data_fixup_at(offset) else {
                return if word(offset)? == 0 {
                    Ok(None)
                } else {
                    Err(Malformed::Entries)
                };
            };
            let target = usize::try_from(reloc.addend).map_err(|_| Malformed::Entries)?;
            if reloc.kind != Kind::Abs64Data || !self.literals().contains(&target) {
                return Err(Malformed::Entries);
            }
            Ok(Some(target))
        };
        let revision = match pointer(at + 16)? {
            None => None,
            Some(carrier) => match word(carrier.checked_add(8).ok_or(Malformed::Entries)?)? {
                0 => None,
                32 => {
                    let data = pointer(carrier)?.ok_or(Malformed::Entries)?;
                    Some(self.fixed_data(data, 32)?.first_chunk().unwrap())
                }
                _ => return Err(Malformed::Entries),
            },
        };
        Ok(Some(Descriptor { identity, revision }))
    }

    /// Metadata read before installation must have the same bytes after relocation.
    fn fixed_data(&self, start: usize, len: usize) -> Result<&'a [u8], Malformed> {
        let end = start.checked_add(len).ok_or(Malformed::Entries)?;
        let data = self.data().get(start..end).ok_or(Malformed::Entries)?;
        if !data.is_empty() {
            let fixups = self.sections[4].as_chunks::<{ crate::FIXUP }>().0;
            // Every data fixup occupies exactly eight bytes.
            let next =
                fixups.partition_point(|r| u64::from(super::u32_at(r, 0)) + 8 <= start as u64);
            if fixups
                .get(next)
                .is_some_and(|r| (super::u32_at(r, 0) as usize) < end)
            {
                return Err(Malformed::Entries);
            }
        }
        Ok(data)
    }

    fn data_fixup_at(&self, at: usize) -> Option<crate::Fixup> {
        let table = self.sections[4].as_chunks::<{ crate::FIXUP }>().0;
        let i = table
            .binary_search_by_key(&u32::try_from(at).ok()?, |r| super::u32_at(r, 0))
            .ok()?;
        Some(super::fixup(&table[i]))
    }
}
