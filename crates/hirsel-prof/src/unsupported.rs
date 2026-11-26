//! Capture on a host with no capture provider: every capture is refused by
//! name. Reading, querying and exporting a profile need no provider. Darwin's
//! provider (an in-process sampler with whole-run counters) is macOS slice S13.
use std::io;
use std::path::Path;
use std::process::ExitStatus;
use std::time::Duration;

use crate::capture::{CaptureOptions, Launch, SymbolSource};
use crate::profile::Result;

const REFUSED: &str = "hirsel prof cannot capture on this host: sampling needs Linux perf events, and the macOS sampler is not built";

pub fn run(
    _launch: &Launch<'_>,
    _options: &CaptureOptions<'_>,
    _resolve: impl FnOnce(&Path, &[u8]) -> io::Result<SymbolSource>,
) -> Result<ExitStatus> {
    Err(REFUSED.into())
}

pub fn replay(
    _launch: &Launch<'_>,
    _out: &Path,
    _resolve: impl FnOnce(&Path, &[u8]) -> io::Result<SymbolSource>,
) -> Result<ExitStatus> {
    Err(REFUSED.into())
}

pub fn attach(
    _pid: u32,
    _duration: Option<Duration>,
    _options: &CaptureOptions<'_>,
    _resolve: impl FnOnce(&Path, &[u8]) -> io::Result<SymbolSource>,
) -> Result<()> {
    Err(REFUSED.into())
}
