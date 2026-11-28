//! Match the identity and revision observed during Runner registration to the native
//! entry of this installation. All offsets refer to the image's read-only data band.
use super::{ContentHash, EntryIndex, ImageEntries, Installed, Refusal, Signature};
use hirsel_mod::Module;

pub(super) struct Description {
    entry: usize,
    start: usize,
    len: usize,
    revision: Option<ContentHash>,
}

pub(super) fn directory(module: &Module<'_>) -> Result<Box<[Description]>, Refusal> {
    let mut rows = Vec::new();
    for (entry, native) in module.entries().enumerate() {
        let Some(descriptor) = module.descriptor(&native).map_err(Refusal::Malformed)? else {
            continue;
        };
        let start = native.descriptor as usize + hirsel_mod::DESCRIPTOR;
        rows.push(Description {
            entry,
            start,
            len: descriptor.identity.len(),
            revision: descriptor.revision.copied().map(ContentHash),
        });
    }
    let source = |row: &Description| &module.data()[row.start..row.start + row.len];
    rows.sort_unstable_by(|a, b| source(a).cmp(source(b)));
    Ok(rows.into_boxed_slice())
}

impl Installed {
    /// Resolve the PROG-9 metadata of a registered function to a typed candidate.
    /// A missing or ambiguous match, a different revision, or another callable
    /// signature refuses binding.
    pub fn resolve_described<F: Signature>(
        &self,
        identity: &[u8],
        revision: Option<ContentHash>,
    ) -> Result<EntryIndex<F>, Refusal> {
        let ImageEntries::Native {
            entries,
            descriptions,
        } = &self.0.entries
        else {
            return Err(Refusal::NoEntry);
        };
        let bytes = |row: &Description| {
            // SAFETY: the checked descriptor lies within immutable image data. The
            // borrowed installation owns the mapping for this lookup.
            unsafe { std::slice::from_raw_parts((self.0.data + row.start) as *const u8, row.len) }
        };
        let first = descriptions.partition_point(|row| bytes(row) < identity);
        let mut matching = descriptions[first..]
            .iter()
            .take_while(|row| bytes(row) == identity)
            .filter(|row| {
                let entry = &entries[row.entry];
                row.revision == revision && entry.signature == F::DIGEST && entry.waits == F::WAITS
            });
        let index = matching.next().ok_or(Refusal::NoEntry)?.entry;
        if matching.next().is_some() {
            return Err(Refusal::NoEntry);
        }
        Ok(EntryIndex::new(&self.0, index))
    }
}
