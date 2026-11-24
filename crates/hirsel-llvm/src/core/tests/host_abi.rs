use super::HostOp;

pub(super) fn parameters(op: HostOp) -> &'static [&'static str] {
    match op {
        HostOp::ExecSpawn => &["u64", "ptr", "ptr", "ptr", "ptr", "ptr", "u8", "u8", "u8"],
        HostOp::ExecWait => &["u64", "u8", "u64", "u64"],
        HostOp::ExecSignal => &["u64", "u8"],
        HostOp::ExecNarrow => &["u64", "u32", "ptr", "ptr"],
        HostOp::Connect => &["u64", "u8", "u64", "u64", "u16", "u8", "u64", "u64"],
        HostOp::Listen => &["u64", "u8", "u64", "u64", "u16", "u16"],
        HostOp::Accept => &["u64", "u8", "u64", "u64"],
        HostOp::LocalAddr | HostOp::PeerAddr => &["u64"],
        HostOp::RecvBounded => &["u64", "ptr", "u64", "u8", "u64", "u64"],
        HostOp::SendPair => &["u64", "ptr", "ptr", "u8", "u64", "u64"],
        HostOp::NetNarrow => &["u64", "u8", "u8", "u64", "u64", "u8", "u16", "u16"],
        HostOp::TlsConnect => &["u64", "u64", "ptr", "u8", "ptr", "ptr", "u8", "u64", "u64"],
        HostOp::NetNarrowTls => &["u64", "ptr", "u16", "u16", "u8", "ptr"],
        HostOp::NetNarrowResolve => &["u64", "ptr"],
        HostOp::UnixConnect => &["u64", "ptr", "ptr", "u8", "u64", "u64"],
        HostOp::UnixListen => &["u64", "ptr", "ptr", "u16"],
        HostOp::UnixAccept | HostOp::RecvReady => &["u64", "u8", "u64", "u64"],
        HostOp::DatagramOpen => &["u64"],
        HostOp::DatagramBind => &["u64", "ptr", "ptr"],
        HostOp::DatagramConnect => &["u64", "ptr", "ptr"],
        HostOp::DatagramSend => &["u64", "ptr", "u8", "u64", "u64"],
        HostOp::DatagramSendTo => &["u64", "ptr", "ptr", "ptr", "u8", "u64", "u64"],
        HostOp::DatagramRecv => &["u64", "ptr", "u8", "u64", "u64"],
        HostOp::NetNarrowUnix => &["u64", "u8", "ptr", "ptr"],
        HostOp::Resolve | HostOp::CredFetch => &["u64", "ptr", "ptr", "u8", "u64", "u64"],
        HostOp::SendSecret => &["u64", "ptr", "ptr", "ptr", "u8", "u64", "u64"],
        HostOp::ScramSha256 => &["u64", "u64", "ptr", "ptr", "ptr", "ptr", "u8", "u64", "u64"],
        HostOp::ScramFinalLen => &["u64", "ptr", "ptr"],
        HostOp::ScramVerify => &["u64", "ptr"],
        HostOp::SecretSeal => &["ptr", "ptr"],
        HostOp::SecretNarrow => &["u64", "ptr"],
        HostOp::SecretDeriveHmac => &["u64", "ptr", "ptr", "ptr"],
        HostOp::SecretDeriveHkdf => &["u64", "ptr", "ptr", "u64", "ptr"],
        HostOp::SecretDeriveArgon2id => &["u64", "ptr", "u32", "u32", "u32", "u64", "ptr"],
        HostOp::NetSplit
        | HostOp::Stopped
        | HostOp::EnvLen
        | HostOp::ProvideRandom
        | HostOp::ClockIdle
        | HostOp::ProcessTrigger => &["u64"],
        HostOp::ProvideProcess => &[],
        HostOp::ProvideRandomTape => &["ptr"],
        HostOp::ProvideDir | HostOp::ProvideNet | HostOp::ProvideExec | HostOp::PortNext => {
            &["u64"]
        }
        HostOp::ProvideStdout | HostOp::ProvideStderr => &["u64", "bool"],
        HostOp::StreamTerminal => &["u64", "u8"],
        HostOp::CallWord | HostOp::CallSecret | HostOp::NetJoin => &["u64", "u64"],
        HostOp::CallPayload => &["u64", "u64", "ptr"],
        HostOp::CallPayloadPrefix => &["u64", "u64", "u64", "ptr"],
        HostOp::PortReply => &["u64"; 9],
        HostOp::PortFill => &["u64", "u64", "ptr"],
        HostOp::ProvideClock | HostOp::ClockAdvance | HostOp::ClockSetWall => &["u64", "u64"],
        HostOp::StdinRead => &["u64", "ptr", "u8", "u64", "u64"],
        HostOp::EnvName => &["u64", "u64"],
        HostOp::Open => &["u64", "ptr", "u8"],
        HostOp::Close | HostOp::TryLock | HostOp::Unlock | HostOp::WallMs | HostOp::MonoNs => {
            &["u64"]
        }
        HostOp::Rename
        | HostOp::SecretHmac
        | HostOp::SecretHmacSha384
        | HostOp::SecretHmacSha512
        | HostOp::SecretVerifyHmacSha384
        | HostOp::SecretVerifyHmacSha512
        | HostOp::SecretVerifyHmac => &["u64", "ptr", "ptr"],
        HostOp::ReadAt | HostOp::WriteAt | HostOp::SecretWrap => &["u64", "u64", "ptr"],
        HostOp::SecretJwsHmacSign | HostOp::SecretJwsHmacVerify => &["u64", "ptr", "ptr", "u64"],
        HostOp::SecretUnwrap => &["u64", "ptr", "ptr"],
        HostOp::SecretSign => &["u64", "u64", "ptr", "ptr"],
        HostOp::SecretAeadSeal | HostOp::SecretAeadOpen => {
            &["u64", "u64", "ptr", "ptr", "ptr", "ptr"]
        }
        HostOp::SleepUntil | HostOp::SleepUntilWall => &["u64", "u64"],
        HostOp::Sync | HostOp::SyncDir | HostOp::ZeroExtension | HostOp::StatFile => &["u64"],
        _ => &["u64", "ptr"],
    }
}
