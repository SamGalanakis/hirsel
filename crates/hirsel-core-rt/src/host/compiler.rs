//! Registry compiler-grant provider. A launch owns one bounded service and
//! its private scratch directory for all calls sharing the designation.
use crate::provider::{Outcome, Request};
use std::path::PathBuf;

pub(crate) struct Binding {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    service: hirsel_compiler::grant::Designation,
}

impl Binding {
    pub(crate) fn new(worker: PathBuf) -> Box<Self> {
        #[cfg(target_os = "ios")]
        let _ = worker;
        Box::new(Self {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            service: hirsel_compiler::grant::Designation::new(worker),
        })
    }

    pub(crate) fn handle(&self) -> u64 {
        std::ptr::from_ref(self) as u64
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn check(&self, input: &[u8]) -> (u64, Vec<u8>) {
        self.service.check(input)
    }

    #[cfg(target_os = "ios")]
    fn check(&self, _: &[u8]) -> (u64, Vec<u8>) {
        (9, b"compiler workers are unavailable on iOS".to_vec())
    }
}

/// The caller owns the request, borrowed input and reply until delivery.
pub(crate) unsafe extern "C" fn issue(_: *mut u8, row: *mut Request) -> Outcome {
    // SAFETY: the adapter pins these operands until the synchronous issue returns.
    let request = unsafe { &*row };
    let reply = unsafe { &mut *request.path.cast_mut().cast::<Vec<u8>>() };
    let input = if request.buffer.len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(request.buffer.data, request.buffer.len) }
    };
    let binding = unsafe { (request.handle as *const Binding).as_ref() };
    let (value, bytes) = binding.map_or_else(
        || (9, b"compiler service is unavailable".to_vec()),
        |binding| crate::units::off_slot(crate::units::HostCall::Bounded, || binding.check(input)),
    );
    *reply = bytes;
    Outcome {
        value,
        ..Default::default()
    }
}
