/// AArch64 Linux images run under qemu and its sysroot; x86-64 ones natively, Memcheck included.
const EMULATED: bool = match NATIVE {
    hirsel_core_layout::Target::Aarch64Linux => true,
    hirsel_core_layout::Target::X86_64Linux
    | hirsel_core_layout::Target::Aarch64Darwin
    | hirsel_core_layout::Target::Aarch64Ios
    | hirsel_core_layout::Target::Aarch64IosSim => false,
};
const RUNS_MACHO: bool = match NATIVE.object_format() {
    hirsel_core_layout::ObjectFormat::MachO => true,
    hirsel_core_layout::ObjectFormat::Elf => false,
};
const MEMCHECK_SUPPORTED: bool = match NATIVE {
    hirsel_core_layout::Target::X86_64Linux => true,
    hirsel_core_layout::Target::Aarch64Linux
    | hirsel_core_layout::Target::Aarch64Darwin
    | hirsel_core_layout::Target::Aarch64Ios
    | hirsel_core_layout::Target::Aarch64IosSim => false,
};
fn c_compiler() -> PathBuf {
    if EMULATED {
        std::env::var_os("HIRSEL_AARCH64_CC").map(PathBuf::from).unwrap_or_else(|| input_path("HIRSEL_TOOLCHAIN_ARM").join("bin/aarch64-none-linux-gnu-gcc"))
    } else {
        PathBuf::from(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
    }
}
