//! `--timings[=json]` and `--mem-usage` (docs/building.md, "Compile timings").
use hirsel_intern::{memory, timings};
use std::ffi::OsString;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Format {
    Table,
    Json,
}

/// What the command measures; both reports go to stderr after it finishes.
#[derive(Debug, Default)]
pub struct Measure {
    timings: Option<Format>,
    memory: bool,
    query_profile: Option<std::path::PathBuf>,
}

/// The commands that compile: check, build, run, test and precompute.
const COMMANDS: [&str; 5] = ["check", "build", "run", "test", "precompute"];

impl Measure {
    /// Take `--timings`, `--timings=json` and `--mem-usage` from anywhere
    /// before a `--` separator, as `--verbose` is taken.
    pub fn take(args: &mut Vec<OsString>) -> Result<Self, String> {
        let mut query_profile = None;
        let mut index = 0;
        while index < args.len() && args[index] != "--" {
            if args[index] == "--query-profile" {
                if query_profile.is_some() {
                    return Err("duplicate --query-profile".into());
                }
                if index + 1 == args.len() || args[index + 1] == "--" {
                    return Err("--query-profile needs a file path".into());
                }
                query_profile = Some(std::path::PathBuf::from(args.remove(index + 1)));
                args.remove(index);
            } else {
                index += 1;
            }
        }
        let end = args.iter().position(|a| a == "--").unwrap_or(args.len());
        let mut measure = Measure {
            query_profile,
            ..Measure::default()
        };
        let mut index = 0;
        let mut error = None;
        args.retain(|arg| {
            index += 1;
            if index > end {
                return true;
            }
            match arg.to_str() {
                Some("--timings") => measure.timings = Some(Format::Table),
                Some("--timings=json") => measure.timings = Some(Format::Json),
                Some("--mem-usage") => measure.memory = true,
                Some(other) if other.starts_with("--timings=") => {
                    error = Some(format!(
                        "invalid option '{other}': expected --timings or --timings=json"
                    ));
                }
                _ => return true,
            }
            false
        });
        if let Some(error) = error {
            return Err(error);
        }
        let command = args.first().and_then(|a| a.to_str());
        if measure.query_profile.is_some()
            && !command.is_some_and(|c| matches!(c, "check" | "build"))
        {
            return Err("--query-profile applies to check and build".into());
        }
        if (measure.timings.is_some() || measure.memory)
            && !command.is_some_and(|c| COMMANDS.contains(&c))
        {
            return Err(
                "--timings and --mem-usage apply to check, build, run, test and precompute".into(),
            );
        }
        Ok(measure)
    }

    /// Start recording before the command runs.
    pub fn start(&self) -> Result<(), String> {
        if self.query_profile.is_some() {
            hirsel_engine_runtime::profile::start()?;
        }
        if self.timings.is_some() {
            timings::start_request();
        }
        if self.memory {
            memory::install(crate::allocator::count_live());
        }
        Ok(())
    }

    /// Print the reports for `command`, which took `wall`.
    pub fn finish(&self, command: &str, wall: Duration) -> Result<(), String> {
        let profile_result = self
            .query_profile
            .as_ref()
            .map(|path| {
                let capture = hirsel_engine_runtime::profile::finish().expect("profile started");
                std::fs::File::create(path)
                    .and_then(|file| capture.write_to(std::io::BufWriter::new(file)))
                    .map_err(|e| format!("query profile {}: {e}", path.display()))
            })
            .transpose();
        if self.timings.is_none() && !self.memory {
            return profile_result.map(|_| ());
        }
        let memory = self.memory.then(|| {
            hirsel_engine_store::release_shared_caches();
            Memory {
                peak_rss: peak_rss(),
                held: memory::records(),
                allocations: crate::allocator::totals(),
            }
        });
        let report = self.timings.map(|_| timings::report());
        timings::disable();
        let text = match (self.timings, report) {
            (Some(Format::Json), Some(report)) => {
                json(command, wall, &report, memory.as_ref()).to_string() + "\n"
            }
            (_, report) => {
                let mut text = String::new();
                if let Some(report) = report {
                    text += &table(command, wall, &report);
                }
                if let Some(memory) = &memory {
                    text += &memory.table();
                }
                text
            }
        };
        eprint!("{text}");
        profile_result.map(|_| ())
    }
}

struct Memory {
    peak_rss: Option<u64>,
    held: Vec<(&'static str, i64)>,
    allocations: (u64, u64, u64),
}

impl Memory {
    fn table(&self) -> String {
        let mib = |bytes: i64| format!("{:>10.1} MiB", bytes as f64 / (1 << 20) as f64);
        let mut text = String::from("hirsel mem-usage\n");
        match self.peak_rss {
            Some(bytes) => text += &format!("{:<32}{}\n", "peak RSS", mib(bytes as i64)),
            None => text += &format!("{:<32}{:>14}\n", "peak RSS", "unknown"),
        }
        text += &format!(
            "allocations: {}, reallocations: {}, cumulative requested bytes: {}\n",
            self.allocations.0, self.allocations.1, self.allocations.2
        );
        text += "held at exit, largest first\n";
        // Rows under 64 KiB appear only in the JSON form.
        for (name, bytes) in self.held.iter().filter(|(_, b)| b.abs() >= 64 << 10) {
            text += &format!("  {:<30}{}\n", name, mib(*bytes));
        }
        text
    }
}

/// The process's peak resident set (`VmHWM`), where the OS reports it.
fn peak_rss() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

fn milliseconds(duration: Duration) -> String {
    format!("{:>10.1}", duration.as_secs_f64() * 1e3)
}

fn share(part: Duration, wall: Duration) -> String {
    let percent = if wall.is_zero() {
        0.0
    } else {
        part.as_secs_f64() / wall.as_secs_f64() * 100.0
    };
    format!("{percent:>7.1}%")
}

/// The wall time no interval covers.
fn untimed(wall: Duration, report: &timings::Report) -> Duration {
    wall.saturating_sub(report.intervals.iter().map(|(_, w)| *w).sum())
}

fn table(command: &str, wall: Duration, report: &timings::Report) -> String {
    let mut text = format!(
        "hirsel timings: {command}, wall {:.1} ms\n",
        wall.as_secs_f64() * 1e3
    );
    text += &format!("{:<20}{:>10} {:>8}\n", "interval", "wall ms", "wall");
    let interval = |name: &str, time: Duration| {
        format!("{name:<20}{} {}\n", milliseconds(time), share(time, wall))
    };
    for (i, time) in &report.intervals {
        text += &interval(i.name(), *time);
    }
    text += &interval("(untimed)", untimed(wall, report));
    text += &format!(
        "{:<20}{:>10} {:>10} {:>9}\n",
        "phase", "work ms", "cpu ms", "calls"
    );
    for p in &report.phases {
        text += &format!(
            "{:<20}{} {} {:>9}\n",
            p.phase.name(),
            milliseconds(p.work),
            milliseconds(p.cpu),
            p.calls
        );
    }
    if let Some(cpu) = report.process_cpu {
        let phased: Duration = report.phases.iter().map(|p| p.cpu).sum();
        text += &format!(
            "process CPU ms: {}, untimed CPU ms: {}\n",
            milliseconds(cpu),
            milliseconds(cpu.saturating_sub(phased))
        );
    }
    let split = &report.comptime_verify;
    text += &format!(
        "{:<20}{} {} {:>9}\n",
        "  verify in comptime",
        milliseconds(split.work),
        milliseconds(split.cpu),
        split.calls
    );
    text += &format!("{:<20}{}\n", "wait", milliseconds(report.wait));
    text += "count\n";
    for (count, value) in &report.counts {
        text += &format!("  {:<18}{value:>10}\n", count.name());
    }
    text += &instructions_table(&report.instructions);
    let runner = hirsel_native::counts();
    text += &format!(
        "runner\n  {:<18}{:>10}\n  {:<18}{:>10}\n",
        "image runs", runner.images, "cached images", runner.cached
    );
    for (kind, count) in &runner.fallbacks {
        text += &format!("  fallback: {} ({count})\n", runner.reasons[kind]);
    }
    text
}

fn json(
    command: &str,
    wall: Duration,
    report: &timings::Report,
    memory: Option<&Memory>,
) -> serde_json::Value {
    let nanos = |d: Duration| d.as_nanos() as u64;
    let mut value = serde_json::json!({
        "schema": 1,
        "command": command,
        "wall_ns": nanos(wall),
        "untimed_ns": nanos(untimed(wall, report)),
        "process_cpu_ns": report.process_cpu.map(nanos),
        "untimed_cpu_ns": report.process_cpu.map(|cpu| nanos(cpu.saturating_sub(report.phases.iter().map(|p| p.cpu).sum()))),
        "intervals": report.intervals.iter().map(|(i, time)| serde_json::json!({
            "interval": i.name(),
            "wall_ns": nanos(*time),
        })).collect::<Vec<_>>(),
        "phases": report.phases.iter().map(|p| serde_json::json!({
            "phase": p.phase.name(),
            "work_ns": nanos(p.work),
            "cpu_ns": nanos(p.cpu),
            "calls": p.calls,
        })).collect::<Vec<_>>(),
        "comptime_verify": {
            "work_ns": nanos(report.comptime_verify.work),
            "cpu_ns": nanos(report.comptime_verify.cpu),
            "calls": report.comptime_verify.calls,
        },
        "wait_ns": nanos(report.wait),
        "counts": report.counts
            .iter()
            .map(|(count, value)| (count.name().to_string(), (*value).into()))
            .collect::<serde_json::Map<_, _>>(),
    });
    value["instructions"] = instructions_json(&report.instructions);
    let runner = hirsel_native::counts();
    value["runner"] = serde_json::json!({
        "image_runs": runner.images,
        "cached_images": runner.cached,
        "fallbacks": runner.fallbacks.iter().map(|(kind, count)| serde_json::json!({
            "reason": runner.reasons[kind],
            "count": count,
        })).collect::<Vec<_>>(),
    });
    if let Some(memory) = memory {
        value["memory"] = serde_json::json!({
            "peak_rss_bytes": memory.peak_rss,
            "allocations": memory.allocations.0,
            "reallocations": memory.allocations.1,
            "requested_bytes": memory.allocations.2,
            "checkpoints": memory::checkpoints().iter().map(|point| serde_json::json!({
                "name": point.name,
                "heap_request_delta_bytes": point.heap_request_delta,
                "rss_bytes": point.rss,
                "peak_rss_bytes": point.peak_rss,
            })).collect::<Vec<_>>(),
            "held": memory.held.iter().map(|(name, bytes)| serde_json::json!({
                "name": name,
                "bytes": bytes,
            })).collect::<Vec<_>>(),
        });
    }
    value
}

/// Exclusive user-space instructions per owner (diagnostic only).
fn instructions_json(report: &timings::instructions::Report) -> serde_json::Value {
    let census = |rows: &[(timings::instructions::Census, u64)]| {
        rows.iter()
            .map(|(c, n)| (c.name().to_string(), (*n).into()))
            .collect::<serde_json::Map<_, _>>()
    };
    serde_json::json!({
        "refused": report.refused,
        "owners": report.owners.iter().map(|(owner, instructions, entries)| serde_json::json!({
            "owner": owner.name(),
            "instructions": instructions,
            "entries": entries,
        })).collect::<Vec<_>>(),
        "threads_total": report.threads_total,
        "threads": report.threads,
        "reads": report.reads,
        "read_cost": report.read_cost,
        "census": census(&report.census),
        "census_overflow": report.census_overflow,
        "volumes": report.volumes
            .iter()
            .map(|(v, n)| (v.name().to_string(), (*n).into()))
            .collect::<serde_json::Map<_, _>>(),
    })
}

fn instructions_table(report: &timings::instructions::Report) -> String {
    if let Some(why) = &report.refused {
        return format!("instructions: refused ({why})\n");
    }
    if report.threads == 0 {
        return String::new();
    }
    let mut text = format!(
        "{:<36}{:>16} {:>10}\n",
        "instructions:u (exclusive)", "instructions", "entries"
    );
    for (owner, instructions, entries) in &report.owners {
        text += &format!("  {:<34}{instructions:>16} {entries:>10}\n", owner.name());
    }
    text += &format!(
        "  {:<34}{:>16} {:>10}\n",
        "(thread total)", report.threads_total, report.threads
    );
    text
}
