use std::{ptr::NonNull, sync::Arc};
use wasmtime::{CustomCodeMemory, Engine, Module, Result};

/// An engine that loads precompiled code from the executable image.
pub struct ImageEngine(Engine);

impl ImageEngine {
    /// Create a runtime engine after the host has installed its signal handlers.
    pub fn new() -> Result<Self> {
        let mut config = super::engine_config();
        config.with_custom_code_memory(Some(Arc::new(ImageCode)));
        Engine::new(&config).map(Self)
    }

    /// The engine that stores and linkers of loaded modules use.
    pub fn engine(&self) -> &Engine {
        &self.0
    }

    /// Load an artifact emitted into the executable segment of this image.
    ///
    /// # Safety
    ///
    /// The bytes must be the unmodified output of the pinned precompiler and
    /// must reside at a 64 KiB boundary in the image's read-execute segment.
    pub unsafe fn load(&self, artifact: &'static [u8]) -> Result<Module> {
        // SAFETY: The caller provides the trusted artifact and its lifetime and
        // memory protections. ImageCode requires no writes or protection changes.
        unsafe { Module::deserialize_raw(&self.0, NonNull::from(artifact)) }
    }
}

struct ImageCode;

impl CustomCodeMemory for ImageCode {
    fn required_alignment(&self) -> usize {
        match (std::env::consts::ARCH, std::env::consts::OS) {
            ("x86_64", "linux") => 4 << 10,
            ("aarch64", "linux") => 64 << 10,
            ("aarch64", "macos") => 16 << 10,
            _ => unreachable!("unsupported contained runtime target"),
        }
    }

    fn publish_executable(&self, address: *const u8, len: usize) -> Result<()> {
        #[cfg(target_arch = "aarch64")]
        {
            use wasmtime_internal_jit_icache_coherence::{clear_cache, pipeline_flush_mt};

            // SAFETY: The engine passes a contiguous live range of image code.
            unsafe { clear_cache(address.cast(), len)? };
            pipeline_flush_mt()?;
        }
        #[cfg(not(target_arch = "aarch64"))]
        let _ = (address, len);
        Ok(())
    }

    fn unpublish_executable(&self, _: *const u8, _: usize) -> Result<()> {
        Ok(())
    }
}
