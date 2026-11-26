use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use rusqlite::Connection;

use crate::MemMode;
use crate::host::{self, KernelSymbols};
use crate::image::{self, Function, Image, Symbolized};
use crate::memory::MemoryImport;
use crate::observation::{Observation, RecordKind};
use crate::profile::{Result, Timestamp, Unsigned};
use crate::record::{CPU_CLOCK, Identity, Kind, Mapping, Record, Sample, parse_mappings};
use crate::unwind::{Registers, Stack, Step};

struct ActiveMapping {
    id: i64,
    map: Mapping,
    image: Option<Rc<Image>>,
    virtual_start: Option<u64>,
}

#[derive(Eq, Hash, PartialEq)]
struct FrameKey {
    mapping: Option<i64>,
    rel_pc: Option<u64>,
    symbol: Symbolized,
}

struct Thread {
    id: i64,
    name: String,
}

pub(crate) struct Import<'a> {
    db: &'a Connection,
    pid: u32,
    source_root: PathBuf,
    main: Rc<Image>,
    main_identity: Identity,
    mapping: BTreeMap<u64, ActiveMapping>,
    images: HashMap<Identity, Rc<Image>>,
    vdso: Option<Rc<Image>>,
    pub(crate) kernel_symbols: Option<KernelSymbols>,
    threads: HashMap<u32, Thread>,
    functions: HashMap<Function, i64>,
    frames: HashMap<FrameKey, i64>,
    callsites: HashMap<(Option<i64>, i64), i64>,
    sources: HashMap<String, Option<Vec<String>>>,
    snippets: HashSet<(String, u32)>,
    context: gimli::UnwindContext<usize>,
    pub(crate) samples_lost: u64,
}

impl<'a> Import<'a> {
    pub(crate) fn new(
        db: &'a Connection,
        pid: u32,
        source_root: PathBuf,
        main: Rc<Image>,
        main_identity: Identity,
    ) -> Self {
        Self {
            db,
            pid,
            source_root,
            main,
            main_identity,
            mapping: BTreeMap::new(),
            images: HashMap::new(),
            vdso: None,
            kernel_symbols: None,
            threads: HashMap::new(),
            functions: HashMap::new(),
            frames: HashMap::new(),
            callsites: HashMap::new(),
            sources: HashMap::new(),
            snippets: HashSet::new(),
            context: gimli::UnwindContext::new(),
            samples_lost: 0,
        }
    }

    pub(crate) fn initialize(
        &mut self,
        threads: Vec<(u32, String)>,
        maps: Vec<Mapping>,
        time: u64,
    ) -> Result<()> {
        for (tid, name) in threads {
            self.thread(tid, &name, time)?;
        }
        for mapping in maps {
            self.mapping(mapping, time)?;
        }
        Ok(())
    }

    pub(crate) fn source_remarks(&mut self) -> Result<()> {
        let main = self.main.clone();
        for (def_path, instance, file, line, col, kind, outcome, count) in &main.remarks {
            let id = self.function(&Function {
                def_path: Some(def_path.clone()),
                instance: instance.clone(),
                name: def_path.clone(),
                kind: "hirsel",
            })?;
            self.snapshot(file, *line)?;
            self.db.execute(
                "INSERT INTO codegen_remark VALUES (?1,?2,?3,?4,?5,?6,'source_remark',?7)",
                (id, file, line, col, kind, outcome, Unsigned(*count)),
            )?;
        }
        Ok(())
    }

    /// A live capture keeps the perf mappings.
    pub(crate) fn store_memory(
        &mut self,
        observations: &Observation,
        mode: MemMode,
        live: bool,
    ) -> Result<()> {
        let mut memory = MemoryImport::default();
        let unknown = self.unknown_callsite()?;
        let mut records = observations.records.iter().peekable();
        let mut maps = Vec::new();
        while let Some(record) = records.next() {
            match &record.kind {
                RecordKind::Mapping(line) if !live => {
                    maps.extend_from_slice(line);
                    maps.push(b'\n');
                    if !records.peek().is_some_and(|next| {
                        matches!(next.kind, RecordKind::Mapping(_)) && next.ts == record.ts
                    }) {
                        self.close_mappings(record.ts)?;
                        self.initialize(Vec::new(), parse_mappings(&maps)?, record.ts)?;
                        maps.clear();
                    }
                }
                RecordKind::Memory { event, stack } => {
                    let site = if stack.is_empty() {
                        None
                    } else {
                        let (site, status) = self.memory_callsite(stack)?;
                        memory.stack(status);
                        Some(site)
                    };
                    memory.record(record.ts, event, site)?;
                }
                _ => {}
            }
        }
        memory.finish(
            self.db,
            mode.as_str(),
            observations.lost,
            observations.complete.then_some(observations.memory_emitted),
            unknown,
            live,
        )
    }

    pub(crate) fn record(&mut self, record: Record) -> Result<()> {
        let Record {
            time,
            pid,
            tid,
            kind,
        } = record;
        if let Kind::Lost(lost) = kind {
            self.samples_lost = self
                .samples_lost
                .checked_add(lost)
                .ok_or("profile loss exceeds u64")?;
            return Ok(());
        }
        if pid != self.pid {
            return Ok(());
        }
        match kind {
            Kind::Sample(sample) => self.sample(tid, time, sample),
            Kind::Mapping(map) => self.mapping(map, time),
            Kind::Comm { name, exec } => {
                if exec {
                    // Mappings from the previous image cannot resolve samples
                    // from the new address space.
                    self.close_mappings(time)?;
                }
                self.thread(tid, &name, time)?;
                Ok(())
            }
            Kind::Fork { parent } => {
                let name = self
                    .threads
                    .get(&parent)
                    .map_or("", |t| t.name.as_str())
                    .to_owned();
                self.thread(tid, &name, time)?;
                Ok(())
            }
            Kind::Exit => self.close_thread(tid, time),
            Kind::Lost(_) => unreachable!(),
        }
    }

    pub(crate) fn close(&mut self, time: u64) -> Result<()> {
        self.close_mappings(time)?;
        for (_, thread) in self.threads.drain() {
            self.db.execute(
                "UPDATE thread SET end_ns=?1 WHERE id=?2",
                (Timestamp(time), thread.id),
            )?;
        }
        Ok(())
    }

    fn close_mappings(&mut self, time: u64) -> Result<()> {
        for (_, mapping) in std::mem::take(&mut self.mapping) {
            self.db.execute(
                "UPDATE mapping SET end_ns=?1 WHERE id=?2",
                (Timestamp(time), mapping.id),
            )?;
        }
        Ok(())
    }

    fn close_thread(&mut self, tid: u32, time: u64) -> Result<()> {
        if let Some(thread) = self.threads.remove(&tid) {
            self.db.execute(
                "UPDATE thread SET end_ns=?1 WHERE id=?2",
                (Timestamp(time), thread.id),
            )?;
        }
        Ok(())
    }

    fn thread(&mut self, tid: u32, name: &str, time: u64) -> Result<i64> {
        if let Some(thread) = self.threads.get(&tid)
            && (name.is_empty() || thread.name == name)
        {
            return Ok(thread.id);
        }
        self.close_thread(tid, time)?;
        self.db.execute(
            "INSERT INTO thread(pid,tid,name,start_ns) VALUES (?1,?2,?3,?4)",
            (self.pid, tid, name, Timestamp(time)),
        )?;
        let id = self.db.last_insert_rowid();
        self.threads.insert(
            tid,
            Thread {
                id,
                name: name.to_owned(),
            },
        );
        Ok(id)
    }

    fn mapping(&mut self, map: Mapping, time: u64) -> Result<()> {
        if map.start >= map.end {
            return Err("empty or inverted perf mapping".into());
        }
        if self.mapping.get(&map.start).is_some_and(|previous| {
            previous.map.end == map.end
                && previous.map.offset == map.offset
                && previous.map.path == map.path
                && previous.map.dev == map.dev
                && previous.map.inode == map.inode
        }) {
            return Ok(());
        }
        let mut overlaps = Vec::new();
        if let Some((&start, previous)) = self.mapping.range(..map.start).next_back()
            && previous.map.end > map.start
        {
            overlaps.push(start);
        }
        overlaps.extend(
            self.mapping
                .range(map.start..map.end)
                .map(|(&start, _)| start),
        );
        for start in overlaps {
            let previous = self.mapping.remove(&start).expect("existing mapping");
            self.db.execute(
                "UPDATE mapping SET end_ns=?1 WHERE id=?2",
                (Timestamp(time), previous.id),
            )?;
            if previous.map.start < map.start {
                let mut left = previous.map.clone();
                left.end = map.start;
                self.insert_mapping(left, previous.image.clone(), time)?;
            }
            if previous.map.end > map.end {
                let mut right = previous.map;
                right.offset = right
                    .offset
                    .checked_add(map.end - right.start)
                    .ok_or("mapping offset overflows")?;
                right.start = map.end;
                self.insert_mapping(right, previous.image, time)?;
            }
        }
        let image = self.load_image(&map);
        self.insert_mapping(map, image, time)
    }

    fn insert_mapping(&mut self, map: Mapping, image: Option<Rc<Image>>, time: u64) -> Result<()> {
        let virtual_start = image.as_ref().and_then(|image| image.virtual_start(&map));
        self.db.execute(
            "INSERT INTO mapping(start_addr_hex,end_addr_hex,file_offset,path,build_id,image_digest,start_ns)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            (
                format!("{:#x}", map.start), format!("{:#x}", map.end),
                Unsigned(map.offset), map.path.to_string_lossy(),
                image.as_ref().and_then(|image| image.build_id.as_deref()),
                image.as_ref().map(|image| image.digest.as_str()), Timestamp(time),
            ),
        )?;
        let id = self.db.last_insert_rowid();
        self.mapping.insert(
            map.start,
            ActiveMapping {
                id,
                map,
                image,
                virtual_start,
            },
        );
        Ok(())
    }

    fn load_image(&mut self, mapping: &Mapping) -> Option<Rc<Image>> {
        if mapping.path == Path::new("[vdso]") {
            if self.vdso.is_none() {
                let bytes = host::vdso(self.pid, mapping).ok()?;
                self.vdso = Some(Rc::new(
                    Image::parse(&bytes, image::local_symbols(&bytes), false).ok()?,
                ));
            }
            return self.vdso.clone();
        }
        if Identity::mapping(mapping) == Some(self.main_identity) {
            return Some(self.main.clone());
        }
        let mut fd = host::mapped_file(self.pid, mapping).ok()?;
        let identity = Identity::read(&fd).ok()?;
        if identity == self.main_identity {
            return Some(self.main.clone());
        }
        if let Some(image) = self.images.get(&identity) {
            return Some(image.clone());
        }
        if !fd.metadata().ok()?.is_file() {
            return None;
        }
        let mut bytes = Vec::new();
        fd.read_to_end(&mut bytes).ok()?;
        let image = Rc::new(Image::parse(&bytes, image::local_symbols(&bytes), false).ok()?);
        self.images.insert(identity, image.clone());
        Some(image)
    }

    fn sample(&mut self, tid: u32, time: u64, sample: Sample) -> Result<()> {
        let thread = self.thread(tid, "", time)?;
        let (frames, root, status) = if sample.kernel
            || (host::kernel_address(sample.pc)
                && Self::active_mapping(&self.mapping, sample.pc).is_none())
        {
            let label = self
                .kernel_symbols
                .get_or_insert_with(KernelSymbols::read)
                .label(sample.pc);
            let symbol = Symbolized {
                function: Some(Function {
                    def_path: None,
                    instance: label.to_owned(),
                    name: label.to_owned(),
                    kind: "kernel",
                }),
                ..Symbolized::default()
            };
            (
                vec![FrameKey {
                    mapping: None,
                    rel_pc: None,
                    symbol,
                }],
                "kernel",
                "not_attempted",
            )
        } else {
            self.stack(sample.pc, sample.registers, &sample.stack)?
        };
        let site = self.callsite(frames)?;
        self.db.execute(
            "INSERT INTO cpu_sample(ts_ns,thread_id,cpu,callsite_id,raw_pc_hex,event,period,stack_root,unwind_status)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            (Timestamp(time), thread, sample.cpu, site, format!("{:#x}", sample.pc),
             sample.event.name, Unsigned(sample.period), root, status),
        )?;
        Ok(())
    }

    /// A stack the runtime walked by frame pointers: the interrupted PC,
    /// then each return address. The walk needs no image, so a frame without
    /// one stays unsymbolized and the frames above it still count.
    pub(crate) fn walked(
        &mut self,
        tid: u32,
        time: u64,
        period: u64,
        pcs: &[u64],
        truncated: bool,
    ) -> Result<()> {
        let leaf = *pcs.first().ok_or("a walked stack must have a leaf")?;
        let thread = self.thread(tid, "", time)?;
        let mut frames = Vec::new();
        let mut host = false;
        let mut fiber = false;
        for (depth, &raw) in pcs.iter().enumerate() {
            // A return address names the instruction after its call.
            let pc = if depth == 0 {
                raw
            } else {
                raw.saturating_sub(1)
            };
            let active = Self::active_mapping(&self.mapping, pc);
            let rel_pc = active.map(|active| pc - active.map.start);
            let resolved = active.and_then(|active| {
                let address = active.virtual_start?.checked_add(rel_pc?)?;
                Some((active.image.as_ref()?, address))
            });
            let symbols = if let Some((image, address)) = resolved {
                image.frames(address)?
            } else {
                vec![Symbolized::default()]
            };
            frames.extend(symbols.into_iter().map(|symbol| FrameKey {
                mapping: active.map(|active| active.id),
                rel_pc,
                symbol,
            }));
            if let Some((image, address)) = resolved
                && image.main
                && let Some(name) = image.physical_name(address)
            {
                host |= image::host_fiber_invocation(name);
                if !host && image::fiber_boundary(name) {
                    fiber = true;
                    break;
                }
            }
        }
        let (root, status) = if Self::active_mapping(&self.mapping, leaf).is_none() {
            ("failed", "unknown_mapping")
        } else if fiber {
            ("fiber", "fiber_root")
        } else if truncated {
            ("truncated", "truncated_stack")
        } else {
            ("host", "ok")
        };
        let site = self.callsite(frames)?;
        self.db.execute(
            "INSERT INTO cpu_sample(ts_ns,thread_id,cpu,callsite_id,raw_pc_hex,event,period,stack_root,unwind_status)
             VALUES (?1,?2,NULL,?3,?4,?5,?6,?7,?8)",
            (Timestamp(time), thread, site, format!("{leaf:#x}"),
             CPU_CLOCK.name, Unsigned(period), root, status),
        )?;
        Ok(())
    }

    pub(crate) fn memory_callsite(&mut self, bytes: &[u8]) -> Result<(i64, &'static str)> {
        let (registers, bytes) = bytes
            .split_first_chunk::<32>()
            .ok_or("truncated memory snapshot registers")?;
        let mut words = registers
            .chunks_exact(8)
            .map(|word| u64::from_le_bytes(word.try_into().unwrap()));
        let pc = words.next().unwrap();
        let regs = Some((
            words.next().unwrap(),
            words.next().unwrap(),
            words.next().unwrap(),
        ));
        let (mut frames, _, status) = self.stack(pc, regs, bytes)?;
        if let Some(source) = frames.iter().position(|f| {
            f.symbol
                .function
                .as_ref()
                .is_some_and(|f| f.kind == "hirsel")
        }) {
            frames.drain(..source);
        }
        Ok((self.callsite(frames)?, status))
    }

    pub(crate) fn unknown_callsite(&mut self) -> Result<i64> {
        self.callsite(vec![FrameKey {
            mapping: None,
            rel_pc: None,
            symbol: Symbolized::default(),
        }])
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

    fn stack(
        &mut self,
        mut pc: u64,
        captured: Option<(u64, u64, u64)>,
        bytes: &[u8],
    ) -> Result<(Vec<FrameKey>, &'static str, &'static str)> {
        let mut frames = Vec::new();
        let mut registers = captured.map(|(sp, bp, link)| Registers::new(pc, sp, bp, link));
        let stack = Stack {
            base: registers.map_or(0, |r| r.sp),
            bytes,
        };
        let mut host = false;
        let mut fallback = false;
        let mut visited = HashSet::new();
        let (root, status) = loop {
            let active = Self::active_mapping(&self.mapping, pc);
            let rel_pc = active.map(|active| pc - active.map.start);
            let resolved = active.and_then(|active| {
                let address = active.virtual_start?.checked_add(rel_pc?)?;
                Some((active.image.as_ref()?, address))
            });
            let symbols = if let Some((image, address)) = resolved {
                image.frames(address)?
            } else {
                vec![Symbolized::default()]
            };
            frames.extend(symbols.into_iter().map(|symbol| FrameKey {
                mapping: active.map(|active| active.id),
                rel_pc,
                symbol,
            }));
            let Some((image, address)) = resolved else {
                break ("failed", "unknown_mapping");
            };
            let Some(regs) = registers else {
                break ("failed", "eval_error");
            };
            if !visited.insert((pc, regs.sp)) {
                break ("failed", "eval_error");
            }
            let physical_name = image.physical_name(address);
            host |= image.main && physical_name.is_some_and(image::host_fiber_invocation);
            match image.unwind.step(address, &mut self.context, &regs, &stack) {
                // dyld calls a Mach-O entry from the shared cache, which has
                // no image file to unwind: the entry is the thread's root.
                Step::Caller(caller)
                    if image.in_entry(address)
                        && Self::active_mapping(&self.mapping, caller.pc.saturating_sub(1))
                            .is_none_or(|active| active.image.is_none()) =>
                {
                    break ("host", "ok");
                }
                Step::Caller(caller) => {
                    pc = caller.pc.saturating_sub(1);
                    registers = Some(caller);
                }
                Step::FramePointerFallback(caller) => {
                    fallback = true;
                    pc = caller.pc.saturating_sub(1);
                    registers = Some(caller);
                }
                Step::Root if host => break ("host", "ok"),
                Step::Root if image.main && physical_name.is_some_and(image::fiber_boundary) => {
                    break ("fiber", "fiber_root");
                }
                Step::Root => break ("host", "ok"),
                Step::Truncated => break ("truncated", "truncated_stack"),
                Step::MissingCfi => break ("failed", "missing_cfi"),
                Step::EvalError => break ("failed", "eval_error"),
            }
        };
        if fallback {
            return Ok((frames, "failed", "missing_cfi"));
        }
        Ok((frames, root, status))
    }

    fn active_mapping(mapping: &BTreeMap<u64, ActiveMapping>, pc: u64) -> Option<&ActiveMapping> {
        mapping
            .range(..=pc)
            .next_back()
            .map(|(_, value)| value)
            .filter(|active| pc < active.map.end)
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
            let path = image::source_path(&self.source_root, file);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProfileWriter;

    #[test]
    fn runtime_observation_refuses_unassigned_kind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime.observations");
        let mut wire = b"HIRSOBS3".to_vec();
        // Both frames have no words or bytes. Kind 24 is unassigned; 65,534 is END.
        for kind in [24u16, 65_534] {
            let mut header = [0; 20];
            header[12..14].copy_from_slice(&kind.to_le_bytes());
            wire.extend_from_slice(&header);
        }
        std::fs::write(&path, wire).unwrap();
        let profile = ProfileWriter::create(&dir.path().join("run.hprof")).unwrap();
        let error = Observation::read(&path)
            .and_then(|observations| observations.store(profile.connection()))
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "unassigned runtime observation kind 24 in HIRSOBS3"
        );
    }
}
