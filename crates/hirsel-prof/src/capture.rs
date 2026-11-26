//! What a capture is asked to do, on every host. Capturing itself is a host
//! mechanism: `linux` samples with perf events, and a host without a capture
//! provider refuses (`unsupported`).
use std::ffi::OsStr;
use std::path::Path;

/// The caller verifies detached DWARF against the executable before selecting it.
pub enum SymbolSource {
    EmbeddedDwarf,
    DetachedDwarf(Vec<u8>),
    SymtabOnly,
    OffsetsOnly,
}

/// The settings common to launching or attaching to a native process.
pub struct CaptureOptions<'a> {
    pub out: &'a Path,
    pub freq: u64,
    pub pmu: Option<&'a str>,
    pub stack_bytes: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemMode {
    Exact,
    Sampled,
}

impl MemMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Sampled => "sampled",
        }
    }
}

#[derive(Clone, Copy)]
pub struct ObservationOptions<'a> {
    pub path: &'a Path,
    pub tasks: bool,
    pub mem: Option<MemMode>,
}

impl<'a> ObservationOptions<'a> {
    pub(crate) const KEYS: [&str; 3] = [
        "HIRSEL_PROFILE",
        "HIRSEL_PROFILE_TASKS",
        "HIRSEL_PROFILE_MEM",
    ];

    pub(crate) fn environment(self) -> [(&'static str, Option<&'a OsStr>); 3] {
        [
            (Self::KEYS[0], Some(self.path.as_os_str())),
            (
                Self::KEYS[1],
                Some(OsStr::new(if self.tasks { "1" } else { "0" })),
            ),
            (
                Self::KEYS[2],
                self.mem.map(|mode| OsStr::new(mode.as_str())),
            ),
        ]
    }
}

pub struct Launch<'a> {
    pub executable: &'a Path,
    pub arguments: &'a [String],
    pub record: Option<&'a OsStr>,
    pub observation: Option<ObservationOptions<'a>>,
    pub source_root: &'a Path,
}
