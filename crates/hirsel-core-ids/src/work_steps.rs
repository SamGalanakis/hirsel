//! MEM-30's work charge (docs/spec/budgets.md, "Work steps"): the further steps
//! an operation takes, before any of its work, for the units it processes and
//! the fixed weight of an asymmetric operation. This table is the one source of
//! every operation's quantum and weight; each engine emits the same count from
//! it, so the evaluator, Cranelift and LLVM agree.

/// One row of the work table: `units` cost `ceil(units / 2^shift) + weight`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Work {
    /// The quantum, as a power of two: units per step.
    pub shift: u32,
    /// Further steps every charge of this operation takes.
    pub weight: u64,
}

impl Work {
    const fn per(shift: u32) -> Self {
        Self { shift, weight: 0 }
    }

    const fn with(self, weight: u64) -> Self {
        Self { weight, ..self }
    }

    /// Units per step.
    pub const fn quantum(self) -> u64 {
        1 << self.shift
    }

    /// Steps for `units`: one per quantum or part of one, plus the weight,
    /// saturating at `u64::MAX`.
    pub fn steps(self, units: u64) -> u64 {
        self.steps_sum(&[units])
    }

    /// Steps for the exact sum of `units`, as [`Work::steps`].
    pub fn steps_sum(self, units: &[u64]) -> u64 {
        let total: u128 = units.iter().map(|&u| u128::from(u)).sum();
        let bulk = total.div_ceil(1u128 << self.shift);
        u64::try_from(bulk)
            .ok()
            .and_then(|bulk| bulk.checked_add(self.weight))
            .unwrap_or(u64::MAX)
    }
}

/// The work table. Each quantum is a power of two chosen so that one step
/// stands for a bounded amount of work on every engine (the rule in MEM-30);
/// the measurements behind each row are in the change's report, not here.
pub mod table {
    use super::Work;

    /// Moving, filling or comparing bytes: `memmove`, `memset`, `memcmp`,
    /// appends, the bytes of a clone, `copy_within`, `fill`, `compare_bytes`
    /// and byte-sequence `==`. Units are bytes.
    pub const COPY: Work = Work::per(12);
    /// Visiting leaves one at a time: structural `==` and ordering over
    /// non-byte elements (a zero-sized element counts one), `list.sum`, the
    /// elements `deque.remove` shifts. Units are leaves.
    pub const WALK: Work = Work::per(5);
    /// Cloning an element that owns a block: units are such elements.
    pub const CLONE_ITEM: Work = Work::per(2);
    /// `columns.fill`: units are elements.
    pub const COLUMNS_FILL: Work = Work::per(6);
    /// `int.parse`: units are input bytes.
    pub const PARSE: Work = Work::per(4);
    /// UTF-8 validation (`string.from_utf8`) past its ASCII prefix: units are
    /// input bytes.
    pub const UTF8: Work = Work::per(3);
    /// The ASCII prefix UTF-8 validation scans in blocks: units are bytes.
    pub const UTF8_ASCII: Work = Work::per(8);
    /// Base64 encoding: units are input bytes.
    pub const BASE64: Work = Work::per(3);
    /// ASCII case mapping: units are bytes.
    pub const ASCII_CASE: Work = Work::per(6);
    /// `sha1.blocks`: units are bytes.
    pub const SHA1: Work = Work::per(8);
    /// `sha256.blocks`: units are bytes.
    pub const SHA256: Work = Work::per(8);
    /// `sha512.blocks`: units are bytes.
    pub const SHA512: Work = Work::per(5);
    /// `crc32c.update`: units are bytes.
    pub const CRC32C: Work = Work::per(12);
    /// `crc64nvme.update`: units are bytes.
    pub const CRC64: Work = Work::per(11);
    /// `check.xxh3_stripes`: units are bytes.
    pub const XXH3: Work = Work::per(11);
    /// Authenticated encryption and key wrapping: units are the bytes sealed
    /// or opened, associated data included.
    pub const AEAD: Work = Work::per(8).with(1);
    /// HMAC with SHA-1: units are message bytes.
    pub const HMAC_SHA1: Work = Work::per(6).with(2);
    /// HMAC with SHA-256: units are message bytes.
    pub const HMAC_SHA256: Work = Work::per(8).with(1);
    /// HMAC with SHA-384 or SHA-512: units are message bytes.
    pub const HMAC_SHA512: Work = Work::per(6).with(4);
    /// Ed25519 verification: units are message bytes (hashed with SHA-512).
    pub const ED25519_VERIFY: Work = Work::per(6).with(256);
    /// Ed25519 signing: units are message bytes.
    pub const ED25519_SIGN: Work = Work::per(6).with(256);
    /// ECDSA P-256 with SHA-256 verification: units are message bytes.
    pub const P256_VERIFY: Work = Work::per(8).with(512);
    /// ECDSA P-256 with SHA-256 signing: units are message bytes.
    pub const P256_SIGN: Work = Work::per(8).with(128);
    /// ECDSA P-384 with SHA-384 verification: units are message bytes.
    pub const P384_VERIFY: Work = Work::per(6).with(8192);
    /// RSA (PKCS#1 v1.5 or PSS) with SHA-256 verification, at the largest
    /// modulus admitted: units are message bytes.
    pub const RSA_VERIFY: Work = Work::per(8).with(2048);
    /// RS256 signing, at the largest modulus admitted: units are message bytes.
    pub const RS256_SIGN: Work = Work::per(8).with(32768);
}

/// Codes for the core-source work charge (`core.work_steps`): a body written
/// in core source names its table row by one of these constants.
pub mod code {
    use super::{Work, table};

    pub const WALK: u64 = 0;
    pub const PARSE: u64 = 1;
    pub const UTF8: u64 = 2;
    pub const BASE64: u64 = 3;
    pub const ASCII_CASE: u64 = 4;
    pub const COPY: u64 = 5;
    pub const UTF8_ASCII: u64 = 6;

    /// The table row a core-source code names.
    pub fn work(code: u64) -> Option<Work> {
        Some(match code {
            WALK => table::WALK,
            PARSE => table::PARSE,
            UTF8 => table::UTF8,
            BASE64 => table::BASE64,
            ASCII_CASE => table::ASCII_CASE,
            COPY => table::COPY,
            UTF8_ASCII => table::UTF8_ASCII,
            _ => return None,
        })
    }
}
