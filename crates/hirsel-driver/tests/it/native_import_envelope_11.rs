//! Retired import-cache wire vectors must miss at the demand-product boundary.
use super::native_import_cutover_19::assert_retired_import_payloads;

// Handwritten: magic, version, section count, ordered (tag,length) headers,
// then raw payloads. The fourth section includes both byte boundaries.
const EMPTY: &[u8] = b"HIRIMP\x02\x05\x00\x00\x01\x00\x02\x00\x03\x00\x04\x00";
const BINARY: &[u8] = b"HIRIMP\x02\x05\x00\x01\x01\x02\x02\x00\x03\x02\x04\x00a[]\x00\xff";

