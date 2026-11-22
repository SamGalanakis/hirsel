use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use hirsel_prof::{CaptureOptions, Launch, SymbolSource};

use crate::cli::{Invocation, OptionId};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Sampling {
    out: PathBuf,
    freq: u64,
    pmu: Option<String>,
    stack_bytes: u32,
}

impl Default for Sampling {
    fn default() -> Self {
        Self {
            out: "profile.hprof".into(),
            freq: 997,
            pmu: None,
            stack_bytes: 32768,
        }
    }
}

impl Sampling {
    fn apply(&mut self, id: OptionId, value: String) -> Result<()> {
        match id {
            OptionId::ProfOut => self.out = value.into(),
            OptionId::ProfFrequency => self.freq = value.parse()?,
            OptionId::ProfPmu => self.pmu = Some(value),
            OptionId::ProfStackBytes => self.stack_bytes = value.parse()?,
            _ => unreachable!(),
        }
        Ok(())
    }

    fn options(&self) -> CaptureOptions<'_> {
        CaptureOptions {
            out: &self.out,
            freq: self.freq,
            pmu: self.pmu.as_deref(),
            stack_bytes: self.stack_bytes,
        }
    }
}

pub(crate) struct Run {
    sampling: Sampling,
    source_root: PathBuf,
}

impl Run {
    pub(crate) fn read(invocation: &mut Invocation) -> Result<Self> {
        let mut run = Self {
            sampling: Sampling::default(),
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
            run.sampling
                .apply(id, value.expect("profile option takes a value"))?;
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
            &self.sampling.options(),
            |path, image| symbol_source(path, image, None),
        )?;
        eprintln!("hirsel: wrote {}", self.sampling.out.display());
        Ok(status)
    }
}

pub(crate) fn attach(invocation: Invocation) -> Result<()> {
    let pid = invocation
        .positional
        .as_deref()
        .ok_or("prof attach requires a PID")?
        .parse()?;
    let mut sampling = Sampling::default();
    let mut debug: Option<PathBuf> = None;
    let mut duration = None;
    for (id, value) in invocation.options {
        let value = value.expect("attach option takes a value");
        match id {
            OptionId::ProfDuration => {
                let parsed = Duration::try_from_secs_f64(value.parse()?)?;
                if parsed.is_zero() {
                    return Err("--duration must be positive".into());
                }
                duration = Some(parsed);
            }
            OptionId::ProfDebug => debug = Some(value.into()),
            _ => sampling.apply(id, value)?,
        }
    }
    hirsel_prof::attach(pid, duration, &sampling.options(), |path, image| {
        symbol_source(path, image, debug.as_deref())
    })?;
    eprintln!("hirsel: wrote {}", sampling.out.display());
    Ok(())
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
