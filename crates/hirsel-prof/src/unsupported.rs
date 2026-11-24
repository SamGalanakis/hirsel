//! Capture needs Linux perf events, procfs and ptrace. Elsewhere the same
//! interface refuses every capture; recorded profiles still load and query.
use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::ExitStatus;
use std::time::Duration;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const UNSUPPORTED: &str = "hirsel prof captures with Linux perf events only";

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

#[derive(Clone, Copy)]
pub struct ObservationOptions<'a> {
    pub path: &'a Path,
    pub tasks: bool,
    pub mem: Option<MemMode>,
}

pub struct Launch<'a> {
    pub executable: &'a Path,
    pub arguments: &'a [String],
    pub record: Option<&'a OsStr>,
    pub observation: Option<ObservationOptions<'a>>,
    pub source_root: &'a Path,
}

pub enum SymbolSource {
    EmbeddedDwarf,
    DetachedDwarf(Vec<u8>),
    SymtabOnly,
    OffsetsOnly,
}

pub fn run(
    _launch: &Launch<'_>,
    _options: &CaptureOptions<'_>,
    _resolve: impl FnOnce(&Path, &[u8]) -> io::Result<SymbolSource>,
) -> Result<ExitStatus> {
    Err(UNSUPPORTED.into())
}

pub fn replay(
    _launch: &Launch<'_>,
    _out: &Path,
    _resolve: impl FnOnce(&Path, &[u8]) -> io::Result<SymbolSource>,
) -> Result<ExitStatus> {
    Err(UNSUPPORTED.into())
}

pub fn attach(
    _pid: u32,
    _duration: Option<Duration>,
    _options: &CaptureOptions<'_>,
    _resolve: impl FnOnce(&Path, &[u8]) -> io::Result<SymbolSource>,
) -> Result<()> {
    Err(UNSUPPORTED.into())
}
