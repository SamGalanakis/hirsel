//! Copying one module into the open cohort (PACK-4 steps 2 to 4.1).

use super::retire::Retire;
use super::table::{DataRow, Owner};
use super::{
    Cohort, ContentHash, Image, ImageEntries, Loader, Refusal, Shared, Slab, State, TARGET, WINDOW,
    Window, fixup, host,
};
use hirsel_mod::{Import, Module};
use host::Access;
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::time::Instant;

/// The room `place` gave one install. Dropped before the install keeps it, it takes
/// the room back, and a cohort the install opened never existed.
struct Room<'a> {
    slab: &'a mut Slab,
    /// Where the window's free space started before this install.
    before: Option<(usize, usize)>,
    cohort: u64,
    /// Whether this install opened the cohort.
    leads: bool,
    kept: bool,
}

impl Drop for Room<'_> {
    fn drop(&mut self) {
        if self.kept {
            return;
        }
        let start = if self.leads {
            self.slab.open = None;
            let mine = self.slab.cohorts.remove(&self.cohort);
            mine.map(|mine| (mine.text.0, mine.data.0))
        } else {
            self.before
        };
        if let (Some(window), Some(start)) = (&mut self.slab.window, start) {
            (window.text.0, window.data.0) = start;
        }
    }
}

impl Shared {
    /// Room for an image in the open cohort, opening a window or a cohort as needed:
    /// its text and data offsets and its cohort.
    fn place(
        &self,
        slab: &mut Slab,
        text: (usize, usize),
        data: (usize, usize),
    ) -> Option<(usize, usize, u64)> {
        for _ in 0..2 {
            if let Some((free, ends)) = slab
                .window
                .as_ref()
                .map(|w| ((w.text.0, w.data.0), (w.text.1, w.data.1)))
            {
                let at = (
                    free.0.next_multiple_of(text.1),
                    free.1.next_multiple_of(data.1),
                );
                if at.0 + text.0 <= ends.0 && at.1 + data.0 <= ends.1 {
                    let id = match slab.open {
                        Some(id) => id,
                        None => {
                            let id = slab.next;
                            slab.next += 1;
                            slab.open = Some(id);
                            slab.cohorts.insert(
                                id,
                                Cohort {
                                    text: (free.0, free.0),
                                    data: (free.1, free.1),
                                    state: State::Open,
                                    first: Instant::now(),
                                    live: 0,
                                    bytes: 0,
                                    retired: 0,
                                    images: Vec::new(),
                                    data_owners: Vec::new(),
                                    unsynchronized: Vec::new(),
                                },
                            );
                            id
                        }
                    };
                    self.trap(free.0, at.0);
                    let window = slab.window.as_mut().unwrap();
                    (window.text.0, window.data.0) = (at.0 + text.0, at.1 + data.0);
                    return Some((at.0, at.1, id));
                }
                self.close(slab);
            }
            let (code, words) = (self.pages(text.0), self.pages(data.0));
            let code = slab.text.take(code, code.max(WINDOW))?;
            let words = slab.data.take(words, words.max(WINDOW));
            let words = words.filter(|words| {
                // A refused run is still closed, so it goes back with the text run.
                let open = host::protect(self.data(words.0), words.1, Access::Write).is_ok();
                if !open {
                    slab.data.give(words.0, words.1);
                }
                open
            });
            let Some(words) = words else {
                slab.text.give(code.0, code.1);
                return None;
            };
            // A page the band has never held reads as zero, which this target runs.
            #[cfg(target_arch = "x86_64")]
            self.trap(code.0, code.0 + code.1);
            slab.window = Some(Window {
                text: (code.0, code.0 + code.1),
                data: (words.0, words.0 + words.1),
            });
        }
        None
    }
}

impl Loader {
    /// Copy a module into the open cohort. Nothing of the image is reachable until its
    /// cohort is published, and a refusal leaves the slab as it was.
    pub(super) fn install(
        &self,
        slab: &mut Slab,
        module: &Module<'_>,
        digest: ContentHash,
    ) -> Result<Arc<Image>, Refusal> {
        let shared = &*self.0;
        let config = &shared.config;
        let cause_owner_row = hirsel_mod::row("hirsel_core_cause_owner");
        let mut cause_owner_slots = Vec::new();
        let mut slots = Vec::with_capacity(module.imports().len());
        for (slot_index, import) in module.imports().enumerate() {
            slots.push(match import {
                Import::Row(row) => {
                    if cause_owner_row == Some(row) {
                        cause_owner_slots.push(slot_index);
                    }
                    hirsel_mod::ROWS
                        .get(row as usize)
                        .filter(|(_, class)| config.mask.admits(*class))
                        .and_then(|_| (config.row)(row))
                        .ok_or(Refusal::Import(row))?
                }
                Import::Instance(instance) => {
                    (config.instance)(&instance).ok_or(Refusal::Unbound(instance))?
                }
            });
        }
        let descriptions = super::descriptors::directory(module)?;
        let aligns = (
            module.header.text_align as usize,
            module.header.data_align as usize,
        );
        if !aligns.0.is_power_of_two()
            || !aligns.1.is_power_of_two()
            || aligns.0.max(aligns.1) > shared.page
        {
            return Err(Refusal::Unsupported);
        }
        let (mut text, mut data) = (module.text().to_vec(), module.data().to_vec());
        let bytes = (text.len() + data.len()) as u64;
        if slab.installed + bytes > config.installed_bytes {
            return Err(Refusal::Capacity);
        }
        let logical = text.len();
        #[cfg(target_arch = "aarch64")]
        let owned = logical.next_multiple_of(4);
        #[cfg(target_arch = "x86_64")]
        let owned = logical;
        let before = slab
            .window
            .as_ref()
            .map(|window| (window.text.0, window.data.0));
        let opened = slab.open;
        let (at, words, cohort) = shared
            .place(slab, (owned, aligns.0), (data.len(), aligns.1))
            .ok_or(Refusal::Capacity)?;
        let mut room = Room {
            before,
            cohort,
            leads: slab.open != opened,
            kept: false,
            slab,
        };
        let (t, d) = (shared.text(at), shared.data(words));
        for (slot, address) in data.as_chunks_mut().0.iter_mut().zip(&slots) {
            *slot = (*address as u64).to_le_bytes();
        }
        fixup::apply(
            TARGET.arch(),
            (&mut text, t as u64),
            (&mut data, d as u64),
            module.text_fixups(),
            module.data_fixups(),
        )
        .map_err(Refusal::Fixup)?;
        // The data owner exists before any entry is published (PACK-9.7): an image
        // whose sites or literals a cause can outlive never installs without one.
        let literals = module.literals();
        let cause_data = match config.cause_data {
            Some(ops) if !module.sites().is_empty() || !literals.is_empty() => Some(
                (ops.create)(&data[literals.clone()], module.sites()).ok_or(Refusal::Capacity)?,
            ),
            _ => None,
        };
        if let Some(handle) = cause_data {
            for slot_index in cause_owner_slots {
                let at = 8 * slot_index;
                data[at..at + 8].copy_from_slice(&(handle.slot_addr as u64).to_le_bytes());
            }
        }
        text.resize(owned, 0);
        // SAFETY: `place` gave this image these writable ranges and nothing else holds them.
        host::write(&mut || unsafe {
            std::ptr::copy_nonoverlapping(
                text.as_ptr(),
                (shared.writer + at) as *mut u8,
                text.len(),
            );
            std::ptr::copy_nonoverlapping(data.as_ptr(), d as *mut u8, data.len());
        });
        let data_range = match cause_data {
            Some(handle) if handle.ro_literals_len != 0 => {
                (words + literals.start, handle.ro_literals_len)
            }
            _ => (0, 0),
        };
        let serial = room.slab.installs;
        let image = Arc::new(Image {
            shared: self.0.clone(),
            digest,
            cohort: Some(cohort),
            text: t,
            data: d,
            bytes,
            cause_data,
            entries: ImageEntries::Native {
                entries: module.entries().collect(),
                descriptions,
            },
            slots: AtomicU32::new(0),
            retired: AtomicBool::new(false),
            retire: Some(Box::new(Retire {
                next: null_mut(),
                serial,
                digest,
                cohort: Some(cohort),
                text: (at, logical),
                owned: text.len(),
                data: data_range,
                bytes,
                retired: false,
                cause_data,
            })),
        });
        let record = room
            .slab
            .cohorts
            .get_mut(&cohort)
            .ok_or(Refusal::Capacity)?;
        if let (Some(handle), true) = (cause_data, data_range.1 != 0) {
            record.data_owners.push(DataRow {
                start: data_range.0 as u32,
                length: data_range.1 as u32,
                owner: handle.owner,
                ro_base: handle.ro_literals_base,
            });
        }
        let row = Owner {
            start: at as u32,
            length: logical as u32,
            digest: digest.0[..16].try_into().unwrap(),
            image: Arc::downgrade(&image),
        };
        record.images.push((row, digest));
        record.live += 1;
        record.bytes += bytes;
        let slab = &mut *room.slab;
        slab.images.insert(digest, (serial, Arc::downgrade(&image)));
        slab.installs += 1;
        slab.installed += bytes;
        room.kept = true;
        Ok(image)
    }
}
