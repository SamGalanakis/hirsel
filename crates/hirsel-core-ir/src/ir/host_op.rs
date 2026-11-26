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
    SecretHmacSha1 = 164,
    SecretVerifyHmacSha1 = 165,
    SecretP256Key = hirsel_core_ids::host_op::MEDIA_P256_KEY as isize,
    SecretKeyShare = hirsel_core_ids::host_op::MEDIA_KEY_SHARE as isize,
    ScheduleQuicInitial = hirsel_core_ids::host_op::SCHEDULE_QUIC_INITIAL as isize,
    ScheduleImportTraffic = hirsel_core_ids::host_op::SCHEDULE_IMPORT_TRAFFIC as isize,
    ScheduleTls13 = hirsel_core_ids::host_op::SCHEDULE_TLS13 as isize,
    ScheduleApplication = hirsel_core_ids::host_op::SCHEDULE_APPLICATION as isize,
    ScheduleKeyUpdate = hirsel_core_ids::host_op::SCHEDULE_KEY_UPDATE as isize,
    ScheduleRetire = hirsel_core_ids::host_op::SCHEDULE_RETIRE as isize,
    ScheduleFinished = hirsel_core_ids::host_op::SCHEDULE_FINISHED as isize,
    ScheduleVerifyFinished = hirsel_core_ids::host_op::SCHEDULE_VERIFY_FINISHED as isize,
    ScheduleDtls12 = hirsel_core_ids::host_op::SCHEDULE_DTLS12 as isize,
    ScheduleImportSrtp = hirsel_core_ids::host_op::SCHEDULE_IMPORT_SRTP as isize,
    ScheduleSrtp = hirsel_core_ids::host_op::SCHEDULE_SRTP as isize,
    ScheduleSpaceInfo = hirsel_core_ids::host_op::SCHEDULE_SPACE_INFO as isize,
    ScheduleSealerPair = hirsel_core_ids::host_op::SCHEDULE_SEALER_PAIR as isize,
    ScheduleImportSframe = hirsel_core_ids::host_op::SCHEDULE_IMPORT_SFRAME as isize,
    ScheduleSframeSealer = hirsel_core_ids::host_op::SCHEDULE_SFRAME_SEALER as isize,
    ScheduleSframeOpener = hirsel_core_ids::host_op::SCHEDULE_SFRAME_OPENER as isize,
    ScheduleSframeRatchet = hirsel_core_ids::host_op::SCHEDULE_SFRAME_RATCHET as isize,
    ScheduleSealer = hirsel_core_ids::host_op::SCHEDULE_SEALER as isize,
    ScheduleOpener = hirsel_core_ids::host_op::SCHEDULE_OPENER as isize,
    ScheduleNextOpener = hirsel_core_ids::host_op::SCHEDULE_NEXT_OPENER as isize,
    ScheduleHeaderMask = hirsel_core_ids::host_op::SCHEDULE_HEADER_MASK as isize,
    PacketSeal = hirsel_core_ids::host_op::PACKET_SEAL as isize,
    PacketSealInto = hirsel_core_ids::host_op::PACKET_SEAL_INTO as isize,
    PacketOpen = hirsel_core_ids::host_op::PACKET_OPEN as isize,
    PacketOpenInto = hirsel_core_ids::host_op::PACKET_OPEN_INTO as isize,
    PacketMask = hirsel_core_ids::host_op::PACKET_MASK as isize,
    SecretDeriveDigest = 166,
    NetClass = 168,
    NetDenials = 169,
    NetNarrowBegin = 170,
    NetNarrowEnd = 171,
    TryLockShared = hirsel_core_ids::host_op::TRY_LOCK_SHARED as isize,
    ZeroAt = hirsel_core_ids::host_op::ZERO_AT as isize,
    /// TLS-9: read a ClientHello on an accepted socket.
    TlsHello = hirsel_core_ids::host_op::TLS_HELLO as isize,
    /// TLS-9: complete a server handshake.
    TlsAccept = hirsel_core_ids::host_op::TLS_ACCEPT as isize,
    /// TLS-9: a `Hello`'s server name, a view of its record.
    TlsServerName = 176,
    /// A stream's terminal size, as `cols | rows << 16`, or 0 for none.
    StreamSize = hirsel_core_ids::host_op::TERM_SIZE as isize,
    /// `stdin.read_secret`: an echo-off line, sealed by the host.
    StdinSecret = hirsel_core_ids::host_op::STDIN_SECRET as isize,
}

mod name;
mod symbol;
pub use symbol::host_drop_symbol;

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
        HostOp::TryLockShared,
        HostOp::ZeroAt,
        HostOp::Random,
        HostOp::Connect,
        HostOp::Listen,
        HostOp::Accept,
        HostOp::LocalAddr,
        HostOp::PeerAddr,
        HostOp::TlsConnect,
        HostOp::TlsHello,
        HostOp::TlsAccept,
        HostOp::TlsServerName,
        HostOp::SendSecret,
        HostOp::SecretSeal,
        HostOp::SecretNarrow,
        HostOp::SecretHmac,
        HostOp::SecretHmacSha1,
        HostOp::SecretVerifyHmacSha1,
        HostOp::SecretVerifyHmac,
        HostOp::SecretHmacSha384,
        HostOp::SecretHmacSha512,
        HostOp::SecretVerifyHmacSha384,
        HostOp::SecretVerifyHmacSha512,
        HostOp::SecretJwsHmacSign,
        HostOp::SecretJwsHmacVerify,
        HostOp::SecretDeriveHmac,
        HostOp::SecretDeriveDigest,
        HostOp::SecretDeriveHkdf,
        HostOp::SecretDeriveArgon2id,
        HostOp::SecretWrap,
        HostOp::SecretUnwrap,
        HostOp::SecretSign,
        HostOp::SecretP256Key,
        HostOp::SecretKeyShare,
        HostOp::ScheduleQuicInitial,
        HostOp::ScheduleImportTraffic,
        HostOp::ScheduleTls13,
        HostOp::ScheduleApplication,
        HostOp::ScheduleKeyUpdate,
        HostOp::ScheduleRetire,
        HostOp::ScheduleFinished,
        HostOp::ScheduleVerifyFinished,
        HostOp::ScheduleDtls12,
        HostOp::ScheduleImportSrtp,
        HostOp::ScheduleSrtp,
        HostOp::ScheduleSpaceInfo,
        HostOp::ScheduleSealerPair,
        HostOp::ScheduleImportSframe,
        HostOp::ScheduleSframeSealer,
        HostOp::ScheduleSframeOpener,
        HostOp::ScheduleSframeRatchet,
        HostOp::ScheduleSealer,
        HostOp::ScheduleOpener,
        HostOp::ScheduleNextOpener,
        HostOp::ScheduleHeaderMask,
        HostOp::PacketSeal,
        HostOp::PacketSealInto,
        HostOp::PacketOpen,
        HostOp::PacketOpenInto,
        HostOp::PacketMask,
        HostOp::SecretAeadSeal,
        HostOp::SecretAeadOpen,
        HostOp::RecvBounded,
        HostOp::RecvReady,
        HostOp::SendPair,
        HostOp::NetNarrow,
        HostOp::NetClass,
        HostOp::NetDenials,
        HostOp::NetNarrowBegin,
        HostOp::NetNarrowEnd,
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
        HostOp::StdinSecret,
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
        HostOp::StreamSize,
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
            HostOp::NetClass => (vec![V(u8_), V(u64_), V(u64_)], outcome),
            HostOp::NetDenials | HostOp::NetNarrowBegin | HostOp::NetNarrowEnd => {
                (vec![V(u64_)], outcome)
            }
            HostOp::NetNarrow => (
                vec![
                    V(u64_),
                    V(u8_),
                    V(u8_),
                    V(u64_),
                    V(u64_),
                    V(u8_),
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
            HostOp::TlsHello => (vec![V(u64_), V(u8_), V(u64_), V(u64_)], outcome),
            // The Hello, the chain and ALPN carriers and the key's designation.
            HostOp::TlsAccept => (
                vec![V(u64_), V(p), V(u64_), V(p), V(u8_), V(u64_), V(u64_)],
                outcome,
            ),
            HostOp::TlsServerName => (vec![V(u64_)], vec![p, u8_, Ty::Scalar(Scalar::U32), u64_]),
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
            HostOp::SecretDeriveDigest => (
                vec![
                    V(u64_),
                    V(Ty::Scalar(Scalar::U32)),
                    V(p),
                    V(Ty::Scalar(Scalar::U32)),
                    V(p),
                    V(p),
                ],
                outcome,
            ),
            HostOp::SecretDeriveHkdf => (vec![V(u64_), V(p), V(p), V(u64_), V(p)], outcome),
            HostOp::SecretDeriveArgon2id => {
                let w = V(Ty::Scalar(Scalar::U32));
                (vec![V(u64_), V(p), w, w, w, V(u64_), V(p)], outcome)
            }
            HostOp::SecretHmac
            | HostOp::SecretHmacSha1
            | HostOp::SecretVerifyHmacSha1
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
            HostOp::ProvideStdout | HostOp::ProvideStderr => (vec![V(u64_), V(u64_)], outcome),
            HostOp::StreamTerminal | HostOp::StreamSize | HostOp::ExecSignal => {
                (vec![V(u64_), V(u8_)], outcome)
            }
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
            HostOp::ScheduleQuicInitial => (vec![V(u64_), V(p)], outcome),
            HostOp::ScheduleImportTraffic => (vec![V(u64_); 3], outcome),
            HostOp::ScheduleTls13 => (
                vec![V(u64_), V(u64_), V(u64_), V(u64_), V(p), V(p)],
                outcome,
            ),
            HostOp::ScheduleApplication => (vec![V(u64_), V(p)], outcome),
            HostOp::ScheduleKeyUpdate => (vec![V(u64_), V(Ty::Scalar(Scalar::Bool))], outcome),
            HostOp::ScheduleRetire => (vec![V(u64_); 2], outcome),
            HostOp::ScheduleImportSrtp
            | HostOp::ScheduleSrtp
            | HostOp::ScheduleImportSframe
            | HostOp::ScheduleSframeRatchet => (vec![V(u64_); 2], outcome),
            HostOp::ScheduleSframeOpener => (vec![V(u64_); 3], outcome),
            HostOp::ScheduleSframeSealer => (vec![V(u64_); 4], outcome),
            HostOp::ScheduleFinished | HostOp::ScheduleVerifyFinished => {
                (vec![V(u64_), V(u64_), V(p), V(p)], outcome)
            }
            HostOp::ScheduleDtls12 => (
                vec![
                    V(u64_),
                    V(u64_),
                    V(Ty::Scalar(Scalar::U16)),
                    V(u64_),
                    V(p),
                    V(p),
                    V(p),
                    V(p),
                ],
                outcome,
            ),
            HostOp::ScheduleSealer => (vec![V(u64_), V(u64_), V(u64_), V(u64_)], outcome),
            HostOp::ScheduleSealerPair => (vec![V(u64_); 6], outcome),
            HostOp::ScheduleNextOpener => (vec![V(u64_)], outcome),
            HostOp::ScheduleOpener | HostOp::ScheduleSpaceInfo => (vec![V(u64_); 2], outcome),
            HostOp::ScheduleHeaderMask => {
                (vec![V(u64_), V(u64_), V(Ty::Scalar(Scalar::Bool))], outcome)
            }
            HostOp::PacketSeal => (
                vec![V(u64_), V(u64_), V(p), V(u64_), V(u64_), V(p)],
                outcome,
            ),
            HostOp::PacketSealInto => (
                vec![V(u64_), V(u64_), V(p), V(u64_), V(p), V(u64_), V(p)],
                outcome,
            ),
            HostOp::PacketOpen => (
                vec![V(u64_), V(u64_), V(u64_), V(p), V(u64_), V(u64_), V(p)],
                outcome,
            ),
            HostOp::PacketOpenInto => (
                vec![
                    V(u64_),
                    V(u64_),
                    V(u64_),
                    V(p),
                    V(u64_),
                    V(u64_),
                    V(p),
                    V(p),
                ],
                outcome,
            ),
            HostOp::PacketMask => (vec![V(u64_), V(p), V(u64_), V(p)], outcome),
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
            HostOp::StdinRead | HostOp::StdinSecret => {
                (vec![V(u64_), V(p), V(u8_), V(u64_), V(u64_)], outcome)
            }
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
            | HostOp::TryLockShared
            | HostOp::Unlock
            | HostOp::Sync
            | HostOp::SyncDir
            | HostOp::ZeroExtension => (vec![V(u64_)], outcome),
            HostOp::ZeroAt => (vec![V(u64_), V(u64_), V(u64_)], outcome),
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
    /// C entry of a host operation that has its own adapter.
    pub const fn is_media(self) -> bool {
        matches!(
            self,
            HostOp::ScheduleQuicInitial
                | HostOp::ScheduleImportTraffic
                | HostOp::ScheduleTls13
                | HostOp::ScheduleApplication
                | HostOp::ScheduleKeyUpdate
                | HostOp::ScheduleRetire
                | HostOp::ScheduleFinished
                | HostOp::ScheduleVerifyFinished
                | HostOp::ScheduleDtls12
                | HostOp::ScheduleImportSrtp
                | HostOp::ScheduleSrtp
                | HostOp::ScheduleSpaceInfo
                | HostOp::ScheduleSealerPair
                | HostOp::ScheduleImportSframe
                | HostOp::ScheduleSframeSealer
                | HostOp::ScheduleSframeOpener
                | HostOp::ScheduleSframeRatchet
                | HostOp::ScheduleSealer
                | HostOp::ScheduleOpener
                | HostOp::ScheduleNextOpener
                | HostOp::ScheduleHeaderMask
                | HostOp::PacketSeal
                | HostOp::PacketSealInto
                | HostOp::PacketOpen
                | HostOp::PacketOpenInto
                | HostOp::PacketMask
        )
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

    pub const fn is_secret_derivation(self) -> bool {
        matches!(
            self,
            Self::SecretDeriveHmac
                | Self::SecretDeriveDigest
                | Self::SecretDeriveHkdf
                | Self::SecretDeriveArgon2id
        )
    }

    /// The secret MAC signing and verification operations, including JWS.
    pub const fn is_secret_mac(self) -> bool {
        self.secret_mac_signs()
            || matches!(
                self,
                Self::SecretVerifyHmac
                    | Self::SecretVerifyHmacSha1
                    | Self::SecretVerifyHmacSha384
                    | Self::SecretVerifyHmacSha512
                    | Self::SecretJwsHmacVerify
            )
    }

    /// Whether a Secret MAC operation returns a tag instead of a verdict.
    pub const fn secret_mac_signs(self) -> bool {
        matches!(
            self,
            Self::SecretHmac
                | Self::SecretHmacSha1
                | Self::SecretHmacSha384
                | Self::SecretHmacSha512
                | Self::SecretJwsHmacSign
        )
    }

    /// Whether the operation waits (a cancellation point, so it can fail).
    pub const fn waits(self) -> bool {
        !self.is_media()
            && !self.is_secret_derivation()
            && !matches!(
                self,
                HostOp::WallMs
                    | HostOp::MonoNs
                    | HostOp::Random
                    | HostOp::NetNarrow
                    | HostOp::NetClass
                    | HostOp::NetDenials
                    | HostOp::NetNarrowBegin
                    | HostOp::NetNarrowEnd
                    | HostOp::ExecNarrow
                    | HostOp::NetNarrowTls
                    | HostOp::NetNarrowResolve
                    | HostOp::NetNarrowUnix
                    | HostOp::SecretSeal
                    | HostOp::SecretNarrow
                    | HostOp::SecretHmac
                    | HostOp::SecretHmacSha1
                    | HostOp::SecretVerifyHmacSha1
                    | HostOp::SecretHmacSha384
                    | HostOp::SecretHmacSha512
                    | HostOp::SecretVerifyHmacSha384
                    | HostOp::SecretVerifyHmacSha512
                    | HostOp::SecretJwsHmacSign
                    | HostOp::SecretJwsHmacVerify
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
                    | HostOp::TlsServerName
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
                    | HostOp::StreamSize
            )
    }
}
