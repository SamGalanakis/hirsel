use super::{HostOp, Loan, Param, PlaceTy, Scalar, Signature, Ty, TypeId};

/// Engine operations a call can name. Rows are in the crate docs;
/// a `take` argument is passed by address and is the callee's only after the
/// operation's commit point (FAIL-3), so a refused operation leaves it in place.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum RtOp {
    Spawn {
        x: TypeId,
        r: TypeId,
    },
    Join {
        r: TypeId,
    },
    Cancel,
    CheckCancel,
    ChannelNew {
        item: TypeId,
    },
    ChannelSender,
    Send {
        item: TypeId,
    },
    TrySend {
        item: TypeId,
    },
    Receive {
        item: TypeId,
    },
    ReceiveUntil {
        item: TypeId,
    },
    TryReceive {
        item: TypeId,
    },
    /// `latest.new`: `item` is `Arc[T]`, whose drop the root keeps for its last handle.
    LatestNew {
        item: TypeId,
    },
    LatestHandle,
    /// `conn.new` (CONC-17): `(cap, b) -> (client, inbox, available)`; a null
    /// client is `Capacity`, and `cap` 0 fails with `Bounds`.
    ConnNew {
        item: TypeId,
    },
    /// `conn.client`: one more client of the same connection.
    ConnClient,
    /// `secret.share` (TLS-5): one more holder of the designation, as its key.
    SecretShare,
    /// `conn.send`: 0 accepted, 1 closed (the item stays in its place).
    ConnSend {
        item: TypeId,
    },
    /// `conn.try_send`: 0 accepted, 1 closed, 2 full (the item stays in its place).
    ConnTrySend {
        item: TypeId,
    },
    /// `conn.receive`: `(outcome, requested, available)`, outcome 0 an item,
    /// 1 closed, 2 `Capacity` (the item stays queued).
    ConnReceive {
        item: TypeId,
    },
    /// `conn.try_receive`: as `ConnReceive`, and outcome 3 empty.
    ConnTryReceive {
        item: TypeId,
    },
    /// `task.spawn_unit` (CONC-15): `Spawn`'s operands, and
    /// `(task, requested, available)`; a null task is `Capacity`.
    SpawnUnit {
        x: TypeId,
        r: TypeId,
    },
    /// Installs a node and returns the old root's node, which the caller releases.
    LatestPublish,
    /// The root's node, which the caller shares before any other event.
    LatestRead,
    /// `shared.new` (CONC-15): `(x, charge) -> (node, available)`; the runtime
    /// admits `charge` and moves `x` into a new node. A null node is `Capacity`,
    /// with `x` still in its place.
    SharedNew {
        item: TypeId,
    },
    /// `shared.share`: one more holder of the node, counted atomically and not
    /// an event (CONC-15).
    SharedShare,
    /// One holder of a `Shared[item]` node fewer; the last one cleans the payload
    /// and refunds the node through its own record (CONC-15).
    SharedRelease {
        item: TypeId,
    },
    DestId,
    /// Bytewise order of two byte ranges (SEM-7): `String`, `Text` and `Bytes`.
    CompareBytes,
    /// Keyed SipHash-1-3 over a byte sequence or one canonical scalar word.
    MapHashBytes,
    MapHashWord,
    /// Composite Hash: initialize, append to, and finish the caller's scratch.
    MapHashStart,
    MapHashAddWord,
    MapHashAddBytes,
    MapHashFinish,
    MapPlaceWord,
    MapPlaceBytes,
    MapStrongWord,
    MapStrongBytes,
    MapStrongStart,
    /// The SHA-256 compression of whole blocks into an eight-word state (crate docs, "SHA-256").
    Sha256Blocks,
    Sha512Blocks,
    /// The next cell of a `Causes` chain, or null (crate docs, "Causes").
    CauseNext,
    /// A cause cell's `Cause`, in core's layout (crate docs, "Causes").
    CauseValue,
    /// `args.len` (CAP-5): the run's argument count.
    ArgsLen,
    /// `args.get`: a run-owned `String` carrier of capacity 0, or null.
    ArgsGet,
    /// A provider operation of `host`, `stdout` or `stderr` (CAP-5).
    Host(HostOp),
    /// `isolate`'s entry: reserve a cause cell (crate docs, "Isolation").
    IsolateEnter,
    /// `isolate`'s body returned: refund the innermost reservation.
    IsolateLeave,
    /// `isolate`'s handler: take the caught chain as a `Causes`, or fail on with
    /// it when its primary cause is `Cancelled`.
    IsolateCatch,
    /// Reserve steps from the current bound; false leaves the stack unchanged.
    StepEnter,
    /// Close the current lexical step scope and refund unused steps.
    StepLeave,
    /// Close a failed step scope, catching only its own OutOfTime causes.
    StepCatch,
    /// Charge MEM-30's logical units. The low six bits of the second operand
    /// name the quantum's exponent. The third operand is the fixed weight.
    WorkSteps,
    /// The current task's original allowance.
    RootLimit,
    /// Private immutable launch metadata: `(field, index) -> word`.
    LaunchWord,
    /// Private launch target as a borrowed String carrier, anchored to this run.
    LaunchTarget {
        view: TypeId,
    },
    /// Write a Bytes carrier through the private describe transport.
    DescribeEmit,
    /// Owner flow operation with pointers to its fixed operand and result words.
    Flow(hirsel_core_layout::flow::Op),
    /// SHA-1 compression of a byte span and its RFC 3174 padding.
    Sha1Blocks,
    /// Zero a byte span of a list's storage (crate docs, "Zero fill").
    ZeroBytes,
    /// XXH3-64 over at most 240 bytes with seed zero and the default secret.
    Xxh3Short,
    /// Accumulate complete XXH3 stripes in eight words with the given secret.
    Xxh3Stripes,
    /// Take the socket or read the scratch grant designated by the place host.
    LaunchHandle,
    /// Extend a finalized Castagnoli CRC over a readable byte range.
    Crc32c,
    /// Extend a finalized NVME CRC over a readable byte range.
    Crc64Nvme,
    /// Concatenate finalized CRC-32C checksums.
    Crc32cCombine,
    /// Concatenate finalized CRC-64/NVME checksums.
    Crc64NvmeCombine,
    /// Verify a pure Ed25519 signature over a message.
    Ed25519Verify,
    /// Verify a fixed-width P-256 ECDSA signature over a message with SHA-256.
    P256Verify,
    /// Verify a DER P-256 ECDSA signature over a message with SHA-256.
    P256VerifyDer,
    /// Verify a fixed-width P-384 ECDSA signature over a message with SHA-384.
    P384Verify,
    /// Verify a DER P-384 ECDSA signature over a message with SHA-384.
    P384VerifyDer,
    /// Verify an RSA PKCS#1 v1.5 signature over a message with SHA-256.
    RsaVerifyPkcs1Sha256,
    /// Verify an RSA-PSS signature with SHA-256 and a 32-byte salt.
    RsaVerifyPssSha256,
    /// Argon2id v0x13 with block steps and temporary working-memory admission.
    Argon2id,
    /// The running task's frame record (`crate::returnable`). Takes the area
    /// size and `u8` flags for network and connection-send state. Allocated
    /// on first entry, reused on resume, and freed when the root ends.
    FrameRecord,
    /// A wait that may park with the stack given back (`crate::returnable`):
    /// a leading pointer to the caller's child word, then the wrapped row's
    /// operands and results. The word is 1 when the task parked and the
    /// result is not yet delivered, else 0 with the row's result.
    Returnable(Wait),
    /// `Receive` that may park with the stack given back (`crate::returnable`):
    /// a leading pointer to the caller's child word, then `Receive`'s operands.
    /// The word is 1 when the task parked and nothing is received yet, else 0
    /// with `Receive`'s outcome.
    ReceiveRec {
        item: TypeId,
    },
    /// Hand the running task to a fresh root (`crate::returnable`): copy the
    /// `x` input the leading address takes into new zero-charge task input,
    /// with the code word after it, and make the task's body enter that code
    /// through the transfer glue of `(x, r)` once its current frames have
    /// returned; a nonzero `u8` says the root reads none of it once parked,
    /// so it is freed then. True when taken; false (no frame record, replay).
    Transfer {
        x: TypeId,
        r: TypeId,
    },
    /// SHA-1 compression of complete blocks, without padding.
    Sha1Compress,
    /// FAIL-17 glue worklist (crate docs, "Glue worklist"): the running task's
    /// worklist depth, the mark a walk truncates back to.
    GlueMark,
    /// Push one four-word entry `(place, place, resume, index)` on the worklist.
    GluePush,
    /// Pop the top entry above the mark operand: a pointer to its four words, valid
    /// until the next push, or null when the worklist is at the mark.
    GluePop,
    /// Truncate the worklist to the mark operand.
    GlueCut,
    /// A function on LANG-20's closed list is reached; [`super::CLOSED`] names it by
    /// index. It does nothing at run time. A constant's initialiser that
    /// reaches it stops there, and the constant is refused.
    Closed(u8),
    /// `code.bind(f)`: bind a PROG-9 descriptor to a `Code[F]` handle (PACK-14).
    CodeBind,
    /// `code.load[F](loader, key, entry_id)`: validate and load a typed entry (PACK-14).
    CodeLoad,
    /// `code.share(c)`: retain one more counted `Code[F]` handle (PACK-14).
    CodeShare,
    /// `code.slot[F](loader)`: create an empty version-0 `CodeSlot[F]` (PACK-14).
    CodeSlotNew,
    /// `code.activate(mut s, expected, c)`: linearised slot swap (PACK-14).
    CodeActivate,
    /// `code.dispatch` entry: borrow worker-cached generation, or null if empty (PACK-14).
    CodeDispatchBegin,
    /// `code.dispatch` exit: release worker borrow or promoted continuation pin (PACK-14).
    CodeDispatchEnd,
    HeldNew {
        item: TypeId,
    },
    HeldTake {
        item: TypeId,
    },
    ResourceOrigin {
        item: TypeId,
    },
    /// Relocate a loaded image's owned static `String` literal into its `CauseData` R/NX mapping.
    StaticString,
    /// Relocate a loaded image's owned static `FrozenBytes` literal into its `CauseData` R/NX mapping.
    StaticFrozen,
    /// Release any `CauseData` count owned by a `cap == 0` byte carrier.
    ReleaseStaticBytes,
    /// Borrow the compiler witness and input. The next seven words are the
    /// data limits in declaration order. Commit the owned result only on the
    /// normal edge. On a capacity or step failure the operation frees every
    /// staged block and the destination remains uninitialized.
    DataDecode {
        witness: TypeId,
        bytes: TypeId,
        output: TypeId,
    },
    /// `code.identity(c)`: PROG-9 identity bytes view anchored in `c` (PACK-14).
    CodeIdentity,
    /// `code.revision(c)`: optional PROG-9 revision bytes view anchored in `c` (PACK-14).
    CodeRevision,
}

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
            RtOp::GlueMark => (vec![], vec![u64_]),
            RtOp::GluePush => (vec![V(p), V(p), V(u64_), V(u64_)], vec![]),
            RtOp::GluePop => (vec![V(u64_)], vec![p]),
            RtOp::GlueCut => (vec![V(u64_)], vec![]),
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
            RtOp::WorkSteps => (vec![V(u64_), V(u64_), V(u64_)], vec![]),
            RtOp::StepLeave | RtOp::IsolateLeave | RtOp::Closed(_) => (vec![], vec![]),
            RtOp::StepCatch => (vec![], vec![bool_]),
            RtOp::IsolateEnter => (vec![], vec![bool_, u64_]),
            RtOp::IsolateCatch => (vec![], vec![p]),
            RtOp::CodeBind | RtOp::CodeShare => (vec![V(p)], vec![p]),
            RtOp::HeldNew { item } => (vec![take(item), V(p), V(u64_)], vec![p, u64_]),
            RtOp::HeldTake { item } => (vec![V(p), V(p), out(item)], vec![bool_]),
            RtOp::ResourceOrigin { .. } => (vec![V(p)], vec![]),
            RtOp::StaticString => (vec![V(p), V(u64_)], vec![p]),
            RtOp::StaticFrozen => (vec![V(p), V(p), V(u64_)], vec![p, p]),
            RtOp::DataDecode {
                witness,
                bytes,
                output,
            } => {
                let view = |row| Param::Address {
                    place: PlaceTy::Declared(row),
                    loan: Loan::View,
                };
                let mut params = vec![view(witness), view(bytes)];
                params.extend((0..7).map(|_| V(u64_)));
                params.push(out(output));
                (params, vec![])
            }
            RtOp::ReleaseStaticBytes => (vec![V(p)], vec![]),
            RtOp::CodeSlotNew => (vec![V(u64_)], vec![p]),
            RtOp::CodeLoad => (
                vec![V(u64_), V(p), V(p), V(p), V(u8_), V(p), V(p), V(p)],
                vec![u8_],
            ),
            RtOp::CodeActivate => (vec![V(p), V(u64_), V(p), V(p)], vec![u8_]),
            RtOp::CodeDispatchBegin => (vec![V(p), V(p)], vec![p]),
            RtOp::CodeDispatchEnd => (vec![V(u64_)], vec![]),
            RtOp::CodeIdentity => (vec![V(p), V(p)], vec![]),
            RtOp::CodeRevision => (vec![V(p), V(p)], vec![u8_]),
            RtOp::Host(op) => op.operand_signature(),
        };
        Signature::new(params, results, None)
    }

    pub fn types(self) -> Vec<TypeId> {
        match self {
            RtOp::DataDecode {
                witness,
                bytes,
                output,
            } => vec![witness, bytes, output],
            RtOp::Returnable(wait) => wait.row().types(),
            RtOp::Spawn { x, r } | RtOp::SpawnUnit { x, r } | RtOp::Transfer { x, r } => vec![x, r],
            RtOp::Join { r } => vec![r],
            RtOp::ConnNew { item }
            | RtOp::ConnSend { item }
            | RtOp::ConnTrySend { item }
            | RtOp::ConnReceive { item }
            | RtOp::ConnTryReceive { item }
            | RtOp::HeldNew { item }
            | RtOp::HeldTake { item }
            | RtOp::ResourceOrigin { item }
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
            | RtOp::GlueMark
            | RtOp::GluePush
            | RtOp::GluePop
            | RtOp::GlueCut
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
            | RtOp::WorkSteps
            | RtOp::Closed(_)
            | RtOp::CodeBind
            | RtOp::CodeLoad
            | RtOp::CodeShare
            | RtOp::CodeSlotNew
            | RtOp::CodeActivate
            | RtOp::CodeDispatchBegin
            | RtOp::CodeDispatchEnd
            | RtOp::CodeIdentity
            | RtOp::CodeRevision
            | RtOp::StaticString
            | RtOp::StaticFrozen
            | RtOp::ReleaseStaticBytes => vec![],
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
                | RtOp::DataDecode { .. }
                | RtOp::Argon2id
                | RtOp::WorkSteps
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
