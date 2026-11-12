//! A neutral per-thread hardware counter shared by compiler drivers.

/// One pinned per-thread user-space retired-instruction counter.
pub struct InstructionCounter(i32);

#[cfg(target_os = "linux")]
mod sys {
    unsafe extern "C" {
        pub fn syscall(number: i64, ...) -> i64;
        pub fn read(fd: i32, buffer: *mut std::ffi::c_void, count: usize) -> isize;
        pub fn close(fd: i32) -> i32;
    }
    #[cfg(target_arch = "x86_64")]
    pub const PERF_EVENT_OPEN: i64 = 298;
    #[cfg(target_arch = "aarch64")]
    pub const PERF_EVENT_OPEN: i64 = 241;
}

impl InstructionCounter {
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub fn open() -> Result<InstructionCounter, String> {
        // struct perf_event_attr, PERF_ATTR_SIZE_VER8 (136 bytes).
        let mut attr = [0u8; 136];
        attr[0..4].copy_from_slice(&0u32.to_ne_bytes()); // PERF_TYPE_HARDWARE
        attr[4..8].copy_from_slice(&136u32.to_ne_bytes());
        attr[8..16].copy_from_slice(&1u64.to_ne_bytes()); // PERF_COUNT_HW_INSTRUCTIONS
        // read_format: TOTAL_TIME_ENABLED | TOTAL_TIME_RUNNING.
        attr[32..40].copy_from_slice(&3u64.to_ne_bytes());
        // pinned | exclude_kernel | exclude_hv.
        attr[40..48].copy_from_slice(&((1u64 << 2) | (1 << 5) | (1 << 6)).to_ne_bytes());
        // SAFETY: `attr` is a complete, initialized perf_event_attr of the
        // declared size; pid 0 and cpu -1 count this thread on any CPU.
        let fd = unsafe {
            sys::syscall(
                sys::PERF_EVENT_OPEN,
                attr.as_ptr(),
                0i32,
                -1i32,
                -1i32,
                8u64, // PERF_FLAG_FD_CLOEXEC
            )
        };
        if fd < 0 {
            return Err(format!(
                "perf_event_open instructions:u unavailable: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(InstructionCounter(fd as i32))
    }
    #[cfg(not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )))]
    pub fn open() -> Result<InstructionCounter, String> {
        Err("instruction counters need Linux perf events".into())
    }
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub fn read(&self) -> Result<u64, String> {
        let mut words = [0u64; 3];
        // SAFETY: the buffer is 24 writable bytes; `self.0` is our open fd.
        let n = unsafe { sys::read(self.0, words.as_mut_ptr().cast(), 24) };
        if n != 24 {
            return Err("pinned instruction counter lost its PMU slot".into());
        }
        let [value, enabled, running] = words;
        if enabled != running {
            return Err(format!(
                "instruction counter multiplexed (enabled {enabled} ns, running {running} ns)"
            ));
        }
        Ok(value)
    }
    #[cfg(not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )))]
    pub fn read(&self) -> Result<u64, String> {
        Err("instruction counters need Linux perf events".into())
    }
}

impl Drop for InstructionCounter {
    fn drop(&mut self) {
        #[cfg(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ))]
        // SAFETY: closes the fd this counter owns, once.
        unsafe {
            sys::close(self.0);
        }
    }
}
