use std::cell::Cell;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use hirsel_prof::{CaptureOptions, Launch, MemMode, ObservationOptions, SymbolSource};

use crate::cli::{AttachOption, CompilerOption, ProfileExportOption, ProfileQueryOption, Request};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Sampling {
    out: PathBuf,
    freq: u64,
    pmu: Option<String>,
    stack_bytes: Option<u32>,
}

impl Default for Sampling {
    fn default() -> Self {
        Self {
            out: "profile.hprof".into(),
            freq: 997,
            pmu: None,
            stack_bytes: None,
        }
    }
}

impl Sampling {
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
    pub(crate) fn read(invocation: &mut Request<CompilerOption>) -> Result<Self> {
        let mut run = Self {
            sampling: Sampling::default(),
            source_root: invocation.positional.as_deref().unwrap_or(".").into(),
            tasks: false,
            mem: None,
            replay: invocation.command.name == "prof replay",
            replay_executable: Cell::new(None),
        };
        use CompilerOption::*;
        for option in invocation.options.extract_if(.., |option| {
            matches!(
                option,
                ProfTasks
                    | ProfMem(_)
                    | ProfOut(_)
                    | ProfFrequency(_)
                    | ProfPmu(_)
                    | ProfStackBytes(_)
            )
        }) {
            match option {
                ProfTasks => run.tasks = true,
                ProfMem(mode) => run.mem = Some(mode),
                ProfOut(value) => run.sampling.out = value.into(),
                ProfFrequency(value) => run.sampling.freq = value.parse()?,
                ProfPmu(value) => run.sampling.pmu = Some(value),
                ProfStackBytes(value) => run.sampling.stack_bytes = Some(value.parse()?),
                _ => unreachable!("only profiling options are extracted"),
            }
        }
        if run.replay {
            invocation.positional.get_or_insert_with(|| ".".into());
            if !invocation
                .options
                .iter()
                .any(|option| matches!(option, Output(_)))
            {
                let scratch = tempfile::tempdir()?;
                let executable = scratch.path().join("program");
                let executable = executable
                    .to_str()
                    .ok_or("temporary executable path is not UTF-8")?;
                invocation.options.push(Output(executable.to_owned()));
                run.replay_executable.set(Some(scratch));
            }
            invocation.command = crate::cli::named("tape replay")?;
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
        stdio: Option<&[std::fs::File; 2]>,
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
            stdio,
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

pub(crate) fn attach(invocation: Request<AttachOption>) -> Result<()> {
    let pid = invocation
        .positional
        .as_deref()
        .ok_or("prof attach requires a PID")?
        .parse()?;
    let mut sampling = Sampling::default();
    let mut debug: Option<PathBuf> = None;
    let mut duration = None;
    for option in invocation.options {
        match option {
            AttachOption::ProfDuration(value) => {
                let parsed = Duration::try_from_secs_f64(value.parse()?)?;
                if parsed.is_zero() {
                    return Err("--duration must be positive".into());
                }
                duration = Some(parsed);
            }
            AttachOption::ProfDebug(value) => debug = Some(value.into()),
            AttachOption::ProfOut(value) => sampling.out = value.into(),
            AttachOption::ProfFrequency(value) => sampling.freq = value.parse()?,
            AttachOption::ProfPmu(value) => sampling.pmu = Some(value),
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

pub(crate) fn query(invocation: Request<ProfileQueryOption>) -> Result<()> {
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
    for option in invocation.options {
        match option {
            ProfileQueryOption::Json => json = true,
            ProfileQueryOption::ProfBy(value) => {
                by = match value.as_str() {
                    "self" => By::SelfCost,
                    "inclusive" => By::Inclusive,
                    _ => return Err("--by requires self or inclusive".into()),
                }
            }
            ProfileQueryOption::ProfGroup(value) => {
                group = match value.as_str() {
                    "def_path" => Group::DefPath,
                    "instance" => Group::Instance,
                    _ => return Err("--group requires def_path or instance".into()),
                }
            }
            ProfileQueryOption::ProfEvent(value) => event = Some(value),
            ProfileQueryOption::ProfFilter(value) => filter = Some(value),
            ProfileQueryOption::ProfRows(value) => limit = value.as_str().parse()?,
            ProfileQueryOption::ProfDepth(value) => depth = value.as_str().parse()?,
            ProfileQueryOption::ProfMinDeltaPct(value) => min_delta_pct = value.as_str().parse()?,
            ProfileQueryOption::ProfTimeout(value) => {
                timeout = Duration::from_millis(value.as_str().parse()?)
            }
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

pub(crate) fn export(invocation: Request<ProfileExportOption>) -> Result<()> {
    let input = invocation
        .positional
        .as_deref()
        .ok_or("prof export requires a .hprof file")?;
    let mut format = None;
    let mut out = None;
    for option in invocation.options {
        match option {
            ProfileExportOption::ProfFormat(value) => format = Some(value),
            ProfileExportOption::ProfOut(value) => out = Some(value),
        }
    }
    if format.as_deref() != Some("pprof") {
        return Err("prof export requires --format pprof".into());
    }
    let out = out.as_deref().ok_or("prof export requires --out PATH")?;
    hirsel_prof::export_pprof(Path::new(input), Path::new(out))
}
