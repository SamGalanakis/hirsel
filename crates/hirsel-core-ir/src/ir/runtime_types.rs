use super::{HostOp, Loan, Param, PlaceTy, RtOp, Scalar, Signature, Ty, TypeId};

impl RtOp {
    /// The signature row a call of this operation names. A `take` input is `&take T`
    /// and stays the caller's when the operation refuses before its commit point; an
    /// output the operation writes is `&mut T`.
    ///
    /// | op | params | results |
    /// |---|---|---|
    /// | `spawn[X, R]` | `n: u64`, `&X`, `f: ptr`, `tasks: u64` | task `ptr` (null on `Capacity`), `available: u64` |
    /// | `join[R]` | task `ptr`, `&R` | status `u8`: 0 Ok (R written), 1 Failed, 2 Cancelled; `Causes` `ptr` (null when Ok) |
    /// | `cancel` | task `ptr` | |
    /// | `check_cancel` | | |
    /// | `channel_new[T]` | `cap: u64` | sender `ptr`, receiver `ptr` (null on `Capacity`), `available: u64` |
    /// | `channel_sender` | sender `ptr` | sender `ptr` |
    /// | `send[T]` | sender `ptr`, `&T` | `bool`: published (false: `Closed`, the item stays) |
    /// | `try_send[T]` | sender `ptr`, `&T` | status `u8`: 0 published, 1 `Full`, 2 `Closed` |
    /// | `receive[T]` | receiver `ptr`, `&T` | `bool`: received (false: `Closed`) |
    /// | `try_receive[T]` | receiver `ptr`, `&T` | status `u8`: 0 received, 1 `Empty`, 2 `Closed` |
    /// | `dest_id` | | `u64`: a destination identity no other call of the run returns (MEM-4) |
    /// | `compare_bytes` | `a: ptr`, `a_len: u64`, `b: ptr`, `b_len: u64` | `i8`: -1, 0 or 1 as `a` orders before, equal to or after `b` |
    /// | `map_hash_bytes` | `data: ptr`, `len: u64` | `u64`: keyed byte sequence hash |
    /// | `map_hash_word` | `word: u64` | `u64`: keyed scalar hash |
    /// | `map_hash_start` | `state: ptr` | |
    /// | `map_hash_add_word` | `state: ptr`, `word: u64` | |
    /// | `map_hash_add_bytes` | `state: ptr`, `data: ptr`, `len: u64` | |
    /// | `map_hash_finish` | `state: ptr` | `u64`: keyed hash of the stream |
    /// | `xxh3_short` | `data: ptr`, `len: u64` | `u64`: XXH3-64 (crate docs, "XXH3") |
    /// | `xxh3_stripes` | `acc: ptr`, `data: ptr`, `secret: ptr`, `count: u64` | (crate docs, "XXH3") |
    /// | `sha1_blocks` | state `ptr`, `data: ptr`, `len: u64` | (crate docs, "SHA-1") |
    /// | `sha1_compress` | state `ptr`, `data: ptr`, `blocks: u64` | (crate docs, "SHA-1") |
    /// | `zero_bytes` | `block: ptr`, `at: ptr`, `len: u64` | (crate docs, "Zero fill") |
    /// | `sha256_blocks` | state `ptr`, `data: ptr`, `blocks: u64` | (crate docs, "SHA-256") |
    /// | `sha512_blocks` | state `ptr`, `data: ptr`, `blocks: u64` | (crate docs, "SHA-512") |
    /// | `crc32c` | `crc: u32`, `data: ptr`, `len: u64` | `u32`: updated CRC-32C |
    /// | `crc64nvme` | `crc: u64`, `data: ptr`, `len: u64` | `u64`: updated CRC-64/NVME |
    /// | `crc32c_combine` | `a: u32`, `b: u32`, `len_b: u64` | `u32`: concatenated CRC-32C |
    /// | `crc64nvme_combine` | `a: u64`, `b: u64`, `len_b: u64` | `u64`: concatenated CRC-64/NVME |
    /// | `ed25519_verify`, `p256_verify`, `p256_verify_der`, `p384_verify`, `p384_verify_der` | key, message and signature pointer/length pairs (lengths `u64`) | `u64`: 1 if valid, 0 otherwise |
    /// | `rsa_verify_pkcs1_sha256`, `rsa_verify_pss_sha256` | modulus, exponent, message and signature pointer/length pairs (lengths `u64`) | `u64`: 1 if valid, 0 otherwise |
    /// | `cause_next` | cell `ptr` | `ptr`: the next cell, null after the last |
    /// | `cause_value` | cell `ptr` | `ptr`: the cell's `Cause` |
    /// | `args_len` | `Args: u64` | `u64` |
    /// | `args_get` | `Args: u64`, `i: u64` | `ptr`: a `String` carrier, null past the end |
    /// | `host_open` | `Dir: u64`, path `ptr`, `mode: u8` | outcome |
    /// | `host_read` | `HostFile: u64`, buffer `ptr` | outcome |
    /// | `host_write` | `HostFile: u64`, bytes `ptr` | outcome |
    /// | `host_close`, `host_try_lock`, `host_unlock`, `host_sync` | `HostFile: u64` | outcome |
    /// | `host_sync_dir`, `host_zero_extension` | `Dir: u64` | outcome |
    /// | `host_random` | `Random: u64`, buffer `ptr` | |
    /// | `host_mkdir`, `host_remove`, `host_remove_tree` | `Dir: u64`, path `ptr` | outcome |
    /// | `host_mkdtemp` | `Dir: u64`, name `ptr` | outcome |
    /// | `host_wall_ms`, `host_mono_ns` | `Clock: u64` | `u64` |
    /// | `stdout_write`, `stderr_write` | stream `u64`, string `ptr` | outcome |
    /// | `isolate_enter` | | `bool`: reserved; `available: u64` (0 when reserved) |
    /// | `isolate_leave` | | |
    /// | `isolate_catch` | | `Causes` `ptr`: the caught chain's head |
    ///
    /// `f` is a plain function `(&X) -> () dest R`. `channel_new` with `cap` 0 fails
    /// with `Bounds`; `send`, `receive`, `join` and `check_cancel` fail with
    /// `Cancelled` when they observe it (CONC-7). `isolate_catch` fails, adding no
    /// cause, when the chain it would catch has the primary cause `Cancelled`. Every
    /// provider row can fail (for example `Cancelled` or `Bounds`). These are the rows
    /// that can fail
    /// ([`RtOp::fails`]); the others never do.
    ///
    /// `compare_bytes` orders unsigned bytes from the first, a proper prefix first
    /// (SEM-7); it reads both ranges, writes nothing, admits nothing and takes no
    /// context. A pointer with length 0 is never read, so it may be null.
    ///
    /// The provider rows (crate docs, "Providers") take the capability or file
    /// handle, and each `String` or `Bytes` operand as a pointer to its carrier.
    /// An outcome is `value: u64, error: u8, code: u32, written: u64`: `error` is
    /// 0 for success, else `IoError`'s case + 1 (`code` is `Other`'s payload),
    /// `value` is the opened `HostFile`, the bytes read, the lock taken (1) or the
    /// new `Dir`, and `written` the bytes written.
    pub fn signature(self) -> Signature {
        use Param::Value as V;
        let take = |ty| Param::Address {
            place: PlaceTy::Declared(ty),
            loan: Loan::Take,
        };
        let out = |ty| Param::Address {
            place: PlaceTy::Declared(ty),
            loan: Loan::Mut,
        };
        let (u8_, u64_, bool_) = (
            Ty::Scalar(Scalar::U8),
            Ty::Scalar(Scalar::U64),
            Ty::Scalar(Scalar::Bool),
        );
        let p = Ty::Ptr;
        let (params, results) = match self {
            RtOp::Spawn { x, .. } => (vec![V(u64_), take(x), V(p), V(u64_)], vec![p, u64_]),
            RtOp::Join { r } => (vec![V(p), out(r)], vec![u8_, p]),
            RtOp::Cancel => (vec![V(p)], vec![]),
            RtOp::CheckCancel => (vec![], vec![]),
            RtOp::ChannelNew { .. } => (vec![V(u64_)], vec![p, p, u64_]),
            RtOp::ChannelSender => (vec![V(p)], vec![p]),
            RtOp::Send { item } => (vec![V(p), take(item)], vec![bool_]),
            RtOp::TrySend { item } => (vec![V(p), take(item)], vec![u8_]),
            RtOp::Receive { item } => (vec![V(p), out(item)], vec![bool_]),
            RtOp::ReceiveRec { item } => (vec![V(p), V(p), out(item)], vec![bool_]),
            RtOp::ReceiveUntil { item } => {
                (vec![V(p), out(item), V(u8_), V(u64_), V(u64_)], vec![u8_])
            }
            RtOp::TryReceive { item } => (vec![V(p), out(item)], vec![u8_]),
            RtOp::LatestNew { .. } => (vec![V(p)], vec![p, u64_]),
            RtOp::ConnNew { .. } => (vec![V(u64_), V(u64_)], vec![p, p, u64_]),
            RtOp::ConnClient => (vec![V(p)], vec![p]),
            RtOp::ConnSend { item } | RtOp::ConnTrySend { item } => {
                (vec![V(p), take(item)], vec![u8_])
            }
            RtOp::ConnReceive { item } | RtOp::ConnTryReceive { item } => {
                (vec![V(p), out(item)], vec![u8_, u64_, u64_])
            }
            RtOp::SpawnUnit { x, .. } => {
                (vec![V(u64_), take(x), V(p), V(u64_)], vec![p, u64_, u64_])
            }
            RtOp::LatestHandle | RtOp::LatestRead | RtOp::SharedShare => (vec![V(p)], vec![p]),
            RtOp::SharedNew { item } => (vec![take(item), V(u64_)], vec![p, u64_]),
            RtOp::SharedRelease { .. } => (vec![V(p)], vec![]),
            RtOp::LatestPublish => (vec![V(p), V(p)], vec![]),
            RtOp::DestId => (vec![], vec![u64_]),
            RtOp::CompareBytes => (
                vec![V(p), V(u64_), V(p), V(u64_)],
                vec![Ty::Scalar(Scalar::I8)],
            ),
            RtOp::MapHashBytes | RtOp::MapPlaceBytes | RtOp::MapStrongBytes => {
                (vec![V(p), V(u64_)], vec![u64_])
            }
            RtOp::MapHashWord | RtOp::MapPlaceWord | RtOp::MapStrongWord => {
                (vec![V(u64_)], vec![u64_])
            }
            RtOp::MapHashStart | RtOp::MapStrongStart => (vec![V(p)], vec![]),
            RtOp::MapHashAddWord => (vec![V(p), V(u64_)], vec![]),
            RtOp::MapHashAddBytes => (vec![V(p), V(p), V(u64_)], vec![]),
            RtOp::MapHashFinish => (vec![V(p)], vec![u64_]),
            RtOp::Xxh3Short => (vec![V(p), V(u64_)], vec![u64_]),
            RtOp::Xxh3Stripes => (vec![V(p), V(p), V(p), V(u64_)], vec![]),
            RtOp::Sha1Blocks | RtOp::Sha1Compress => (vec![V(p), V(p), V(u64_)], vec![]),
            RtOp::ZeroBytes => (vec![V(p), V(p), V(u64_)], vec![]),
            RtOp::Sha256Blocks | RtOp::Sha512Blocks => (vec![V(p), V(p), V(u64_)], vec![]),
            RtOp::Crc32c => {
                let u32_ = Ty::Scalar(Scalar::U32);
                (vec![V(u32_), V(p), V(u64_)], vec![u32_])
            }
            RtOp::Crc64Nvme => (vec![V(u64_), V(p), V(u64_)], vec![u64_]),
            RtOp::Crc32cCombine => {
                let u32_ = Ty::Scalar(Scalar::U32);
                (vec![V(u32_), V(u32_), V(u64_)], vec![u32_])
            }
            RtOp::Crc64NvmeCombine => (vec![V(u64_), V(u64_), V(u64_)], vec![u64_]),
            RtOp::Ed25519Verify
            | RtOp::P256Verify
            | RtOp::P256VerifyDer
            | RtOp::P384Verify
            | RtOp::P384VerifyDer => (
                vec![V(p), V(u64_), V(p), V(u64_), V(p), V(u64_)],
                vec![u64_],
            ),
            RtOp::RsaVerifyPkcs1Sha256 | RtOp::RsaVerifyPssSha256 => (
                vec![V(p), V(u64_), V(p), V(u64_), V(p), V(u64_), V(p), V(u64_)],
                vec![u64_],
            ),
            RtOp::Argon2id => {
                let u32_ = Ty::Scalar(Scalar::U32);
                (
                    vec![
                        V(p),
                        V(u64_),
                        V(p),
                        V(u64_),
                        V(p),
                        V(u64_),
                        V(p),
                        V(u64_),
                        V(u32_),
                        V(u32_),
                        V(u32_),
                        V(u64_),
                        V(p),
                    ],
                    vec![u64_, u64_, u8_],
                )
            }
            RtOp::CauseNext | RtOp::CauseValue => (vec![V(p)], vec![p]),
            RtOp::ArgsLen => (vec![V(u64_)], vec![u64_]),
            RtOp::ArgsGet => (vec![V(u64_), V(u64_)], vec![p]),
            RtOp::RootLimit => (vec![], vec![u64_]),
            RtOp::LaunchWord => (vec![V(u64_), V(u64_)], vec![u64_]),
            RtOp::LaunchHandle | RtOp::SecretShare => (vec![V(u64_)], vec![u64_]),
            RtOp::FrameRecord => (vec![V(u64_), V(u8_), V(u8_)], vec![p]),
            RtOp::Transfer { x, .. } => (vec![take(x), V(p), V(u8_)], vec![bool_]),
            RtOp::Returnable(wait) => {
                let row = wait.row().signature();
                let mut params = vec![V(p)];
                params.extend(row.params);
                (params, row.results)
            }
            RtOp::LaunchTarget { view } => (vec![out(view)], vec![]),
            RtOp::DescribeEmit => (vec![V(p)], vec![bool_]),
            RtOp::Flow(_) => (vec![V(p), V(p), V(p)], vec![]),
            RtOp::StepEnter => (vec![V(u64_)], vec![bool_]),
            RtOp::StepLeave | RtOp::IsolateLeave | RtOp::Closed(_) => (vec![], vec![]),
            RtOp::StepCatch => (vec![], vec![bool_]),
            RtOp::IsolateEnter => (vec![], vec![bool_, u64_]),
            RtOp::IsolateCatch => (vec![], vec![p]),
            RtOp::Host(op) => op.operand_signature(),
        };
        Signature::new(params, results, None)
    }

    pub fn types(self) -> Vec<TypeId> {
        match self {
            RtOp::Returnable(wait) => wait.row().types(),
            RtOp::Spawn { x, r } | RtOp::SpawnUnit { x, r } | RtOp::Transfer { x, r } => vec![x, r],
            RtOp::Join { r } => vec![r],
            RtOp::ConnNew { item }
            | RtOp::ConnSend { item }
            | RtOp::ConnTrySend { item }
            | RtOp::ConnReceive { item }
            | RtOp::ConnTryReceive { item }
            | RtOp::ChannelNew { item }
            | RtOp::Send { item }
            | RtOp::TrySend { item }
            | RtOp::Receive { item }
            | RtOp::ReceiveRec { item }
            | RtOp::ReceiveUntil { item }
            | RtOp::TryReceive { item }
            | RtOp::LatestNew { item }
            | RtOp::SharedNew { item }
            | RtOp::SharedRelease { item } => vec![item],
            RtOp::LaunchTarget { view } => vec![view],
            RtOp::Cancel
            | RtOp::CheckCancel
            | RtOp::ChannelSender
            | RtOp::ConnClient
            | RtOp::LatestHandle
            | RtOp::LatestPublish
            | RtOp::LatestRead
            | RtOp::SharedShare
            | RtOp::DestId
            | RtOp::CompareBytes
            | RtOp::MapHashBytes
            | RtOp::MapHashWord
            | RtOp::MapHashStart
            | RtOp::MapHashAddWord
            | RtOp::MapHashAddBytes
            | RtOp::MapHashFinish
            | RtOp::MapPlaceWord
            | RtOp::MapPlaceBytes
            | RtOp::MapStrongWord
            | RtOp::MapStrongBytes
            | RtOp::MapStrongStart
            | RtOp::Xxh3Short
            | RtOp::Xxh3Stripes
            | RtOp::Sha1Blocks
            | RtOp::Sha1Compress
            | RtOp::ZeroBytes
            | RtOp::Sha256Blocks
            | RtOp::Sha512Blocks
            | RtOp::Crc32c
            | RtOp::Crc64Nvme
            | RtOp::Crc32cCombine
            | RtOp::Crc64NvmeCombine
            | RtOp::Ed25519Verify
            | RtOp::P256Verify
            | RtOp::P256VerifyDer
            | RtOp::P384Verify
            | RtOp::P384VerifyDer
            | RtOp::RsaVerifyPkcs1Sha256
            | RtOp::RsaVerifyPssSha256
            | RtOp::Argon2id
            | RtOp::CauseNext
            | RtOp::CauseValue
            | RtOp::ArgsLen
            | RtOp::ArgsGet
            | RtOp::RootLimit
            | RtOp::LaunchWord
            | RtOp::LaunchHandle
            | RtOp::SecretShare
            | RtOp::DescribeEmit
            | RtOp::Flow(_)
            | RtOp::Host(_)
            | RtOp::FrameRecord
            | RtOp::IsolateEnter
            | RtOp::IsolateLeave
            | RtOp::IsolateCatch
            | RtOp::StepEnter
            | RtOp::StepLeave
            | RtOp::StepCatch
            | RtOp::Closed(_) => vec![],
        }
    }

    /// Whether a call of this row can fail, and so returns the failure status
    /// (crate docs, "Native ABI").
    pub fn fails(self) -> bool {
        matches!(
            self,
            RtOp::Join { .. }
                | RtOp::Spawn { .. }
                | RtOp::CheckCancel
                | RtOp::Argon2id
                | RtOp::ChannelNew { .. }
                | RtOp::Send { .. }
                | RtOp::Receive { .. }
                | RtOp::ReceiveRec { .. }
                | RtOp::ReceiveUntil { .. }
                | RtOp::ConnNew { .. }
                | RtOp::ConnSend { .. }
                | RtOp::ConnTrySend { .. }
                | RtOp::ConnReceive { .. }
                | RtOp::SpawnUnit { .. }
                | RtOp::IsolateCatch
                | RtOp::Flow(_)
        ) || matches!(self, RtOp::Host(_) | RtOp::Returnable(_))
    }
}

/// The waits with a returnable row ([`RtOp::Returnable`]), each the stackful
/// row [`Wait::row`] with the child word in front.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Wait {
    /// An owned provider wait (one with [`HostOp::returnable_symbol`]).
    Host(HostOp),
    Join {
        r: TypeId,
    },
    ConnSend {
        item: TypeId,
    },
    ConnReceive {
        item: TypeId,
    },
}

impl Wait {
    /// The stackful row this wait wraps.
    pub fn row(self) -> RtOp {
        match self {
            Wait::Host(op) => RtOp::Host(op),
            Wait::Join { r } => RtOp::Join { r },
            Wait::ConnSend { item } => RtOp::ConnSend { item },
            Wait::ConnReceive { item } => RtOp::ConnReceive { item },
        }
    }

    /// The returnable wait of a stackful row, if it has one.
    pub fn of(row: RtOp) -> Option<Wait> {
        Some(match row {
            RtOp::Host(op) if op.returnable_symbol().is_some() => Wait::Host(op),
            RtOp::Join { r } => Wait::Join { r },
            RtOp::ConnSend { item } => Wait::ConnSend { item },
            RtOp::ConnReceive { item } => Wait::ConnReceive { item },
            _ => return None,
        })
    }
}
