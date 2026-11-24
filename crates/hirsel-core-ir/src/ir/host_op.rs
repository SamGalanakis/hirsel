/// The provider operations, numbered as the run time's `HirselCoreHostRequest`
/// operation codes ([`HostOp::code`]).
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum HostOp {
    Open,
    Read,
    Write,
    Close,
    TryLock,
    Unlock,
    Mkdir,
    Mkdtemp,
    Remove,
    RemoveTree,
    WallMs,
    MonoNs,
    Stdout,
    Stderr,
    Rename,
    SleepUntil,
    /// Flush a file's data and read-visible metadata with fdatasync.
    Sync,
    /// Flush directory entries with fsync.
    SyncDir,
    /// Fill a Bytes carrier's spare capacity from the Random capability.
    Random,
    Connect,
    Listen,
    Accept,
    LocalAddr = hirsel_core_ids::host_op::LOCAL_ADDR as isize,
    PeerAddr = hirsel_core_ids::host_op::PEER_ADDR as isize,
    RecvBounded = 27,
    /// Wait until a read half's next receive has something to report.
    RecvReady = hirsel_core_ids::host_op::RECV_READY as isize,
    SendPair = 26,
    NetNarrow = 24,
    NetSplit = 25,
    Stopped = 28,
    TlsConnect = 22,
    NetNarrowTls = 29,
    SendSecret = 23,
    SecretSeal = 30,
    SecretNarrow = 31,
    OpenDir = hirsel_core_ids::host_op::OPEN_DIR as isize,
    Entries = hirsel_core_ids::host_op::ENTRIES as isize,
    NextEntries = hirsel_core_ids::host_op::NEXT_ENTRIES as isize,
    Stat = hirsel_core_ids::host_op::STAT as isize,
    StatFile = hirsel_core_ids::host_op::STAT_FILE as isize,
    ReadAt = 37,
    WriteAt = 38,
    SecretHmac = hirsel_core_ids::host_op::SECRET_MAC as isize,
    SecretVerifyHmac = 42,
    SecretDeriveHmac = hirsel_core_ids::host_op::SECRET_KDF as isize,
    SecretWrap = 99,
    SecretUnwrap = 100,
    SecretSign = 101,
    SecretAeadSeal = 120,
    SecretAeadOpen = 121,
    SecretDeriveHkdf = 122,
    SecretDeriveArgon2id = 123,
    Resolve = hirsel_core_ids::host_op::RESOLVE as isize,
    CredFetch = hirsel_core_ids::host_op::CRED_FETCH as isize,
    NetNarrowResolve = 41,
    StdinRead = hirsel_core_ids::host_op::STDIN_READ as isize,
    EnvGet = 33,
    EnvLen = 34,
    EnvName = 35,
    EnvNarrow = 36,
    ProvideClock = hirsel_core_ids::host_op::PROVIDE_CLOCK as isize,
    ProvideRandom = hirsel_core_ids::host_op::PROVIDE_RANDOM as isize,
    ProvideRandomTape = hirsel_core_ids::host_op::PROVIDE_RANDOM_TAPE as isize,
    ProvideProcess = hirsel_core_ids::host_op::PROVIDE_PROCESS as isize,
    ClockAdvance = hirsel_core_ids::host_op::CLOCK_ADVANCE as isize,
    ClockSetWall = hirsel_core_ids::host_op::CLOCK_SET_WALL as isize,
    ClockIdle = hirsel_core_ids::host_op::CLOCK_IDLE as isize,
    ProcessTrigger = hirsel_core_ids::host_op::PROCESS_TRIGGER as isize,
    ProvideStdout = hirsel_core_ids::host_op::PROVIDE_STDOUT as isize,
    ProvideStderr = hirsel_core_ids::host_op::PROVIDE_STDERR as isize,
    PortNext = hirsel_core_ids::host_op::PORT_NEXT as isize,
    CallWord = hirsel_core_ids::host_op::CALL_WORD as isize,
    CallSecret = hirsel_core_ids::host_op::CALL_SECRET as isize,
    ExecSpawn = hirsel_core_ids::host_op::EXEC_SPAWN as isize,
    ExecWait = hirsel_core_ids::host_op::EXEC_WAIT as isize,
    ExecSignal = hirsel_core_ids::host_op::EXEC_SIGNAL as isize,
    CallPayload = hirsel_core_ids::host_op::CALL_PAYLOAD as isize,
    PortReply = hirsel_core_ids::host_op::PORT_REPLY as isize,
    ProvideDir = hirsel_core_ids::host_op::PROVIDE_DIR as isize,
    ProvideNet = hirsel_core_ids::host_op::PROVIDE_NET as isize,
    ProvideExec = hirsel_core_ids::host_op::PROVIDE_EXEC as isize,
    PortFill = hirsel_core_ids::host_op::PORT_FILL as isize,
    UdpFillPackets = hirsel_core_ids::host_op::UDP_FILL_PACKETS as isize,
    ScramSha256 = hirsel_core_ids::host_op::SCRAM as isize,
    ScramVerify = hirsel_core_ids::host_op::SCRAM_VERIFY as isize,
    StreamTerminal = 96,
    NetJoin = 97,
    ScramFinalLen = 98,
    ExecNarrow = hirsel_core_ids::host_op::EXEC_NARROW as isize,
    UnixConnect = hirsel_core_ids::host_op::UNIX_CONNECT as isize,
    UnixListen = hirsel_core_ids::host_op::UNIX_LISTEN as isize,
    UnixAccept = hirsel_core_ids::host_op::UNIX_ACCEPT as isize,
    DatagramOpen = hirsel_core_ids::host_op::DATAGRAM_OPEN as isize,
    DatagramBind = hirsel_core_ids::host_op::DATAGRAM_BIND as isize,
    DatagramConnect = hirsel_core_ids::host_op::DATAGRAM_CONNECT as isize,
    DatagramSend = hirsel_core_ids::host_op::DATAGRAM_SEND as isize,
    DatagramSendTo = hirsel_core_ids::host_op::DATAGRAM_SEND_TO as isize,
    DatagramRecv = hirsel_core_ids::host_op::DATAGRAM_RECV as isize,
    UdpOpen = hirsel_core_ids::host_op::UDP_OPEN as isize,
    UdpBind = hirsel_core_ids::host_op::UDP_BIND as isize,
    UdpBindGroup = hirsel_core_ids::host_op::UDP_BIND_GROUP as isize,
    UdpRecv = hirsel_core_ids::host_op::UDP_RECV as isize,
    UdpSend = hirsel_core_ids::host_op::UDP_SEND as isize,
    UdpLocalAddr = hirsel_core_ids::host_op::UDP_LOCAL_ADDR as isize,
    NetNarrowUnix = hirsel_core_ids::host_op::NET_NARROW_UNIX as isize,
    /// Wait until the clock's wall time reaches an absolute millisecond reading.
    SleepUntilWall = hirsel_core_ids::host_op::SLEEP_UNTIL_WALL as isize,
    Mkstemp = hirsel_core_ids::host_op::MKSTEMP as isize,
    ZeroExtension = hirsel_core_ids::host_op::ZERO_EXTENSION as isize,
    CallPayloadPrefix = hirsel_core_ids::host_op::CALL_PAYLOAD_PREFIX as isize,
    SecretHmacSha384 = 125,
    SecretHmacSha512 = 126,
    SecretVerifyHmacSha384 = 127,
    SecretVerifyHmacSha512 = 128,
    SecretJwsHmacSign = 129,
    SecretJwsHmacVerify = 130,
    SecretP256Key = hirsel_core_ids::host_op::MEDIA_P256_KEY as isize,
    SecretKeyShare = hirsel_core_ids::host_op::MEDIA_KEY_SHARE as isize,
}

impl HostOp {
    /// Whether the native adapter returns an IP endpoint with its outcome.
    pub const fn address_outcome(self) -> bool {
        matches!(
            self,
            Self::Listen
                | Self::Accept
                | Self::LocalAddr
                | Self::PeerAddr
                | Self::UdpBind
                | Self::UdpBindGroup
                | Self::UdpLocalAddr
        )
    }

    pub(super) fn address_signature(self) -> (Vec<super::Param>, Vec<super::Ty>) {
        use super::{Param::Value as V, Scalar, Ty};
        let byte = Ty::Scalar(Scalar::U8);
        let short = Ty::Scalar(Scalar::U16);
        let word = Ty::Scalar(Scalar::U32);
        let long = Ty::Scalar(Scalar::U64);
        let mut args = vec![V(long)];
        match self {
            Self::Listen | Self::UdpBind | Self::UdpBindGroup => {
                args.extend([V(byte), V(long), V(long), V(short)]);
                if self == Self::Listen {
                    args.push(V(short));
                } else {
                    args.extend([V(word), V(word)]);
                    if self == Self::UdpBindGroup {
                        args.extend([V(short), V(word), V(Ty::Ptr)]);
                    }
                }
            }
            Self::Accept => args.extend([V(byte), V(long), V(long)]),
            Self::LocalAddr | Self::PeerAddr | Self::UdpLocalAddr => {}
            _ => unreachable!(),
        }
        (args, vec![long, byte, word, long, byte, long, long, short])
    }

    /// Native entry for rows that use the common operand signature.
    pub fn io_symbol(self) -> &'static str {
        match self {
            Self::WallMs | Self::MonoNs => "hirsel_core_host_clock",
            Self::Stat | Self::StatFile => "hirsel_core_host_stat",
            Self::Random | Self::Stdout | Self::Stderr | Self::Stopped => "hirsel_core_host_io",
            _ => "hirsel_core_host_op",
        }
    }

    pub const ALL: &[HostOp] = &[
        HostOp::Open,
        HostOp::Read,
        HostOp::Write,
        HostOp::Close,
        HostOp::TryLock,
        HostOp::Unlock,
        HostOp::Mkdir,
        HostOp::Mkdtemp,
        HostOp::Mkstemp,
        HostOp::Remove,
        HostOp::RemoveTree,
        HostOp::WallMs,
        HostOp::MonoNs,
        HostOp::Stdout,
        HostOp::Stderr,
        HostOp::Rename,
        HostOp::SleepUntil,
        HostOp::SleepUntilWall,
        HostOp::Sync,
        HostOp::SyncDir,
        HostOp::ZeroExtension,
        HostOp::Random,
        HostOp::Connect,
        HostOp::Listen,
        HostOp::Accept,
        HostOp::LocalAddr,
        HostOp::PeerAddr,
        HostOp::TlsConnect,
        HostOp::SendSecret,
        HostOp::SecretSeal,
        HostOp::SecretNarrow,
        HostOp::SecretHmac,
        HostOp::SecretVerifyHmac,
        HostOp::SecretHmacSha384,
        HostOp::SecretHmacSha512,
        HostOp::SecretVerifyHmacSha384,
        HostOp::SecretVerifyHmacSha512,
        HostOp::SecretJwsHmacSign,
        HostOp::SecretJwsHmacVerify,
        HostOp::SecretDeriveHmac,
        HostOp::SecretDeriveHkdf,
        HostOp::SecretDeriveArgon2id,
        HostOp::SecretWrap,
        HostOp::SecretUnwrap,
        HostOp::SecretSign,
        HostOp::SecretP256Key,
        HostOp::SecretKeyShare,
        HostOp::SecretAeadSeal,
        HostOp::SecretAeadOpen,
        HostOp::RecvBounded,
        HostOp::RecvReady,
        HostOp::SendPair,
        HostOp::NetNarrow,
        HostOp::NetNarrowTls,
        HostOp::NetSplit,
        HostOp::Stopped,
        HostOp::OpenDir,
        HostOp::Entries,
        HostOp::NextEntries,
        HostOp::Stat,
        HostOp::StatFile,
        HostOp::ReadAt,
        HostOp::WriteAt,
        HostOp::Resolve,
        HostOp::CredFetch,
        HostOp::NetNarrowResolve,
        HostOp::UnixConnect,
        HostOp::UnixListen,
        HostOp::UnixAccept,
        HostOp::DatagramOpen,
        HostOp::DatagramBind,
        HostOp::DatagramConnect,
        HostOp::DatagramSend,
        HostOp::DatagramSendTo,
        HostOp::DatagramRecv,
        HostOp::UdpOpen,
        HostOp::UdpBind,
        HostOp::UdpBindGroup,
        HostOp::UdpRecv,
        HostOp::UdpSend,
        HostOp::UdpLocalAddr,
        HostOp::NetNarrowUnix,
        HostOp::StdinRead,
        HostOp::EnvGet,
        HostOp::EnvLen,
        HostOp::EnvName,
        HostOp::EnvNarrow,
        HostOp::ProvideClock,
        HostOp::ProvideRandom,
        HostOp::ProvideRandomTape,
        HostOp::ProvideProcess,
        HostOp::ClockAdvance,
        HostOp::ClockSetWall,
        HostOp::ClockIdle,
        HostOp::ProcessTrigger,
        HostOp::ProvideStdout,
        HostOp::ProvideStderr,
        HostOp::PortNext,
        HostOp::CallWord,
        HostOp::CallSecret,
        HostOp::ExecSpawn,
        HostOp::ExecWait,
        HostOp::ExecSignal,
        HostOp::CallPayload,
        HostOp::CallPayloadPrefix,
        HostOp::PortReply,
        HostOp::ProvideDir,
        HostOp::PortFill,
        HostOp::UdpFillPackets,
        HostOp::StreamTerminal,
        HostOp::NetJoin,
        HostOp::ScramSha256,
        HostOp::ScramFinalLen,
        HostOp::ScramVerify,
        HostOp::ProvideNet,
        HostOp::ProvideExec,
        HostOp::ExecNarrow,
    ];
    pub(super) fn operand_signature(self) -> (Vec<super::Param>, Vec<super::Ty>) {
        use super::{Param::Value as V, Scalar, Ty};
        let (u8_, u64_, p) = (Ty::Scalar(Scalar::U8), Ty::Scalar(Scalar::U64), Ty::Ptr);
        let op = self;
        let outcome = vec![u64_, u8_, Ty::Scalar(Scalar::U32), u64_];
        match op {
            HostOp::Connect => (
                vec![
                    V(u64_),
                    V(u8_),
                    V(u64_),
                    V(u64_),
                    V(Ty::Scalar(Scalar::U16)),
                    V(u8_),
                    V(u64_),
                    V(u64_),
                ],
                outcome,
            ),
            HostOp::Listen
            | HostOp::Accept
            | HostOp::LocalAddr
            | HostOp::PeerAddr
            | HostOp::UdpBind
            | HostOp::UdpBindGroup
            | HostOp::UdpLocalAddr => op.address_signature(),
            HostOp::UdpOpen => (
                vec![
                    V(u64_),
                    V(Ty::Scalar(Scalar::Bool)),
                    V(Ty::Scalar(Scalar::U32)),
                    V(Ty::Scalar(Scalar::U32)),
                ],
                outcome,
            ),
            HostOp::UdpRecv | HostOp::UdpSend => {
                (vec![V(u64_), V(p), V(p), V(u8_), V(u64_), V(u64_)], outcome)
            }
            HostOp::RecvBounded => (
                vec![V(u64_), V(p), V(u64_), V(u8_), V(u64_), V(u64_)],
                outcome,
            ),
            HostOp::Resolve | HostOp::CredFetch | HostOp::UnixConnect | HostOp::SendPair => {
                (vec![V(u64_), V(p), V(p), V(u8_), V(u64_), V(u64_)], outcome)
            }
            HostOp::NetNarrowResolve => (vec![V(u64_), V(p)], outcome),
            HostOp::ExecNarrow => (
                vec![V(u64_), V(Ty::Scalar(Scalar::U32)), V(p), V(p)],
                outcome,
            ),
            HostOp::UnixListen => (
                vec![V(u64_), V(p), V(p), V(Ty::Scalar(Scalar::U16))],
                outcome,
            ),
            HostOp::UnixAccept | HostOp::RecvReady | HostOp::ExecWait => {
                (vec![V(u64_), V(u8_), V(u64_), V(u64_)], outcome)
            }
            HostOp::DatagramSend | HostOp::DatagramRecv => {
                (vec![V(u64_), V(p), V(u8_), V(u64_), V(u64_)], outcome)
            }
            HostOp::DatagramSendTo => (
                vec![V(u64_), V(p), V(p), V(p), V(u8_), V(u64_), V(u64_)],
                outcome,
            ),
            HostOp::NetNarrowUnix => (vec![V(u64_), V(u8_), V(p), V(p)], outcome),
            HostOp::NetNarrow => (
                vec![
                    V(u64_),
                    V(u8_),
                    V(u8_),
                    V(u64_),
                    V(u64_),
                    V(u8_),
                    V(Ty::Scalar(Scalar::U16)),
                    V(Ty::Scalar(Scalar::U16)),
                ],
                outcome,
            ),
            HostOp::TlsConnect => (
                vec![
                    V(u64_),
                    V(u64_),
                    V(p),
                    V(u8_),
                    V(p),
                    V(p),
                    V(u8_),
                    V(u64_),
                    V(u64_),
                ],
                vec![
                    u64_,
                    u8_,
                    Ty::Scalar(Scalar::U32),
                    u64_,
                    u64_,
                    u64_,
                    u64_,
                    u64_,
                ],
            ),
            HostOp::SendSecret => (
                vec![V(u64_), V(p), V(p), V(p), V(u8_), V(u64_), V(u64_)],
                outcome,
            ),
            HostOp::ScramSha256 => (
                vec![
                    V(u64_),
                    V(u64_),
                    V(p),
                    V(p),
                    V(p),
                    V(p),
                    V(u8_),
                    V(u64_),
                    V(u64_),
                ],
                outcome,
            ),
            HostOp::ScramVerify | HostOp::SecretNarrow => (vec![V(u64_), V(p)], outcome),
            // The caller retains the carrier, whose owned bytes are wiped on success.
            HostOp::SecretSeal => (vec![V(p), V(p)], outcome),
            HostOp::SecretDeriveHmac => (vec![V(u64_), V(p), V(p), V(p)], outcome),
            HostOp::SecretDeriveHkdf => (vec![V(u64_), V(p), V(p), V(u64_), V(p)], outcome),
            HostOp::SecretDeriveArgon2id => {
                let w = V(Ty::Scalar(Scalar::U32));
                (vec![V(u64_), V(p), w, w, w, V(u64_), V(p)], outcome)
            }
            HostOp::SecretHmac
            | HostOp::SecretVerifyHmac
            | HostOp::SecretHmacSha384
            | HostOp::SecretHmacSha512
            | HostOp::SecretVerifyHmacSha384
            | HostOp::SecretVerifyHmacSha512
            | HostOp::SecretUnwrap
            | HostOp::SecretP256Key
            | HostOp::DatagramBind
            | HostOp::DatagramConnect
            | HostOp::ScramFinalLen
            | HostOp::Rename => (vec![V(u64_), V(p), V(p)], outcome),
            HostOp::SecretJwsHmacSign | HostOp::SecretJwsHmacVerify => {
                (vec![V(u64_), V(p), V(p), V(u64_)], outcome)
            }
            HostOp::SecretSign => (vec![V(u64_), V(u64_), V(u64_), V(p), V(p)], outcome),
            HostOp::SecretAeadSeal | HostOp::SecretAeadOpen => {
                (vec![V(u64_), V(u64_), V(p), V(p), V(p), V(p)], outcome)
            }
            HostOp::NetNarrowTls => (
                vec![
                    V(u64_),
                    V(p),
                    V(Ty::Scalar(Scalar::U16)),
                    V(Ty::Scalar(Scalar::U16)),
                    V(u8_),
                    V(p),
                ],
                outcome,
            ),
            HostOp::DatagramOpen
            | HostOp::NetSplit
            | HostOp::EnvLen
            | HostOp::ProvideRandom
            | HostOp::ClockIdle
            | HostOp::ProcessTrigger
            | HostOp::ProvideDir
            | HostOp::ProvideNet
            | HostOp::ProvideExec
            | HostOp::PortNext => (vec![V(u64_)], outcome),
            HostOp::ProvideProcess => (vec![], outcome),
            HostOp::ProvideRandomTape => (vec![V(p)], outcome),
            HostOp::ProvideStdout | HostOp::ProvideStderr => {
                (vec![V(u64_), V(Ty::Scalar(Scalar::Bool))], outcome)
            }
            HostOp::StreamTerminal | HostOp::ExecSignal => (vec![V(u64_), V(u8_)], outcome),
            HostOp::CallWord | HostOp::CallSecret | HostOp::NetJoin => {
                (vec![V(u64_), V(u64_)], outcome)
            }
            HostOp::CallPayload
            | HostOp::PortFill
            | HostOp::UdpFillPackets
            | HostOp::ReadAt
            | HostOp::WriteAt
            | HostOp::SecretWrap
            | HostOp::SecretKeyShare => (vec![V(u64_), V(u64_), V(p)], outcome),
            HostOp::CallPayloadPrefix => (vec![V(u64_), V(u64_), V(u64_), V(p)], outcome),
            HostOp::PortReply => (vec![V(u64_); 9], outcome),
            HostOp::ProvideClock | HostOp::ClockAdvance | HostOp::ClockSetWall => {
                (vec![V(u64_), V(u64_)], outcome)
            }
            HostOp::EnvGet | HostOp::EnvName => {
                let input = if op == HostOp::EnvGet { p } else { u64_ };
                (
                    vec![V(u64_), V(input)],
                    vec![p, u8_, Ty::Scalar(Scalar::U32), u64_],
                )
            }
            HostOp::ExecSpawn => {
                let mut results = outcome;
                results.extend([u64_; 3]);
                (
                    vec![
                        V(u64_),
                        V(p),
                        V(p),
                        V(p),
                        V(p),
                        V(p),
                        V(u8_),
                        V(u8_),
                        V(u8_),
                    ],
                    results,
                )
            }
            HostOp::StdinRead => (vec![V(u64_), V(p), V(u8_), V(u64_), V(u64_)], outcome),
            HostOp::Open => (vec![V(u64_), V(p), V(u8_)], outcome),
            HostOp::SleepUntil | HostOp::SleepUntilWall => (vec![V(u64_), V(u64_)], outcome),
            HostOp::Stat | HostOp::StatFile => {
                let mut metadata = outcome;
                metadata[3] = Ty::Scalar(Scalar::I64);
                metadata.push(u8_);
                let params = if op == HostOp::Stat {
                    vec![V(u64_), V(p)]
                } else {
                    vec![V(u64_)]
                };
                (params, metadata)
            }
            HostOp::Close
            | HostOp::TryLock
            | HostOp::Unlock
            | HostOp::Sync
            | HostOp::SyncDir
            | HostOp::ZeroExtension => (vec![V(u64_)], outcome),
            HostOp::Random => (vec![V(u64_), V(p)], vec![]),
            HostOp::Stopped => (vec![V(u64_)], vec![]),
            HostOp::WallMs | HostOp::MonoNs => (vec![V(u64_)], vec![u64_]),
            _ => (vec![V(u64_), V(p)], outcome),
        }
    }

    /// The run time's operation code (`hirsel_core_rt_host.h`).
    pub const fn code(self) -> u8 {
        self as u8
    }
    /// The row's name after `rt.`.
    pub const fn name(self) -> &'static str {
        match self {
            HostOp::Open => "host_open",
            HostOp::Read => "host_read",
            HostOp::OpenDir => "host_open_dir",
            HostOp::Entries => "host_entries",
            HostOp::NextEntries => "host_next_entries",
            HostOp::Stat => "host_stat",
            HostOp::StatFile => "host_stat_file",
            HostOp::ReadAt => "host_read_at",
            HostOp::WriteAt => "host_write_at",
            HostOp::Write => "host_write",
            HostOp::Close => "host_close",
            HostOp::TryLock => "host_try_lock",
            HostOp::Unlock => "host_unlock",
            HostOp::Mkdir => "host_mkdir",
            HostOp::Mkdtemp => "host_mkdtemp",
            HostOp::Mkstemp => "host_mkstemp",
            HostOp::Remove => "host_remove",
            HostOp::RemoveTree => "host_remove_tree",
            HostOp::WallMs => "host_wall_ms",
            HostOp::MonoNs => "host_mono_ns",
            HostOp::Stdout => "stdout_write",
            HostOp::Stderr => "stderr_write",
            HostOp::Rename => "host_rename",
            HostOp::SleepUntil => "host_sleep_until",
            HostOp::SleepUntilWall => "host_sleep_until_wall",
            HostOp::Sync => "host_sync",
            HostOp::SyncDir => "host_sync_dir",
            HostOp::ZeroExtension => "host_zero_extension",
            HostOp::Random => "host_random",
            HostOp::Connect => "host_connect",
            HostOp::Resolve => "host_resolve",
            HostOp::CredFetch => "cred_fetch",
            HostOp::Listen => "host_listen",
            HostOp::Accept => "host_accept",
            HostOp::LocalAddr => "net_local_addr",
            HostOp::PeerAddr => "net_peer_addr",
            HostOp::RecvBounded => "host_recv_bounded",
            HostOp::RecvReady => "host_recv_ready",
            HostOp::SendPair => "host_send_pair",
            HostOp::NetNarrow => "net_narrow",
            HostOp::ExecNarrow => "exec_narrow",
            HostOp::NetNarrowTls => "net_narrow_tls",
            HostOp::NetNarrowResolve => "net_narrow_resolve",
            HostOp::UnixConnect => "host_connect_unix",
            HostOp::UnixListen => "host_listen_unix",
            HostOp::UnixAccept => "host_accept_unix",
            HostOp::DatagramOpen => "host_open_datagram",
            HostOp::DatagramBind => "host_bind_datagram",
            HostOp::DatagramConnect => "host_connect_datagram",
            HostOp::DatagramSend => "host_send_datagram",
            HostOp::DatagramSendTo => "host_send_to",
            HostOp::DatagramRecv => "host_recv_datagram",
            HostOp::UdpOpen => "host_udp_open",
            HostOp::UdpBind => "host_udp_bind",
            HostOp::UdpBindGroup => "host_udp_bind_group",
            HostOp::UdpRecv => "host_udp_recv",
            HostOp::UdpSend => "host_udp_send",
            HostOp::UdpLocalAddr => "host_udp_local_addr",
            HostOp::NetNarrowUnix => "net_narrow_unix",
            HostOp::TlsConnect => "tls_connect",
            HostOp::SendSecret => "send_secret",
            HostOp::SecretSeal => "secret_seal",
            HostOp::SecretNarrow => "secret_narrow",
            HostOp::SecretHmac => "secret_hmac_sha256",
            HostOp::SecretDeriveHmac => "secret_derive_hmac_sha256",
            HostOp::SecretDeriveHkdf => "secret_derive_hkdf_sha256",
            HostOp::SecretDeriveArgon2id => "secret_derive_argon2id",
            HostOp::SecretVerifyHmac => "secret_verify_hmac_sha256",
            HostOp::SecretHmacSha384 => "secret_hmac_sha384",
            HostOp::SecretHmacSha512 => "secret_hmac_sha512",
            HostOp::SecretVerifyHmacSha384 => "secret_verify_hmac_sha384",
            HostOp::SecretVerifyHmacSha512 => "secret_verify_hmac_sha512",
            HostOp::SecretJwsHmacSign => "secret_jws_hmac_sign",
            HostOp::SecretJwsHmacVerify => "secret_jws_hmac_verify",
            HostOp::SecretWrap => "secret_wrap",
            HostOp::SecretUnwrap => "secret_unwrap",
            HostOp::SecretSign => "secret_sign",
            HostOp::SecretP256Key => "secret_p256_key",
            HostOp::SecretKeyShare => "secret_key_share",
            HostOp::SecretAeadSeal => "secret_aead_seal",
            HostOp::SecretAeadOpen => "secret_aead_open",
            HostOp::NetSplit => "net_split",
            HostOp::NetJoin => "net_join",
            HostOp::ScramSha256 => "scram_sha256",
            HostOp::ScramFinalLen => "scram_final_len",
            HostOp::ScramVerify => "scram_verify",
            HostOp::Stopped => "host_stopped",
            HostOp::StdinRead => "stdin_read",
            HostOp::EnvGet => "env_get",
            HostOp::EnvLen => "env_len",
            HostOp::EnvName => "env_name",
            HostOp::EnvNarrow => "env_narrow",
            HostOp::ProvideClock => "provide_clock",
            HostOp::ProvideRandom => "provide_random",
            HostOp::ProvideRandomTape => "provide_random_tape",
            HostOp::ProvideProcess => "provide_process",
            HostOp::ClockAdvance => "clock_advance_to",
            HostOp::ClockSetWall => "clock_set_wall",
            HostOp::ClockIdle => "clock_idle",
            HostOp::ProcessTrigger => "process_trigger",
            HostOp::ProvideStdout => "provide_stdout",
            HostOp::ProvideStderr => "provide_stderr",
            HostOp::PortNext => "port_next",
            HostOp::CallWord => "call_word",
            HostOp::CallSecret => "call_secret",
            HostOp::ExecSpawn => "exec_spawn",
            HostOp::ExecWait => "exec_wait",
            HostOp::ExecSignal => "exec_signal",
            HostOp::CallPayload => "call_payload",
            HostOp::CallPayloadPrefix => "call_payload_prefix",
            HostOp::PortReply => "port_reply",
            HostOp::ProvideDir => "provide_dir",
            HostOp::ProvideNet => "provide_net",
            HostOp::ProvideExec => "provide_exec",
            HostOp::PortFill => "port_fill",
            HostOp::UdpFillPackets => "udp_fill_packets",
            HostOp::StreamTerminal => "stream_is_terminal",
        }
    }
    /// C entry of a host operation that has its own adapter.
    pub const fn adapter_symbol(self) -> &'static str {
        match self {
            Self::Connect => "hirsel_core_host_connect",
            Self::Resolve => "hirsel_core_host_resolve",
            Self::CredFetch => "hirsel_core_cred_fetch",
            Self::Listen => "hirsel_core_host_listen",
            Self::Accept => "hirsel_core_host_accept",
            Self::LocalAddr => "hirsel_core_net_local_addr",
            Self::PeerAddr => "hirsel_core_net_peer_addr",
            Self::RecvBounded => "hirsel_core_host_recv_bounded",
            Self::RecvReady => "hirsel_core_host_recv_ready",
            Self::SendPair => "hirsel_core_host_send_pair",
            Self::NetNarrow => "hirsel_core_net_narrow",
            Self::ExecNarrow => "hirsel_core_exec_narrow",
            Self::NetNarrowTls => "hirsel_core_net_narrow_tls",
            Self::NetNarrowResolve => "hirsel_core_net_narrow_resolve",
            Self::UnixConnect => "hirsel_core_host_connect_unix",
            Self::UnixListen => "hirsel_core_host_listen_unix",
            Self::UnixAccept => "hirsel_core_host_accept_unix",
            Self::DatagramOpen => "hirsel_core_host_open_datagram",
            Self::DatagramBind => "hirsel_core_host_bind_datagram",
            Self::DatagramConnect => "hirsel_core_host_connect_datagram",
            Self::DatagramSend => "hirsel_core_host_send_datagram",
            Self::DatagramSendTo => "hirsel_core_host_send_to",
            Self::DatagramRecv => "hirsel_core_host_recv_datagram",
            Self::UdpOpen => "hirsel_core_host_udp_open",
            Self::UdpBind => "hirsel_core_host_udp_bind",
            Self::UdpBindGroup => "hirsel_core_host_udp_bind_group",
            Self::UdpRecv => "hirsel_core_host_udp_recv",
            Self::UdpSend => "hirsel_core_host_udp_send",
            Self::UdpLocalAddr => "hirsel_core_host_udp_local_addr",
            Self::NetNarrowUnix => "hirsel_core_net_narrow_unix",
            Self::TlsConnect => "hirsel_core_tls_connect",
            Self::SendSecret => "hirsel_core_host_send_secret",
            Self::SecretSeal => "hirsel_core_secret_seal",
            Self::SecretNarrow => "hirsel_core_secret_narrow",
            Self::SecretHmac => "hirsel_core_secret_hmac_sha256",
            Self::SecretDeriveHmac => "hirsel_core_secret_derive_hmac_sha256",
            Self::SecretDeriveHkdf => "hirsel_core_secret_derive_hkdf_sha256",
            Self::SecretDeriveArgon2id => "hirsel_core_secret_derive_argon2id",
            Self::SecretVerifyHmac => "hirsel_core_secret_verify_hmac_sha256",
            Self::SecretHmacSha384 => "hirsel_core_secret_hmac_sha384",
            Self::SecretHmacSha512 => "hirsel_core_secret_hmac_sha512",
            Self::SecretVerifyHmacSha384 => "hirsel_core_secret_verify_hmac_sha384",
            Self::SecretVerifyHmacSha512 => "hirsel_core_secret_verify_hmac_sha512",
            Self::SecretJwsHmacSign => "hirsel_core_secret_jws_hmac_sign",
            Self::SecretJwsHmacVerify => "hirsel_core_secret_jws_hmac_verify",
            Self::SecretWrap => "hirsel_core_secret_wrap",
            Self::SecretUnwrap => "hirsel_core_secret_unwrap",
            Self::SecretSign => "hirsel_core_secret_sign",
            Self::SecretP256Key => "hirsel_core_secret_p256_key",
            Self::SecretKeyShare => "hirsel_core_secret_key_share",
            Self::SecretAeadSeal => "hirsel_core_secret_aead_seal",
            Self::SecretAeadOpen => "hirsel_core_secret_aead_open",
            Self::NetSplit => "hirsel_core_net_split",
            Self::NetJoin => "hirsel_core_net_join",
            Self::ScramSha256 => "hirsel_core_scram_sha256",
            Self::ScramFinalLen => "hirsel_core_scram_final_len",
            Self::ScramVerify => "hirsel_core_scram_verify",
            Self::StdinRead => "hirsel_core_stdin_read",
            Self::EnvGet => "hirsel_core_env_get",
            Self::EnvLen => "hirsel_core_env_len",
            Self::EnvName => "hirsel_core_env_name",
            Self::EnvNarrow => "hirsel_core_env_narrow",
            Self::ProvideClock => "hirsel_core_provide_clock",
            Self::ProvideRandom => "hirsel_core_provide_random",
            Self::ProvideRandomTape => "hirsel_core_provide_random_tape",
            Self::ProvideProcess => "hirsel_core_provide_process",
            Self::ClockAdvance => "hirsel_core_clock_advance_to",
            Self::ClockSetWall => "hirsel_core_clock_set_wall",
            Self::ClockIdle => "hirsel_core_clock_idle",
            Self::ProcessTrigger => "hirsel_core_process_trigger",
            Self::ProvideStdout => "hirsel_core_provide_stdout",
            Self::ProvideStderr => "hirsel_core_provide_stderr",
            Self::PortNext => "hirsel_core_port_next",
            Self::CallWord => "hirsel_core_call_word",
            Self::CallSecret => "hirsel_core_call_secret",
            Self::ExecSpawn => "hirsel_core_exec_spawn",
            Self::ExecWait => "hirsel_core_exec_wait",
            Self::ExecSignal => "hirsel_core_exec_signal",
            Self::CallPayload => "hirsel_core_call_payload",
            Self::CallPayloadPrefix => "hirsel_core_call_payload_prefix",
            Self::PortReply => "hirsel_core_port_reply",
            Self::ProvideDir => "hirsel_core_provide_dir",
            Self::ProvideNet => "hirsel_core_provide_net",
            Self::ProvideExec => "hirsel_core_provide_exec",
            Self::PortFill => "hirsel_core_port_fill",
            Self::UdpFillPackets => "hirsel_core_udp_fill_packets",
            Self::StreamTerminal => "hirsel_core_stream_is_terminal",
            _ => unreachable!(),
        }
    }

    /// The adapter for an owned wait that can return its native stack.
    pub const fn returnable_symbol(self) -> Option<&'static str> {
        match self {
            Self::RecvBounded => Some("hirsel_core_host_recv_bounded_rec"),
            Self::RecvReady => Some("hirsel_core_host_recv_ready_rec"),
            Self::SendPair => Some("hirsel_core_host_send_pair_rec"),
            _ => None,
        }
    }

    pub const fn is_secret_key(self) -> bool {
        matches!(
            self,
            Self::SecretWrap
                | Self::SecretUnwrap
                | Self::SecretSign
                | Self::SecretP256Key
                | Self::SecretKeyShare
                | Self::SecretAeadSeal
                | Self::SecretAeadOpen
        )
    }

    /// Whether a Secret MAC operation returns a tag instead of a verdict.
    pub const fn secret_mac_signs(self) -> bool {
        matches!(
            self,
            Self::SecretHmac
                | Self::SecretHmacSha384
                | Self::SecretHmacSha512
                | Self::SecretJwsHmacSign
        )
    }

    /// Whether the operation waits (a cancellation point, so it can fail).
    pub const fn waits(self) -> bool {
        !matches!(
            self,
            HostOp::WallMs
                | HostOp::MonoNs
                | HostOp::Random
                | HostOp::NetNarrow
                | HostOp::ExecNarrow
                | HostOp::NetNarrowTls
                | HostOp::NetNarrowResolve
                | HostOp::NetNarrowUnix
                | HostOp::SecretSeal
                | HostOp::SecretNarrow
                | HostOp::SecretHmac
                | HostOp::SecretHmacSha384
                | HostOp::SecretHmacSha512
                | HostOp::SecretVerifyHmacSha384
                | HostOp::SecretVerifyHmacSha512
                | HostOp::SecretJwsHmacSign
                | HostOp::SecretJwsHmacVerify
                | HostOp::SecretDeriveHmac
                | HostOp::SecretDeriveHkdf
                | HostOp::SecretDeriveArgon2id
                | HostOp::SecretVerifyHmac
                | HostOp::SecretWrap
                | HostOp::SecretUnwrap
                | HostOp::SecretSign
                | HostOp::SecretP256Key
                | HostOp::SecretKeyShare
                | HostOp::SecretAeadSeal
                | HostOp::SecretAeadOpen
                | HostOp::NetSplit
                | HostOp::NetJoin
                | HostOp::ScramFinalLen
                | HostOp::ScramVerify
                | HostOp::EnvGet
                | HostOp::EnvLen
                | HostOp::EnvName
                | HostOp::EnvNarrow
                | HostOp::ProvideClock
                | HostOp::ProvideRandom
                | HostOp::ProvideRandomTape
                | HostOp::ProvideProcess
                | HostOp::ClockAdvance
                | HostOp::ClockSetWall
                | HostOp::ProcessTrigger
                | HostOp::ProvideStdout
                | HostOp::ProvideStderr
                | HostOp::CallWord
                | HostOp::CallSecret
                | HostOp::PortReply
                | HostOp::ProvideDir
                | HostOp::ProvideNet
                | HostOp::ProvideExec
                | HostOp::PortFill
                | HostOp::UdpFillPackets
                | HostOp::StreamTerminal
        )
    }
}

/// Cleanup for one-word provider handles that borrow the runtime context.
pub fn host_drop_symbol(handle: hirsel_core_layout::Handle) -> Option<&'static str> {
    use hirsel_core_layout::Handle;
    Some(match handle {
        Handle::HostFile => "hirsel_core_hostfile_drop",
        Handle::Socket => "hirsel_core_socket_drop",
        Handle::Secret => "hirsel_core_secret_drop",
        Handle::Media => "hirsel_core_media_drop",
        _ => return None,
    })
}
