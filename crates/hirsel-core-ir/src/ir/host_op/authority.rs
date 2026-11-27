use super::HostOp;
use crate::release_recipe::GrantKind;

impl HostOp {
    /// Grant kinds of the authority operands (packages.md section 11).
    pub const fn authority(self) -> &'static [GrantKind] {
        match self {
            Self::SecretSign => &[GrantKind::Secret, GrantKind::Random],
            Self::SendSecret | Self::TlsAccept | Self::ScramSha256 => {
                &[GrantKind::Net, GrantKind::Secret]
            }
            Self::Open
            | Self::Read
            | Self::Write
            | Self::Close
            | Self::TryLock
            | Self::Unlock
            | Self::Mkdir
            | Self::Mkdtemp
            | Self::Remove
            | Self::RemoveTree
            | Self::Rename
            | Self::Sync
            | Self::SyncDir
            | Self::OpenDir
            | Self::Entries
            | Self::NextEntries
            | Self::Stat
            | Self::StatFile
            | Self::ReadAt
            | Self::WriteAt
            | Self::ProvideDir
            | Self::Mkstemp
            | Self::ZeroExtension
            | Self::TryLockShared
            | Self::ZeroAt => &[GrantKind::Dir],
            Self::WallMs
            | Self::MonoNs
            | Self::BootNs
            | Self::SleepUntil
            | Self::SleepUntilWall
            | Self::ProvideClock => &[GrantKind::Clock],
            Self::Stdout | Self::ProvideStdout | Self::StdoutTerminal | Self::StdoutSize => {
                &[GrantKind::Stdout]
            }
            Self::Stderr | Self::ProvideStderr | Self::StderrTerminal | Self::StderrSize => {
                &[GrantKind::Stderr]
            }
            Self::StdinRead | Self::StdinSecret | Self::StdinTerminal => &[GrantKind::Stdin],
            Self::Random
            | Self::ProvideRandom
            | Self::ProvideRandomTape
            | Self::SecretP256Key
            | Self::SecretKeyShare => &[GrantKind::Random],
            Self::Connect
            | Self::Listen
            | Self::Accept
            | Self::LocalAddr
            | Self::PeerAddr
            | Self::RecvBounded
            | Self::RecvReady
            | Self::SendPair
            | Self::NetNarrow
            | Self::NetSplit
            | Self::TlsConnect
            | Self::NetNarrowTls
            | Self::Resolve
            | Self::NetNarrowResolve
            | Self::ProvideNet
            | Self::NetJoin
            | Self::UnixConnect
            | Self::UnixListen
            | Self::UnixAccept
            | Self::DatagramOpen
            | Self::DatagramBind
            | Self::DatagramConnect
            | Self::DatagramSend
            | Self::DatagramSendTo
            | Self::DatagramRecv
            | Self::UdpOpen
            | Self::UdpBind
            | Self::UdpBindGroup
            | Self::UdpRecv
            | Self::UdpSend
            | Self::UdpLocalAddr
            | Self::NetNarrowUnix
            | Self::NetDenials
            | Self::NetNarrowBegin
            | Self::NetNarrowEnd
            | Self::TlsHello
            | Self::TlsServerName
            | Self::ScramFinalLen => &[GrantKind::Net],
            Self::CredFetch => &[GrantKind::Cred],
            Self::Stopped | Self::ProvideProcess | Self::ProcessTrigger => &[GrantKind::Process],
            Self::EnvGet | Self::EnvLen | Self::EnvName | Self::EnvNarrow => &[GrantKind::Env],
            Self::ExecSpawn
            | Self::ExecWait
            | Self::ExecSignal
            | Self::ExecNarrow
            | Self::ProvideExec => &[GrantKind::Exec],
            Self::SecretSeal
            | Self::SecretNarrow
            | Self::SecretHmac
            | Self::SecretVerifyHmac
            | Self::SecretDeriveHmac
            | Self::SecretWrap
            | Self::SecretUnwrap
            | Self::SecretAeadSeal
            | Self::SecretAeadOpen
            | Self::SecretDeriveHkdf
            | Self::SecretDeriveArgon2id
            | Self::SecretHmacSha384
            | Self::SecretHmacSha512
            | Self::SecretVerifyHmacSha384
            | Self::SecretVerifyHmacSha512
            | Self::SecretJwsHmacSign
            | Self::SecretJwsHmacVerify
            | Self::SecretHmacSha1
            | Self::SecretVerifyHmacSha1
            | Self::SecretDeriveDigest
            | Self::ScramVerify
            | Self::ScheduleImportTraffic
            | Self::ScheduleImportSrtp
            | Self::ScheduleImportSframe
            | Self::ScheduleTls13
            | Self::ScheduleDtls12 => &[GrantKind::Secret],
            Self::NetClass
            | Self::ClockAdvance
            | Self::ClockAdvanceBoot
            | Self::ClockSetWall
            | Self::ClockIdle
            | Self::PortNext
            | Self::CallWord
            | Self::CallSecret
            | Self::CallPayload
            | Self::PortReply
            | Self::PortFill
            | Self::UdpFillPackets
            | Self::CallPayloadPrefix
            | Self::ScheduleQuicInitial
            | Self::ScheduleApplication
            | Self::ScheduleKeyUpdate
            | Self::ScheduleRetire
            | Self::ScheduleFinished
            | Self::ScheduleVerifyFinished
            | Self::ScheduleSrtp
            | Self::ScheduleSpaceInfo
            | Self::ScheduleSealerPair
            | Self::ScheduleSframeSealer
            | Self::ScheduleSframeOpener
            | Self::ScheduleSframeRatchet
            | Self::ScheduleSealer
            | Self::ScheduleOpener
            | Self::ScheduleNextOpener
            | Self::ScheduleHeaderMask
            | Self::PacketSeal
            | Self::PacketSealInto
            | Self::PacketOpen
            | Self::PacketOpenInto
            | Self::PacketMask => &[],
        }
    }
}
