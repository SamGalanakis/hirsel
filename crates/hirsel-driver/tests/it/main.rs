//! The integration tests of this crate, linked as one binary, apart from the std
//! suites (tests/it-stdlib) and scaling probes (tests/it-scaling).

hirsel_guard::install!();

#[path = "../support/image_fixture.rs"]
mod image_fixture;
#[allow(dead_code)]
#[path = "../support/native_ready.rs"]
mod native_ready;
#[path = "../support/source_std.rs"]
mod source_std;

include!("modules.rs");
