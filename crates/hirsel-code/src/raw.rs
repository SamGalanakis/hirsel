//! Untyped PACK-14 loader & handle bridge used by `hirsel-pack` / `hirsel-core-rt`
//! to implement `Code[F]`, `CodeSlot[F]`, `code.load`, `code.bind`, `code.share`,
//! `code.activate`, `code.dispatch`, and Unit pinning without Rust monomorphization.

use super::{CodePin, Image, Loader, RawSlot, Refusal, TARGET};
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

impl Loader {
    /// Register the exact bytes of an admitted module digest so `code.load(loader, key, entry_id)`
    /// can resolve `key -> digest -> bytes` locally (PACK-14).
    pub fn register_bytes(&self, digest: ContentHash, bytes: Arc<[u8]>) {
        self.0
            .slab
            .lock()
            .unwrap()
            .admitted_bytes
            .insert(digest, bytes);
    }

    /// Parse `bytes`, admit `(header.key(), digest)` and register `bytes` in this
    /// loader's local binding table. Returns `(key, digest)`.
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

    /// Resolve `key` through this loader's local PACK-1 binding, obtain its exact
    /// bytes by digest, validate header, policy, quarantine, and the requested
    /// entry `(identity, signature, waits)` BEFORE publishing any image, and return
    /// a counted [`RawCode`] handle.
    pub fn load_by_key(
        &self,
        key: &ContentHash,
        identity: &ContentHash,
        signature: &ContentHash,
        waits: bool,
        flush: bool,
    ) -> Result<RawCode, Refusal> {
        if self.0.config.band_bytes == 0 {
            return self.prelinked_entry(key, identity, signature, waits);
        }
        let bytes = {
            let mut slab = self.0.slab.lock().unwrap();
            self.0.reclaim(&mut slab)?;
            let digest = *slab.admitted.get(key).ok_or(Refusal::NotAdmitted)?;
            if slab.quarantined.contains(&digest) {
                return Err(Refusal::Quarantined);
            }
            slab.admitted_bytes
                .get(&digest)
                .cloned()
                .ok_or(Refusal::NotAdmitted)?
        };
        let actual = hirsel_mod::digest(&bytes);
        {
            let slab = self.0.slab.lock().unwrap();
            if slab.admitted.get(key) != Some(&actual) {
                return Err(Refusal::NotAdmitted);
            }
        }
        self.load_entry_raw(&bytes, identity, signature, waits, flush)
    }

    /// Validate `bytes`, header, policy, admission, quarantine, and the requested
    /// entry `(identity, signature, waits)` BEFORE publishing any new image (PACK-14).
    /// Refusal leaves installed bytes, cohorts, and image counts strictly unchanged.
    pub fn load_entry_raw(
        &self,
        bytes: &[u8],
        identity: &ContentHash,
        signature: &ContentHash,
        waits: bool,
        flush: bool,
    ) -> Result<RawCode, Refusal> {
        let shared = &*self.0;
        let config = &shared.config;
        let module = Module::parse(bytes).map_err(Refusal::Malformed)?;
        let header = &module.header;
        if header.target != TARGET {
            return Err(Refusal::Mismatch("target"));
        }
        if header.abi != config.abi {
            return Err(Refusal::Mismatch("abi"));
        }
        if header.policy != config.policy {
            return Err(Refusal::Mismatch("policy"));
        }
        if config.band_bytes == 0 {
            return self.prelinked_entry(&header.key(), identity, signature, waits);
        }
        let digest = hirsel_mod::digest(bytes);
        let image;
        let published;
        {
            let mut slab = shared.slab.lock().unwrap();
            shared.reclaim(&mut slab)?;
            if slab.admitted.get(&header.key()) != Some(&digest) {
                return Err(Refusal::NotAdmitted);
            }
            if slab.quarantined.contains(&digest) {
                return Err(Refusal::Quarantined);
            }
            // PACK-14: validate requested entry identity, typed signature, and
            // `waits` flag BEFORE installing or publishing any new image.
            let has_entry = module
                .entries()
                .any(|e| e.identity == *identity && e.signature == *signature && e.waits == waits);
            if !has_entry {
                return Err(Refusal::NoEntry);
            }
            let held = slab.images.get(&digest).and_then(|held| held.1.upgrade());
            let (leads, held) = match held {
                Some(held) => (false, held),
                None => self.install(&mut slab, &module, digest)?,
            };
            image = held;
            (slab, published) = shared.settle(slab, &image, leads, flush);
            drop(slab);
        }
        if !published {
            return Err(Refusal::Unsupported);
        }
        let found = image
            .entries
            .iter()
            .find(|e| e.identity == *identity && e.signature == *signature && e.waits == waits)
            .ok_or(Refusal::NoEntry)?;
        Ok(RawCode {
            image: Some(image.clone()),
            entry: found.entry,
            descriptor: found.descriptor,
        })
    }

    /// Prelinked lookup that validates the requested entry before creating/interning
    /// any image handle.
    fn prelinked_entry(
        &self,
        key: &ContentHash,
        identity: &ContentHash,
        signature: &ContentHash,
        waits: bool,
    ) -> Result<RawCode, Refusal> {
        let config = &self.0.config;
        let held = config
            .prelinked
            .iter()
            .find(|held| held.key == *key)
            .ok_or(Refusal::NotPrelinked)?;
        if waits {
            return Err(Refusal::NoEntry);
        }
        let has_entry = held
            .entries
            .iter()
            .any(|(id, sig, _)| id == identity && sig == signature);
        if !has_entry {
            return Err(Refusal::NoEntry);
        }
        let installed = self.prelinked(key)?;
        let found = installed
            .0
            .entries
            .iter()
            .find(|e| e.identity == *identity && e.signature == *signature && e.waits == waits)
            .ok_or(Refusal::NoEntry)?;
        Ok(RawCode {
            image: Some(installed.0.clone()),
            entry: found.entry,
            descriptor: found.descriptor,
        })
    }

    /// Bind an entry defined inside a live loaded image in this pool, retaining
    /// an owning pin on that image so the handle can never outlive it (PACK-14).
    pub fn bind_loaded(&self, entry_ptr: usize, descriptor: usize) -> Option<RawCode> {
        let hit = self.resolve(entry_ptr)?;
        let slab = self.0.slab.lock().unwrap();
        let image = slab
            .images
            .iter()
            .find(|(digest, _)| digest.0[..16] == hit.digest)
            .and_then(|(_, (_, weak))| weak.upgrade())?;
        Some(RawCode {
            image: Some(image),
            entry: entry_ptr,
            descriptor,
        })
    }

    /// Acquire an owning [`CodePin`] for the live image whose text contains `pc`.
    /// Used when a Unit spawns onto loaded code so the Unit owns its generation
    /// through end publication (PACK-14).
    pub fn pin_pc(&self, pc: usize) -> Option<CodePin> {
        let hit = self.resolve(pc)?;
        let slab = self.0.slab.lock().unwrap();
        let image = slab
            .images
            .iter()
            .find(|(digest, _)| digest.0[..16] == hit.digest)
            .and_then(|(_, (_, weak))| weak.upgrade())?;
        Some(CodePin(Some(image)))
    }

    /// Create an empty untyped version-0 slot belonging to this loader's pool.
    pub fn raw_slot(&self) -> RawSlot {
        RawSlot::new(self.0.clone())
    }
}
