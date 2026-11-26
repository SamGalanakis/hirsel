const NATIVE: hirsel_core_layout::Target = match std::env::consts::OS.as_bytes() {
    b"macos" => hirsel_core_layout::Target::Aarch64Darwin,
    _ => hirsel_core_layout::Target::X86_64Linux,
};
