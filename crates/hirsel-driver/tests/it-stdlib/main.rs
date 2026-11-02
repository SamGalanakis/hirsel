//! The standard-library suites of this crate: ordinary std source compiled,
//! checked and executed through the driver, linked as their own binary so a
//! compiler edit relinks three smaller harnesses in parallel.

hirsel_guard::install!();

#[path = "../support/native_ready.rs"]
mod native_ready;
#[path = "../support/source_std.rs"]
mod source_std;

include!("modules.rs");

#[allow(dead_code)]
#[path = "../support/test_requests.rs"]
mod test_requests;
