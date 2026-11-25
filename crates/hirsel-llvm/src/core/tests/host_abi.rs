use super::*;

pub(super) fn parameters(op: HostOp) -> &'static [&'static str] {
    match op {
        HostOp::ExecSpawn => &["u64", "ptr", "ptr", "ptr", "ptr", "ptr", "u8", "u8", "u8"],
        HostOp::ExecWait => &["u64", "u8", "u64", "u64"],
        HostOp::ExecSignal => &["u64", "u8"],
        HostOp::ExecNarrow => &["u64", "u32", "ptr", "ptr"],
        HostOp::Connect => &["u64", "u8", "u64", "u64", "u16", "u8", "u64", "u64"],
        HostOp::Listen => &["u64", "u8", "u64", "u64", "u16", "u16"],
        HostOp::Accept => &["u64", "u8", "u64", "u64"],
        HostOp::LocalAddr
        | HostOp::PeerAddr
        | HostOp::NetDenials
        | HostOp::NetNarrowBegin
        | HostOp::NetNarrowEnd => &["u64"],
        HostOp::NetClass => &["u8", "u64", "u64"],
        HostOp::RecvBounded => &["u64", "ptr", "u64", "u8", "u64", "u64"],
        HostOp::SendPair => &["u64", "ptr", "ptr", "u8", "u64", "u64"],
        HostOp::NetNarrow => &["u64", "u8", "u8", "u64", "u64", "u8", "u8", "u16", "u16"],
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
        HostOp::UdpOpen => &["u64", "u8", "u32", "u32"],
        HostOp::UdpBind => &["u64", "u8", "u64", "u64", "u16", "u32", "u32"],
        HostOp::UdpBindGroup => &[
            "u64", "u8", "u64", "u64", "u16", "u32", "u32", "u16", "u32", "ptr",
        ],
        HostOp::UdpRecv | HostOp::UdpSend => &["u64", "ptr", "ptr", "u8", "u64", "u64"],
        HostOp::UdpLocalAddr => &["u64"],
        HostOp::NetNarrowUnix => &["u64", "u8", "ptr", "ptr"],
        HostOp::Resolve | HostOp::CredFetch => &["u64", "ptr", "ptr", "u8", "u64", "u64"],
        HostOp::SendSecret => &["u64", "ptr", "ptr", "ptr", "u8", "u64", "u64"],
        HostOp::ScramSha256 => &["u64", "u64", "ptr", "ptr", "ptr", "ptr", "u8", "u64", "u64"],
        HostOp::ScramFinalLen => &["u64", "ptr", "ptr"],
        HostOp::ScramVerify => &["u64", "ptr"],
        HostOp::SecretSeal => &["ptr", "ptr"],
        HostOp::SecretNarrow => &["u64", "ptr"],
        HostOp::SecretDeriveHmac => &["u64", "ptr", "ptr", "ptr"],
        HostOp::SecretDeriveDigest => &["u64", "u32", "ptr", "u32", "ptr", "ptr"],
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
        HostOp::PortFill | HostOp::UdpFillPackets => &["u64", "u64", "ptr"],
        HostOp::ProvideClock | HostOp::ClockAdvance | HostOp::ClockSetWall => &["u64", "u64"],
        HostOp::StdinRead => &["u64", "ptr", "u8", "u64", "u64"],
        HostOp::EnvName => &["u64", "u64"],
        HostOp::Open => &["u64", "ptr", "u8"],
        HostOp::Close | HostOp::TryLock | HostOp::Unlock | HostOp::WallMs | HostOp::MonoNs => {
            &["u64"]
        }
        HostOp::Rename
        | HostOp::SecretHmac
        | HostOp::SecretHmacSha1
        | HostOp::SecretVerifyHmacSha1
        | HostOp::SecretHmacSha384
        | HostOp::SecretHmacSha512
        | HostOp::SecretVerifyHmacSha384
        | HostOp::SecretVerifyHmacSha512
        | HostOp::SecretVerifyHmac => &["u64", "ptr", "ptr"],
        HostOp::ReadAt | HostOp::WriteAt | HostOp::SecretWrap | HostOp::SecretKeyShare => {
            &["u64", "u64", "ptr"]
        }
        HostOp::SecretJwsHmacSign | HostOp::SecretJwsHmacVerify => &["u64", "ptr", "ptr", "u64"],
        HostOp::SecretUnwrap => &["u64", "ptr", "ptr"],
        HostOp::SecretSign => &["u64", "u64", "u64", "ptr", "ptr"],
        HostOp::SecretP256Key => &["u64", "ptr", "ptr"],
        HostOp::ScheduleQuicInitial => &["u64", "ptr"],
        HostOp::ScheduleImportTraffic => &["u64"; 3],
        HostOp::ScheduleTls13 => &["u64", "u64", "u64", "u64", "ptr", "ptr"],
        HostOp::ScheduleApplication => &["u64", "ptr"],
        HostOp::ScheduleKeyUpdate => &["u64", "bool"],
        HostOp::ScheduleRetire => &["u64", "u64"],
        HostOp::ScheduleFinished => &["u64", "u64", "ptr", "ptr"],
        HostOp::ScheduleVerifyFinished => &["u64", "u64", "ptr", "ptr"],
        HostOp::ScheduleDtls12 => &["u64", "u64", "u16", "u64", "ptr", "ptr", "ptr", "ptr"],
        HostOp::ScheduleImportSrtp => &["u64", "u64"],
        HostOp::ScheduleSrtp => &["u64", "u64"],
        HostOp::ScheduleSealer => &["u64", "u64", "u64", "u64"],
        HostOp::ScheduleOpener => &["u64", "u64"],
        HostOp::ScheduleHeaderMask => &["u64", "u64", "bool"],
        HostOp::PacketSeal => &["u64", "u64", "ptr", "u64", "u64", "ptr"],
        HostOp::PacketSealInto => &["u64", "u64", "ptr", "u64", "ptr", "u64", "ptr"],
        HostOp::PacketOpen => &["u64", "u64", "u64", "ptr", "u64", "u64", "ptr"],
        HostOp::PacketOpenInto => &["u64", "u64", "u64", "ptr", "u64", "u64", "ptr", "ptr"],
        HostOp::PacketMask => &["u64", "ptr", "u64", "ptr"],
        HostOp::SecretAeadSeal | HostOp::SecretAeadOpen => {
            &["u64", "u64", "ptr", "ptr", "ptr", "ptr"]
        }
        HostOp::SleepUntil | HostOp::SleepUntilWall => &["u64", "u64"],
        HostOp::Sync | HostOp::SyncDir | HostOp::ZeroExtension | HostOp::StatFile => &["u64"],
        _ => &["u64", "ptr"],
    }
}

/// The C-boundary poison law (`c_boundary_poison.rs`) under one seed.
fn check_c_boundary_poison(target: Target, seed: u64) {
    let law = super::c_boundary_poison::law(seed);
    let module = text::parse(&law.core).unwrap_or_else(|e| panic!("{e:?}\n{}", law.core));
    wf::module(&module).unwrap_or_else(|e| panic!("{e:?}\n{}", law.core));
    let mut source = law.c.clone();
    for i in 0..module.functions.len() {
        source = source.replace(&format!("@F{i}@"), &symbol(&module, FuncId(i as u32)));
    }
    source.push_str(&module.site_bindings());
    for optimization in [Optimization::None, Optimization::Speed] {
        let scratch = host_memory::NativeArtifacts::new();
        let dir = scratch.path();
        std::fs::write(
            dir.join("core.o"),
            module.object(target, optimization, false),
        )
        .unwrap();
        std::fs::write(dir.join("test.c"), &source).unwrap();
        let mut compiler = c_linker(target, false);
        for symbol in super::c_boundary_poison::wraps(&source) {
            compiler.arg(format!("-Wl,--wrap={symbol}"));
        }
        output(
            link_runtime(compiler.arg(dir.join("test.c")).arg(dir.join("core.o")))
                .arg("-o")
                .arg(dir.join("test")),
        );
        let run = c_executable(target, false, &dir.join("test"))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let failures = super::c_boundary_poison::failures(&law, &stdout);
        assert!(
            failures.is_empty(),
            "{target:?} seed {seed:#x} {optimization:?}: {} of {} cells failed ({:?})\n{}",
            failures.len(),
            law.cells,
            run.status,
            failures.join("\n")
        );
    }
}

/// Poisoned upper register and stack-slot bits must not change a value
/// crossing the C boundary in either direction (`c_boundary_poison.rs`).
#[test]
fn c_boundary_poison_bits() {
    check_c_boundary_poison(Target::X86_64Linux, 0x2496_c0de_5eed);
}

#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_c_boundary_poison_bits() {
    check_c_boundary_poison(Target::Aarch64Linux, 0x2496_c0de_5eed);
}
