//! macOS capture. A process can read only its own threads' registers, so
//! the profiling runtime samples its threads itself (`HIRSEL_PROFILE_CPU`)
//! and writes each stack, already walked by frame records, to its
//! observation stream. This imports those samples, symbolized from the
//! executable and its detached DWARF, beside the runtime's other records.
//! Attaching to a running process would need its task port, which macOS
//! grants only to entitled debuggers.
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use gimli::{AttributeValue, EndianArcSlice, LittleEndian, UnitOffset, UnitRef};
use object::{Object, ObjectSection, ObjectSegment, ObjectSymbol};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::observation::{Observation, RecordKind};
use crate::profile::{ProfileWriter, Result, RunMeta, Timestamp, Unsigned};

type Reader = EndianArcSlice<LittleEndian>;

const TARGET: &str = "aarch64-apple-darwin";
const LIVE_EXACT: &str =
    "a live memory profile is sampled; record a tape and use 'hirsel prof replay --mem=exact'";

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
    fn as_str(self) -> &'static str {
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

const KEYS: [&str; 4] = [
    "HIRSEL_PROFILE",
    "HIRSEL_PROFILE_TASKS",
    "HIRSEL_PROFILE_MEM",
    "HIRSEL_PROFILE_CPU",
];

pub struct Launch<'a> {
    pub executable: &'a Path,
    pub arguments: &'a [String],
    pub record: Option<&'a OsStr>,
    pub observation: Option<ObservationOptions<'a>>,
    pub source_root: &'a Path,
}

/// The caller verifies detached DWARF against the executable before selecting it.
pub enum SymbolSource {
    EmbeddedDwarf,
    DetachedDwarf(Vec<u8>),
    SymtabOnly,
    OffsetsOnly,
}

impl SymbolSource {
    fn tier(&self) -> &'static str {
        match self {
            Self::EmbeddedDwarf | Self::DetachedDwarf(_) => "full_dwarf",
            Self::SymtabOnly => "symtab_only",
            Self::OffsetsOnly => "offsets_only",
        }
    }
}

/// Profile a just-built executable: its runtime samples every running
/// thread `freq` times a second of wall time.
pub fn run(
    launch: &Launch<'_>,
    options: &CaptureOptions<'_>,
    resolve: impl FnOnce(&Path, &[u8]) -> std::io::Result<SymbolSource>,
) -> Result<ExitStatus> {
    if options.pmu.is_some_and(|pmu| pmu != "cpu-clock") {
        return Err("macOS samples only cpu-clock".into());
    }
    if !(1..=10_000).contains(&options.freq) {
        return Err("--freq requires 1 to 10000 samples a second".into());
    }
    capture(launch, options.out, Some(options.freq), resolve)
}

/// Launch the existing native tape-replay entry without CPU sampling.
/// Its own verifier decides the exit status and no profile is published on refusal.
pub fn replay(
    launch: &Launch<'_>,
    out: &Path,
    resolve: impl FnOnce(&Path, &[u8]) -> std::io::Result<SymbolSource>,
) -> Result<ExitStatus> {
    capture(launch, out, None, resolve)
}

/// Run the executable with its observation stream, sampling its threads at
/// `freq` when given, and publish its profile at `out`. A replay (no `freq`)
/// that fails publishes nothing.
fn capture(
    launch: &Launch<'_>,
    out: &Path,
    freq: Option<u64>,
    resolve: impl FnOnce(&Path, &[u8]) -> std::io::Result<SymbolSource>,
) -> Result<ExitStatus> {
    let mem = launch.observation.and_then(|observation| observation.mem);
    if freq.is_some() && mem == Some(MemMode::Exact) {
        return Err(LIVE_EXACT.into());
    }
    if mem.is_some() {
        return Err("macOS profiles take no memory samples".into());
    }
    let path = launch.executable.canonicalize()?;
    let bytes = std::fs::read(&path)?;
    let symbols = resolve(&path, &bytes)?;
    let tier = symbols.tier();
    let image = Image::parse(&bytes, symbols)?;
    let scratch = tempfile::NamedTempFile::new()?;
    let stream = launch
        .observation
        .map_or(scratch.path(), |observation| observation.path);
    let tasks = launch
        .observation
        .is_some_and(|observation| observation.tasks);
    let mut command = std::process::Command::new(&path);
    command.args(launch.arguments);
    for key in KEYS {
        command.env_remove(key);
    }
    command.env_remove("HIRSEL_RECORD");
    if let Some(record) = launch.record.filter(|_| freq.is_some()) {
        command.env("HIRSEL_RECORD", record);
    }
    command
        .env("HIRSEL_PROFILE", stream)
        .env("HIRSEL_PROFILE_TASKS", if tasks { "1" } else { "0" });
    if let Some(freq) = freq {
        command.env("HIRSEL_PROFILE_CPU", freq.to_string());
    }
    if let Some(mem) = mem {
        command.env("HIRSEL_PROFILE_MEM", mem.as_str());
    }
    let start = monotonic_ns()?;
    let mut child = command.spawn()?;
    let pid = child.id();
    let status = child.wait()?;
    let end = monotonic_ns()?;
    if freq.is_none() && !status.success() {
        return Ok(status);
    }
    let profile = ProfileWriter::create(out)?;
    let observations = Observation::read(stream).map_err(|error| {
        format!("this profile requires a profiling runtime that samples its threads: {error}")
    })?;
    let period = freq.map_or(0, |freq| 1_000_000_000 / freq);
    let mut import = Import::new(
        profile.connection(),
        pid,
        launch.source_root.to_owned(),
        image,
    );
    import.samples(&observations, period)?;
    import.close(end)?;
    let digest = import.image.digest.clone();
    drop(import);
    if launch.observation.is_some() {
        observations.store(profile.connection())?;
    }
    let cmdline = serde_json::to_string(
        &std::iter::once(path.to_string_lossy())
            .chain(launch.arguments.iter().map(|arg| arg.as_str().into()))
            .collect::<Vec<_>>(),
    )?;
    profile.finish(&RunMeta {
        capture_mode: if freq.is_some() { "run" } else { "replay" },
        target: TARGET,
        cmdline: &cmdline,
        build_id: "",
        image_digest: &digest,
        symbol_tier: tier,
        clock: "CLOCK_MONOTONIC",
        primary_event: if freq.is_some() {
            "cpu-clock"
        } else {
            "runtime"
        },
        primary_unit: "nanoseconds",
        freq_hz: freq.unwrap_or(0),
        fallback_reason: "",
        start_ns: start,
        duration_ns: end.checked_sub(start).ok_or("monotonic clock regressed")?,
        samples_lost: observations.lost,
    })?;
    Ok(status)
}

pub fn attach(
    _pid: u32,
    _duration: Option<Duration>,
    _options: &CaptureOptions<'_>,
    _resolve: impl FnOnce(&Path, &[u8]) -> std::io::Result<SymbolSource>,
) -> Result<()> {
    Err("hirsel prof attach needs another process's task port, which macOS grants only to entitled debuggers; profile with hirsel prof run".into())
}

fn monotonic_ns() -> std::io::Result<u64> {
    // SAFETY: zero is a valid initialization and clock_gettime fills the buffer.
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    // The runtime's clock: CLOCK_MONOTONIC counts only whole microseconds.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC_RAW, &mut ts) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64)
}

/// The executable's symbols and DWARF, at its unslid addresses.
struct Image {
    digest: String,
    /// The `__TEXT` segment's address, where a mapping's start lands.
    text: u64,
    dwarf: Option<addr2line::Context<Reader>>,
    symbols: Vec<(u64, u64, String)>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Function {
    def_path: Option<String>,
    instance: String,
    name: String,
    kind: &'static str,
}

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
struct Symbolized {
    function: Option<Function>,
    file: Option<String>,
    line: Option<u32>,
    col: Option<u32>,
    inlined: bool,
}

impl Image {
    fn parse(bytes: &[u8], source: SymbolSource) -> Result<Self> {
        let file = object::File::parse(bytes)?;
        if file.format() != object::BinaryFormat::MachO
            || file.architecture() != object::Architecture::Aarch64
        {
            return Err("profiling requires a native arm64 Mach-O image".into());
        }
        let text = file
            .segments()
            .find(|segment| segment.name().ok().flatten() == Some("__TEXT"))
            .map(|segment| segment.address())
            .ok_or("the executable has no __TEXT segment")?;
        let dwarf = match &source {
            SymbolSource::DetachedDwarf(debug) => {
                Some(dwarf(&object::File::parse(debug.as_slice())?)?)
            }
            SymbolSource::EmbeddedDwarf => Some(dwarf(&file)?),
            _ => None,
        };
        // Mach-O symbols have no size: each runs to the next.
        let mut starts: Vec<(u64, String)> = Vec::new();
        if !matches!(source, SymbolSource::OffsetsOnly) {
            for symbol in file.symbols() {
                if symbol.is_definition()
                    && symbol.kind() == object::SymbolKind::Text
                    && let Ok(name) = symbol.name()
                {
                    starts.push((
                        symbol.address(),
                        name.strip_prefix('_').unwrap_or(name).to_owned(),
                    ));
                }
            }
        }
        starts.sort();
        starts.dedup_by_key(|(address, _)| *address);
        let end = file
            .sections()
            .filter(|section| section.kind() == object::SectionKind::Text)
            .map(|section| section.address() + section.size())
            .max()
            .unwrap_or(0);
        let symbols = (0..starts.len())
            .map(|i| {
                let next = starts.get(i + 1).map_or(end, |(address, _)| *address);
                (starts[i].0, next, starts[i].1.clone())
            })
            .collect();
        Ok(Self {
            digest: hex(&Sha256::digest(bytes)),
            text,
            dwarf,
            symbols,
        })
    }

    fn physical_name(&self, address: u64) -> Option<&str> {
        let before = self
            .symbols
            .partition_point(|(start, _, _)| *start <= address);
        self.symbols[..before]
            .last()
            .filter(|(_, end, _)| address < *end)
            .map(|(_, _, name)| name.as_str())
    }

    fn frames(&self, address: u64) -> Result<Vec<Symbolized>> {
        let physical = self.physical_name(address);
        if let Some(dwarf) = &self.dwarf {
            let unit = dwarf.find_dwarf_and_unit(address).skip_all_loads();
            let hirsel_unit = unit.map(is_hirsel_unit).transpose()?.unwrap_or(false);
            let mut rows = dwarf.find_frames(address).skip_all_loads()?;
            let mut frames = Vec::new();
            while let Some(frame) = rows.next()? {
                let visible = frame.function.as_ref().map(|f| f.demangle()).transpose()?;
                let raw = frame.function.as_ref().map(|f| f.raw_name()).transpose()?;
                let def_name = match (unit, frame.dw_die_offset) {
                    (Some(unit), Some(offset)) => definition_name(unit, offset)?,
                    _ => None,
                };
                let loc = frame.location;
                let function = raw.map(|raw| {
                    let name = def_name.as_deref().or(visible.as_deref()).unwrap_or(&raw);
                    function(&raw, name, def_name.as_deref(), hirsel_unit)
                });
                frames.push(Symbolized {
                    function,
                    file: loc.as_ref().and_then(|loc| loc.file.map(str::to_owned)),
                    line: loc.as_ref().and_then(|loc| loc.line),
                    col: loc.as_ref().and_then(|loc| loc.column),
                    inlined: true,
                });
            }
            if let Some(last) = frames.last_mut() {
                last.inlined = false;
                last.function = match (physical, last.function.take()) {
                    (Some(raw), Some(mut resolved)) => {
                        resolved.instance = raw.to_owned();
                        Some(resolved)
                    }
                    (Some(raw), None) => Some(physical_function(raw)),
                    (None, resolved) => resolved,
                };
                return Ok(frames);
            }
        }
        Ok(vec![Symbolized {
            function: physical.map(physical_function),
            ..Symbolized::default()
        }])
    }
}

fn dwarf(debug: &object::File<'_>) -> Result<addr2line::Context<Reader>> {
    let dwarf = gimli::Dwarf::load(|id| -> object::Result<Reader> {
        let bytes = debug
            .section_by_name(id.name())
            .map(|s| s.data())
            .transpose()?;
        Ok(EndianArcSlice::new(
            Arc::from(bytes.unwrap_or_default()),
            LittleEndian,
        ))
    })?;
    Ok(addr2line::Context::from_dwarf(dwarf)?)
}

fn physical_function(raw: &str) -> Function {
    let name = addr2line::demangle_auto(Cow::Borrowed(raw), None);
    function(raw, &name, None, false)
}

fn function(raw: &str, name: &str, definition: Option<&str>, hirsel_unit: bool) -> Function {
    let is_hirsel = hirsel_unit
        || raw
            .strip_prefix('h')
            .and_then(|tail| tail.as_bytes().get(..64))
            .is_some_and(|digest| digest.iter().all(u8::is_ascii_hexdigit));
    let kind = if is_hirsel {
        "hirsel"
    } else if raw.starts_with("hirsel_")
        || name.contains("hirsel_core_rt::")
        || name.contains("hirsel_stack::")
    {
        "runtime"
    } else {
        "foreign"
    };
    Function {
        def_path: (kind == "hirsel")
            .then(|| definition.map(str::to_owned))
            .flatten(),
        instance: raw.to_owned(),
        name: name.to_owned(),
        kind,
    }
}

fn is_hirsel_unit(unit: UnitRef<'_, Reader>) -> Result<bool> {
    use gimli::Reader as _;
    let mut entries = unit.entries();
    let Some(root) = entries.next_dfs()? else {
        return Ok(false);
    };
    let Some(producer) = root.attr_value(gimli::DW_AT_producer) else {
        return Ok(false);
    };
    Ok(unit.attr_string(producer)?.to_string_lossy()? == "Hirsel")
}

fn definition_name(unit: UnitRef<'_, Reader>, offset: UnitOffset<usize>) -> Result<Option<String>> {
    let mut offset = offset;
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(offset) {
            return Err("DWARF definition origin contains a cycle".into());
        }
        let entry = unit.entry(offset)?;
        if let Some(name) = entry.attr_value(gimli::DW_AT_name) {
            use gimli::Reader as _;
            return Ok(Some(
                unit.attr_string(name)?.to_string_lossy()?.into_owned(),
            ));
        }
        match entry
            .attr_value(gimli::DW_AT_abstract_origin)
            .or_else(|| entry.attr_value(gimli::DW_AT_specification))
        {
            Some(AttributeValue::UnitRef(next)) => offset = next,
            _ => return Ok(None),
        }
    }
}

fn host_fiber_invocation(name: &str) -> bool {
    let name = addr2line::demangle_auto(Cow::Borrowed(name), None);
    name.starts_with("<hirsel_core_rt::fiber::Stack>::call::invoke")
        || name.starts_with("hirsel_core_rt::fiber::Stack::call::invoke")
}

fn fiber_boundary(name: &str) -> bool {
    let name = addr2line::demangle_auto(Cow::Borrowed(name), None);
    name.contains("hirsel_stack::fiber_start") || name.contains("hirsel_stack::switch")
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut result, "{byte:02x}").expect("writing to String");
    }
    result
}

#[derive(Eq, Hash, PartialEq)]
struct FrameKey {
    mapping: Option<i64>,
    rel_pc: Option<u64>,
    symbol: Symbolized,
}

struct Mapping {
    id: i64,
    start: u64,
    end: u64,
    main: bool,
}

struct Import<'a> {
    db: &'a Connection,
    pid: u32,
    source_root: PathBuf,
    image: Image,
    path: PathBuf,
    mappings: Vec<Mapping>,
    threads: HashMap<u64, (i64, u64)>,
    names: HashMap<u64, String>,
    functions: HashMap<Function, i64>,
    frames: HashMap<FrameKey, i64>,
    callsites: HashMap<(Option<i64>, i64), i64>,
    sources: HashMap<String, Option<Vec<String>>>,
    snippets: HashSet<(String, u32)>,
}

impl<'a> Import<'a> {
    fn new(db: &'a Connection, pid: u32, source_root: PathBuf, image: Image) -> Self {
        Self {
            db,
            pid,
            source_root,
            image,
            path: PathBuf::new(),
            mappings: Vec::new(),
            threads: HashMap::new(),
            names: HashMap::new(),
            functions: HashMap::new(),
            frames: HashMap::new(),
            callsites: HashMap::new(),
            sources: HashMap::new(),
            snippets: HashSet::new(),
        }
    }

    fn samples(&mut self, observations: &Observation, period: u64) -> Result<()> {
        let digest = self.image.digest.clone();
        for record in &observations.records {
            match &record.kind {
                RecordKind::Mapping(line) => self.mapping(line, record.ts, &digest)?,
                RecordKind::ThreadName { thread, name } => {
                    self.names
                        .insert(*thread, String::from_utf8_lossy(name).into_owned());
                }
                RecordKind::CpuSample { thread, stack } => {
                    self.sample(*thread, record.ts, stack, period)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// A runtime mapping line: `start-end perms offset dev inode path`.
    fn mapping(&mut self, line: &[u8], time: u64, digest: &str) -> Result<()> {
        let line = String::from_utf8_lossy(line);
        let mut fields = line.splitn(6, ' ');
        let (Some(range), Some(_), Some(offset), Some(_), Some(_), Some(path)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return Err("invalid runtime mapping record".into());
        };
        let (start, end) = range
            .split_once('-')
            .ok_or("invalid runtime mapping range")?;
        let (start, end) = (
            u64::from_str_radix(start, 16)?,
            u64::from_str_radix(end, 16)?,
        );
        let offset = u64::from_str_radix(offset, 16)?;
        // The first image dyld lists is the executable.
        let main = self.mappings.is_empty();
        if main {
            self.path = PathBuf::from(path);
        }
        self.db.execute(
            "INSERT INTO mapping(start_addr_hex,end_addr_hex,file_offset,path,build_id,image_digest,start_ns)
             VALUES (?1,?2,?3,?4,NULL,?5,?6)",
            (
                format!("{start:#x}"),
                format!("{end:#x}"),
                Unsigned(offset),
                path,
                main.then_some(digest),
                Timestamp(time),
            ),
        )?;
        self.mappings.push(Mapping {
            id: self.db.last_insert_rowid(),
            start,
            end,
            main,
        });
        Ok(())
    }

    fn thread(&mut self, thread: u64, time: u64) -> Result<i64> {
        if let Some((id, last)) = self.threads.get_mut(&thread) {
            *last = time;
            return Ok(*id);
        }
        let name = self.names.get(&thread).cloned().unwrap_or_default();
        self.db.execute(
            "INSERT INTO thread(pid,tid,name,start_ns) VALUES (?1,?2,?3,?4)",
            (self.pid, u32::try_from(thread)?, name, Timestamp(time)),
        )?;
        let id = self.db.last_insert_rowid();
        self.threads.insert(thread, (id, time));
        Ok(id)
    }

    fn sample(&mut self, thread: u64, time: u64, stack: &[u64], period: u64) -> Result<()> {
        let thread_id = self.thread(thread, time)?;
        let mut frames = Vec::new();
        let mut outermost = None;
        // A frame of the runtime's host-stack call roots the sample on the host.
        let mut host = false;
        for (depth, &address) in stack.iter().enumerate() {
            // A return address follows its call; the call is the line.
            let pc = if depth == 0 {
                address
            } else {
                address.saturating_sub(1)
            };
            let mapping = self
                .mappings
                .iter()
                .find(|mapping| (mapping.start..mapping.end).contains(&pc));
            let rel_pc = mapping.map(|mapping| pc - mapping.start);
            let symbols = match mapping {
                Some(mapping) if mapping.main => {
                    let symbols = self.image.frames(self.image.text + (pc - mapping.start))?;
                    outermost = self
                        .image
                        .physical_name(self.image.text + (pc - mapping.start))
                        .map(str::to_owned);
                    host |= outermost.as_deref().is_some_and(host_fiber_invocation);
                    symbols
                }
                _ => {
                    outermost = None;
                    vec![Symbolized::default()]
                }
            };
            let id = mapping.map(|mapping| mapping.id);
            frames.extend(symbols.into_iter().map(|symbol| FrameKey {
                mapping: id,
                rel_pc,
                symbol,
            }));
        }
        let (root, status) = if stack.len() >= 128 {
            ("truncated", "truncated_stack")
        } else if host {
            ("host", "ok")
        } else if outermost.as_deref().is_some_and(fiber_boundary) {
            ("fiber", "fiber_root")
        } else {
            ("host", "ok")
        };
        let site = self.callsite(frames)?;
        self.db.execute(
            "INSERT INTO cpu_sample(ts_ns,thread_id,cpu,callsite_id,raw_pc_hex,event,period,stack_root,unwind_status)
             VALUES (?1,?2,0,?3,?4,'cpu-clock',?5,?6,?7)",
            (
                Timestamp(time),
                thread_id,
                site,
                format!("{:#x}", stack[0]),
                Unsigned(period),
                root,
                status,
            ),
        )?;
        Ok(())
    }

    fn callsite(&mut self, frames: Vec<FrameKey>) -> Result<i64> {
        let mut parent = None;
        for (depth, key) in frames.into_iter().rev().enumerate() {
            let frame = self.frame(key)?;
            let key = (parent, frame);
            let id = if let Some(&id) = self.callsites.get(&key) {
                id
            } else {
                self.db.execute(
                    "INSERT INTO callsite(parent_id,frame_id,depth) VALUES (?1,?2,?3)",
                    (parent, frame, i64::try_from(depth)?),
                )?;
                let id = self.db.last_insert_rowid();
                self.callsites.insert(key, id);
                id
            };
            parent = Some(id);
        }
        parent.ok_or_else(|| "a stack must have a leaf".into())
    }

    fn frame(&mut self, key: FrameKey) -> Result<i64> {
        if let Some(&id) = self.frames.get(&key) {
            return Ok(id);
        }
        let function = key
            .symbol
            .function
            .as_ref()
            .map(|f| self.function(f))
            .transpose()?;
        if let (Some(file), Some(line)) = (&key.symbol.file, key.symbol.line) {
            self.snapshot(file, line)?;
        }
        self.db.execute(
            "INSERT INTO frame(mapping_id,rel_pc,function_id,file,line,col,inlined)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            (
                key.mapping,
                key.rel_pc.map(Unsigned),
                function,
                &key.symbol.file,
                key.symbol.line,
                key.symbol.col,
                key.symbol.inlined,
            ),
        )?;
        let id = self.db.last_insert_rowid();
        self.frames.insert(key, id);
        Ok(id)
    }

    fn function(&mut self, key: &Function) -> Result<i64> {
        if let Some(&id) = self.functions.get(key) {
            return Ok(id);
        }
        self.db.execute(
            "INSERT INTO function(def_path,instance_label,name,module_kind) VALUES (?1,?2,?3,?4)",
            (&key.def_path, &key.instance, &key.name, key.kind),
        )?;
        let id = self.db.last_insert_rowid();
        self.functions.insert(key.clone(), id);
        Ok(id)
    }

    fn snapshot(&mut self, file: &str, line: u32) -> Result<()> {
        if line == 0 || !self.snippets.insert((file.to_owned(), line)) {
            return Ok(());
        }
        let lines = self.sources.entry(file.to_owned()).or_insert_with(|| {
            let path = Path::new(file);
            let path = if path.is_absolute() {
                path.to_owned()
            } else {
                self.source_root.join(path)
            };
            std::fs::read_to_string(path)
                .ok()
                .map(|source| source.split_inclusive('\n').map(str::to_owned).collect())
        });
        if let Some(text) = lines
            .as_ref()
            .and_then(|lines| lines.get(line as usize - 1))
        {
            self.db.execute(
                "INSERT INTO source_snippet VALUES (?1,?2,?2,?3)",
                (file, line, text),
            )?;
        }
        Ok(())
    }

    fn close(&mut self, time: u64) -> Result<()> {
        for mapping in &self.mappings {
            self.db.execute(
                "UPDATE mapping SET end_ns=?1 WHERE id=?2",
                (Timestamp(time), mapping.id),
            )?;
        }
        for (id, last) in self.threads.values() {
            self.db.execute(
                "UPDATE thread SET end_ns=?1 WHERE id=?2",
                (Timestamp(time.max(*last)), id),
            )?;
        }
        Ok(())
    }
}
