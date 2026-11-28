//! Untyped PACK-14 loader & handle bridge used by `hirsel-pack` / `hirsel-core-rt`
//! to implement `Code[F]`, `CodeSlot[F]`, `code.load`, `code.bind`, `code.share`,
//! `code.activate`, `code.dispatch`, and Unit pinning without Rust monomorphization.

use super::{CodePin, Image, Loader, RawSlot, Refusal, Want};
use hirsel_core_ids::ContentHash;
use hirsel_mod::Module;
use std::sync::Arc;

/// An untyped counted handle to one entry of one image, or to a statically linked
/// host entry (`image: None`, process-lifetime backing).
#[derive(Clone)]
pub struct RawCode {
    pub(super) image: Option<Arc<Image>>,
    pub(super) entry: usize,
    pub(super) descriptor: usize,
}

impl RawCode {
    /// Wrap a statically linked host entry with process-lifetime backing.
    pub const fn static_entry(entry: usize, descriptor: usize) -> Self {
        Self {
            image: None,
            entry,
            descriptor,
        }
    }

    /// Entry code pointer (PACK-15 typed export adapter).
    pub const fn entry(&self) -> usize {
        self.entry
    }

    /// PROG-9 static descriptor address in the image's data band (0 if none).
    pub const fn descriptor(&self) -> usize {
        self.descriptor
    }

    /// The owning image's module digest, if dynamically loaded or prelinked.
    pub fn digest(&self) -> Option<ContentHash> {
        self.image.as_ref().map(|img| img.digest)
    }

    /// The owning image's independent R/NX cause data owner pointer, if established.
    pub fn cause_owner(&self) -> Option<usize> {
        self.image
            .as_ref()
            .and_then(|img| img.cause_data.map(|h| h.owner))
    }

    /// Convert to an owning [`CodePin`].
    pub fn pin(&self) -> CodePin {
        CodePin(self.image.clone())
    }
}

impl Want<'_> {
    /// The handle to the entry this names in `image`.
    fn code(&self, image: Arc<Image>) -> Result<RawCode, Refusal> {
        let index = image.entry_index(self).ok_or(Refusal::NoEntry)?;
        let (entry, descriptor) = image.addresses(index);
        Ok(RawCode {
            image: Some(image),
            entry,
            descriptor,
        })
    }
}

impl Loader {
    /// Parse `bytes`, admit `(header.key(), digest)` and keep `bytes` so a load by key
    /// finds them (PACK-14). The digest is taken here, once, from the bytes kept.
    /// Returns `(key, digest)`.
    pub fn admit_module(&self, bytes: &[u8]) -> Result<(ContentHash, ContentHash), Refusal> {
        let module = Module::parse(bytes).map_err(Refusal::Malformed)?;
        let key = module.header.key();
        let digest = hirsel_mod::digest(bytes);
        let owned: Arc<[u8]> = Arc::from(bytes);
        let mut slab = self.0.slab.lock().unwrap();
        slab.admitted.insert(key, digest);
        slab.admitted_bytes.insert(digest, owned);
        Ok((key, digest))
    }

    /// How many distinct live images are currently tracked in this loader's slab.
    pub fn live_images(&self) -> usize {
        let _ = self.reclaim();
        self.0
            .slab
            .lock()
            .unwrap()
            .images
            .values()
            .filter(|(_, w)| w.strong_count() > 0)
            .count()
    }

    /// The entry `(identity, signature, waits)` of the module this loader admits for
    /// `key`, whose bytes [`Self::admit_module`] kept (PACK-14). A live image of the
    /// module answers without its bytes being read; every refusal comes before
    /// anything is installed.
    pub fn load_by_key(
        &self,
        key: &ContentHash,
        identity: &ContentHash,
        signature: &ContentHash,
        waits: bool,
        flush: bool,
    ) -> Result<RawCode, Refusal> {
        let shared = &*self.0;
        let want = Want {
            identity,
            signature,
            waits,
        };
        if shared.config.band_bytes == 0 {
            return want.code(self.prelinked_image(key, Some(&want))?);
        }
        let mut slab = shared.slab.lock().unwrap();
        let _ = shared.reclaim(&mut slab);
        let digest = *slab.admitted.get(key).ok_or(Refusal::NotAdmitted)?;
        if slab.quarantined.contains(&digest) {
            return Err(Refusal::Quarantined);
        }
        let live = slab.images.get(&digest).and_then(|held| held.1.upgrade());
        if let Some(image) = live {
            let code = want.code(image.clone())?;
            shared.settled(slab, image, flush)?;
            return Ok(code);
        }
        let bytes = slab.admitted_bytes.get(&digest).cloned();
        drop(slab);
        let bytes = bytes.ok_or(Refusal::NotAdmitted)?;
        want.code(self.load_image(&bytes, digest, Some(&want), flush)?)
    }

    /// The entry `(identity, signature, waits)` of the module `bytes` (PACK-14). Every
    /// refusal comes before anything is installed.
    pub fn load_entry_raw(
        &self,
        bytes: &[u8],
        identity: &ContentHash,
        signature: &ContentHash,
        waits: bool,
        flush: bool,
    ) -> Result<RawCode, Refusal> {
        let want = Want {
            identity,
            signature,
            waits,
        };
        let digest = hirsel_mod::digest(bytes);
        want.code(self.load_image(bytes, digest, Some(&want), flush)?)
    }

    /// The live image whose text holds `pc`, counted. The PC table row names it, so
    /// this takes no lock and searches nothing else.
    fn image_at(&self, pc: usize) -> Option<Arc<Image>> {
        self.0.table.find(pc, 1, |row, _| row.image.upgrade())?
    }

    /// Bind an entry defined inside a live loaded image in this pool, retaining
    /// an owning pin on that image so the handle can never outlive it (PACK-14).
    pub fn bind_loaded(&self, entry_ptr: usize, descriptor: usize) -> Option<RawCode> {
        Some(RawCode {
            image: Some(self.image_at(entry_ptr)?),
            entry: entry_ptr,
            descriptor,
        })
    }

    /// Acquire an owning [`CodePin`] for the live image whose text contains `pc`.
    /// Used when a Unit spawns onto loaded code so the Unit owns its generation
    /// through end publication (PACK-14).
    pub fn pin_pc(&self, pc: usize) -> Option<CodePin> {
        Some(CodePin(Some(self.image_at(pc)?)))
    }

    /// Create an empty untyped version-0 slot belonging to this loader's pool.
    pub fn raw_slot(&self) -> RawSlot {
        RawSlot::new(self.0.clone())
    }
}
