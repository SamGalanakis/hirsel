use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use hirsel_prof::{CaptureOptions, Launch, SymbolSource};

use crate::cli::{Invocation, OptionId};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(crate) struct Run {
    out: PathBuf,
    freq: u64,
    pmu: Option<String>,
    stack_bytes: u32,
    source_root: PathBuf,
}

impl Run {
    pub(crate) fn read(invocation: &mut Invocation) -> Result<Self> {
        let mut run = Self {
            out: "profile.hprof".into(),
            freq: 997,
            pmu: None,
            stack_bytes: 32768,
            source_root: invocation.positional.as_deref().unwrap_or(".").into(),
        };
        for (id, value) in invocation.options.extract_if(.., |(option, _)| {
            matches!(
                option,
                OptionId::ProfOut
                    | OptionId::ProfFrequency
                    | OptionId::ProfPmu
                    | OptionId::ProfStackBytes
            )
        }) {
            let value = value.expect("profile option takes a value");
            match id {
                OptionId::ProfOut => run.out = value.into(),
                OptionId::ProfFrequency => run.freq = value.parse()?,
                OptionId::ProfPmu => run.pmu = Some(value),
                OptionId::ProfStackBytes => run.stack_bytes = value.parse()?,
                _ => unreachable!(),
            }
        }
        invocation.command = crate::cli::named("run")?;
        Ok(run)
    }

    pub(crate) fn capture(
        &self,
        executable: &Path,
        arguments: &[String],
        record: Option<&str>,
    ) -> Result<ExitStatus> {
        let status = hirsel_prof::run(
            &Launch {
                executable,
                arguments,
                record: record.map(OsStr::new),
                source_root: &self.source_root,
            },
            &CaptureOptions {
                out: &self.out,
                freq: self.freq,
                pmu: self.pmu.as_deref(),
                stack_bytes: self.stack_bytes,
            },
            |path, image| symbol_source(path, image, None),
        )?;
        eprintln!("hirsel: wrote {}", self.out.display());
        Ok(status)
    }
}

pub(crate) fn symbol_source(
    path: &Path,
    image: &[u8],
    debug_path: Option<&Path>,
) -> io::Result<SymbolSource> {
    use crate::native_debug::SymbolSource as Resolved;
    let symbols = crate::native_debug::resolve_symbols(path, image, debug_path)?;
    Ok(match symbols {
        Resolved::EmbeddedDwarf => SymbolSource::EmbeddedDwarf,
        Resolved::DetachedDwarf(bytes) => SymbolSource::DetachedDwarf(bytes),
        Resolved::SymtabOnly => SymbolSource::SymtabOnly,
        Resolved::OffsetsOnly => SymbolSource::OffsetsOnly,
    })
}
