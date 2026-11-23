use std::cell::Cell;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use hirsel_prof::{CaptureOptions, Launch, MemMode, ObservationOptions, SymbolSource};

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
    tasks: bool,
    mem: Option<MemMode>,
    replay: bool,
    replay_executable: Cell<Option<tempfile::TempDir>>,
}

impl Run {
    pub(crate) fn read(invocation: &mut Invocation) -> Result<Self> {
        let mut run = Self {
            sampling: Sampling::default(),
            source_root: invocation.positional.as_deref().unwrap_or(".").into(),
            tasks: false,
            mem: None,
            replay: invocation.command.name == "prof replay",
            replay_executable: Cell::new(None),
        };
        for (id, value) in invocation.options.extract_if(.., |(option, _)| {
            matches!(
                option,
                OptionId::ProfTasks
                    | OptionId::ProfMem
                    | OptionId::ProfOut
                    | OptionId::ProfFrequency
                    | OptionId::ProfPmu
                    | OptionId::ProfStackBytes
            )
        }) {
            match id {
                OptionId::ProfTasks => run.tasks = true,
                OptionId::ProfMem => {
                    run.mem = Some(match value.as_deref() {
                        Some("exact") => MemMode::Exact,
                        Some("sampled") => MemMode::Sampled,
                        _ => return Err("--mem requires exact or sampled".into()),
                    });
                }
                _ => run
                    .sampling
                    .apply(id, value.expect("profile option takes a value"))?,
            }
        }
        if run.replay {
            invocation.positional.get_or_insert_with(|| ".".into());
            if !invocation
                .options
                .iter()
                .any(|(id, _)| *id == OptionId::Output)
            {
                let scratch = tempfile::tempdir()?;
                let executable = scratch.path().join("program");
                let executable = executable
                    .to_str()
                    .ok_or("temporary executable path is not UTF-8")?;
                invocation
                    .options
                    .push((OptionId::Output, Some(executable.to_owned())));
                run.replay_executable.set(Some(scratch));
            }
            invocation.command = crate::cli::named("replay")?;
        } else {
            invocation.command = crate::cli::named("run")?;
        }
        Ok(run)
    }

    pub(crate) fn cleanup(&self) {
        drop(self.replay_executable.take());
    }

    pub(crate) fn observes(&self) -> bool {
        self.tasks || self.replay || self.mem.is_some()
    }

    pub(crate) fn capture(
        &self,
        executable: &Path,
        arguments: &[String],
        record: Option<&str>,
    ) -> Result<ExitStatus> {
        let raw = (self.tasks || self.mem.is_some())
            .then(tempfile::NamedTempFile::new)
            .transpose()?;
        let launch = Launch {
            executable,
            arguments,
            record: record.map(OsStr::new),
            observation: raw.as_ref().map(|file| ObservationOptions {
                path: file.path(),
                tasks: self.tasks,
                mem: self.mem,
            }),
            source_root: &self.source_root,
        };
        let resolve = |path: &Path, image: &[u8]| symbol_source(path, image, None);
        let status = if self.replay {
            hirsel_prof::replay(&launch, &self.sampling.out, resolve)?
        } else {
            hirsel_prof::run(&launch, &self.sampling.options(), resolve)?
        };
        if !self.replay || status.success() {
            eprintln!("hirsel: wrote {}", self.sampling.out.display());
        }
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

pub(crate) fn query(invocation: Invocation) -> Result<()> {
    use hirsel_prof::{By, Group, Query};

    let command = invocation.command.name;
    let path = invocation
        .positional
        .as_deref()
        .ok_or("prof query requires a profile file")?;
    let operand = || {
        invocation
            .arguments
            .first()
            .map(String::as_str)
            .ok_or("prof query is missing its second argument")
    };
    let mut by = By::SelfCost;
    let mut group = Group::DefPath;
    let mut event = None;
    let mut filter = None;
    let mut limit = if command == "prof sql" { 500 } else { 20 };
    let mut depth = 5;
    let mut min_delta_pct = 1.0;
    let mut timeout = Duration::from_millis(5000);
    let mut json = false;
    for (id, value) in invocation.options {
        let arg = || value.as_deref().expect("profile option takes a value");
        match id {
            OptionId::Json => json = true,
            OptionId::ProfBy => {
                by = match arg() {
                    "self" => By::SelfCost,
                    "inclusive" => By::Inclusive,
                    _ => return Err("--by requires self or inclusive".into()),
                }
            }
            OptionId::ProfGroup => {
                group = match arg() {
                    "def_path" => Group::DefPath,
                    "instance" => Group::Instance,
                    _ => return Err("--group requires def_path or instance".into()),
                }
            }
            OptionId::ProfEvent => event = value,
            OptionId::ProfFilter => filter = value,
            OptionId::ProfRows => limit = arg().parse()?,
            OptionId::ProfDepth => depth = arg().parse()?,
            OptionId::ProfMinDeltaPct => min_delta_pct = arg().parse()?,
            OptionId::ProfTimeout => timeout = Duration::from_millis(arg().parse()?),
            _ => unreachable!(),
        }
    }
    let request = match command {
        "prof summary" => Query::Summary,
        "prof tail" => Query::Tail,
        "prof schema" => Query::Schema,
        "prof top" => Query::Top {
            by,
            event: event.as_deref(),
            group,
            filter: filter.as_deref(),
        },
        "prof annotate" => Query::Annotate { target: operand()? },
        "prof peak-mem" => Query::PeakMem,
        "prof callers" => Query::Callers {
            target: operand()?,
            depth,
        },
        "prof callees" => Query::Callees {
            target: operand()?,
            depth,
        },
        "prof diff" => Query::Diff {
            head: Path::new(operand()?),
            by,
            group,
            min_delta_pct,
        },
        "prof sql" => Query::Sql {
            statement: operand()?,
            timeout,
        },
        _ => unreachable!("profile command catalog"),
    };
    let report = hirsel_prof::query(Path::new(path), request, limit)?;
    if json {
        println!(
            "{}",
            serde_json::json!({"quality": report.quality, "result": report.result})
        );
    } else {
        for (key, value) in report.quality.as_object().expect("quality object") {
            println!("{key} = {value}");
        }
        println!();
        println!("{:#}", report.result);
    }
    Ok(())
}

pub(crate) fn export(invocation: Invocation) -> Result<()> {
    let input = invocation
        .positional
        .as_deref()
        .ok_or("prof export requires a .hprof file")?;
    let mut format = None;
    let mut out = None;
    for (id, value) in invocation.options {
        match id {
            OptionId::ProfFormat => format = value,
            OptionId::ProfOut => out = value,
            _ => unreachable!("profile export option"),
        }
    }
    if format.as_deref() != Some("pprof") {
        return Err("prof export requires --format pprof".into());
    }
    let out = out.as_deref().ok_or("prof export requires --out PATH")?;
    hirsel_prof::export_pprof(Path::new(input), Path::new(out))
}
