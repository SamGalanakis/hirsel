use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use object::{Object, ObjectSection};

use crate::capture::SymbolSource;
use crate::profile::Result;
use crate::record::{Identity, Mapping, parse_mappings};

pub(crate) fn process(pid: u32) -> PathBuf {
    PathBuf::from(format!("/proc/{pid}"))
}

pub(crate) fn executable(pid: u32) -> Result<(PathBuf, File)> {
    let link = process(pid).join("exe");
    Ok((fs::read_link(&link)?, File::open(link)?))
}

pub(crate) fn thread_count(pid: u32) -> Result<usize> {
    fs::read_to_string(process(pid).join("status"))?
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))
        .ok_or_else(|| "missing Threads in /proc/PID/status".into())
        .and_then(|count| Ok(count.trim().parse()?))
}

pub(crate) fn threads(pid: u32) -> Result<Vec<(u32, String)>> {
    let dir = process(pid).join("task");
    let mut threads = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let tid = entry.file_name().to_string_lossy().parse()?;
        match fs::read_to_string(entry.path().join("comm")) {
            Ok(name) => threads.push((tid, name.trim_end_matches('\n').to_owned())),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(threads)
}

pub(crate) fn mappings(pid: u32) -> Result<Vec<Mapping>> {
    parse_mappings(&fs::read(process(pid).join("maps"))?)
}

#[derive(Default)]
pub(crate) struct KernelSymbols(Vec<(u64, Option<String>)>);

impl KernelSymbols {
    pub(crate) fn read() -> Self {
        fs::read_to_string("/proc/kallsyms")
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    pub(crate) fn parse(text: &str) -> Self {
        let mut symbols: Vec<_> = text
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_ascii_whitespace();
                let address = u64::from_str_radix(fields.next()?, 16).ok()?;
                let kind = fields.next()?;
                let name = fields.next()?;
                if address == 0 {
                    // Linux can expose the names with all addresses redacted.
                    return None;
                }
                let label = matches!(kind, "T" | "t" | "W" | "w").then(|| {
                    let mut label = format!("[kernel] {name}");
                    if let Some(module) = fields.next() {
                        label.push(' ');
                        label.push_str(module);
                    }
                    label
                });
                Some((address, label))
            })
            .collect();
        symbols.sort_by_key(|(address, label)| (*address, label.is_some()));
        Self(symbols)
    }

    pub(crate) fn label(&self, pc: u64) -> &str {
        let next = self.0.partition_point(|(address, _)| *address <= pc);
        if let Some((start, Some(label))) = self.0[..next].last()
            && (pc == *start || next < self.0.len())
        {
            return label;
        }
        "[kernel]"
    }
}

pub(crate) fn mapped_file(pid: u32, mapping: &Mapping) -> Result<File> {
    if !mapping.path.is_absolute() {
        return Err("mapping has no file".into());
    }
    let proc = process(pid);
    let map_file = proc.join(format!("map_files/{:x}-{:x}", mapping.start, mapping.end));
    let target_path = proc.join("root").join(image_path(&mapping.path));
    let file = File::open(map_file)
        .or_else(|_| File::open(target_path))
        .or_else(|_| File::open(&mapping.path))?;
    if let Some(expected) = Identity::mapping(mapping)
        && Identity::read(&file)? != expected
    {
        return Err("mapped image identity changed".into());
    }
    Ok(file)
}

fn image_path(path: &Path) -> &Path {
    let bytes = path.as_os_str().as_bytes();
    let bytes = bytes.strip_suffix(b" (deleted)").unwrap_or(bytes);
    let relative = &bytes[bytes.iter().take_while(|&&byte| byte == b'/').count()..];
    Path::new(OsStr::from_bytes(relative))
}

pub(crate) fn mapped_debug(pid: u32, path: &Path, image: &[u8]) -> Option<SymbolSource> {
    let root = process(pid).join("root");
    let debug = detached_debug(&root, image_path(path), image)
        .or_else(|| detached_debug(Path::new("/"), image_path(path), image))?;
    Some(SymbolSource::DetachedDwarf(debug))
}

fn detached_debug(root: &Path, path: &Path, image: &[u8]) -> Option<Vec<u8>> {
    let file = object::File::parse(image).ok()?;
    let directory = path.parent()?;
    let debug_root = root.join("usr/lib/debug");
    let read = |candidate: PathBuf, matches: &dyn Fn(&object::File<'_>, &[u8]) -> bool| {
        let bytes = fs::read(candidate).ok()?;
        let debug = object::File::parse(bytes.as_slice()).ok()?;
        (debug.format() == file.format()
            && debug.architecture() == file.architecture()
            && debug.is_little_endian() == file.is_little_endian()
            && (debug.section_by_name(".debug_info").is_some() || debug.symbol_table().is_some())
            && matches(&debug, &bytes))
        .then_some(bytes)
    };
    if let Some(id) = file.build_id().ok().flatten().filter(|id| !id.is_empty()) {
        let encoded = hirsel_core_ids::hex::encode(id);
        let (prefix, suffix) = encoded.split_at(2);
        let candidate = debug_root
            .join(".build-id")
            .join(prefix)
            .join(format!("{suffix}.debug"));
        if let Some(bytes) = read(candidate, &|debug, _| {
            debug.build_id().is_ok_and(|actual| actual == Some(id))
        }) {
            return Some(bytes);
        }
    }
    let (name, crc) = file.gnu_debuglink().ok().flatten()?;
    let name = Path::new(OsStr::from_bytes(name));
    if name.file_name() != Some(name.as_os_str()) {
        return None;
    }
    [
        root.join(directory),
        root.join(directory).join(".debug"),
        debug_root.join(directory),
    ]
    .into_iter()
    .find_map(|directory| {
        read(directory.join(name), &|_, bytes| {
            crc32fast::hash(bytes) == crc
        })
    })
}

pub(crate) fn vdso(pid: u32, mapping: &Mapping) -> Result<Vec<u8>> {
    fn bytes(pid: u32, mapping: &Mapping) -> Result<Vec<u8>> {
        let len = usize::try_from(
            mapping
                .end
                .checked_sub(mapping.start)
                .ok_or("invalid vDSO range")?,
        )?;
        let mut bytes = vec![0; len];
        File::open(process(pid).join("mem"))?.read_exact_at(&mut bytes, mapping.start)?;
        Ok(bytes)
    }

    if let Ok(bytes) = bytes(pid, mapping) {
        return Ok(bytes);
    }
    // Our sampler and target use the same native ABI in the same kernel.
    // Access to another live process's /proc/PID/mem may require ptrace.
    let own_pid = std::process::id();
    let mapping = mappings(own_pid)?
        .into_iter()
        .find(|map| map.path == Path::new("[vdso]"))
        .ok_or("no native vDSO mapping")?;
    bytes(own_pid, &mapping)
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;
    use crate::image::Image;
    use crate::import::Import;
    use crate::record::{CPU_CLOCK, Kind, Record, Sample};

    #[test]
    fn mapped_libc_names_ifunc_implementations_at_saved_pcs() {
        let bytes = include_bytes!("../../../testdata/libc-ifunc.elf");
        let debug = include_bytes!("../../../testdata/libc-ifunc.debug");
        let root = tempfile::tempdir().unwrap();
        let build_id = root
            .path()
            .join("usr/lib/debug/.build-id/8c/ebda42d140fc2b09639f8b13a5d5f2e617ef09.debug");
        fs::create_dir_all(build_id.parent().unwrap()).unwrap();
        fs::write(&build_id, debug).unwrap();
        let library = Path::new("usr/lib/libc-ifunc.elf");
        assert_eq!(detached_debug(root.path(), library, bytes).unwrap(), debug);
        let bad_debug = include_bytes!("../../../testdata/llvm-inline.debug");
        fs::write(&build_id, bad_debug).unwrap();

        let path = root.path().join(library);
        fs::write(&path, bytes).unwrap();
        let sibling = path.with_file_name("libc-ifunc.debug");
        fs::write(&sibling, bad_debug).unwrap();
        assert!(detached_debug(root.path(), library, bytes).is_none());
        fs::write(&sibling, debug).unwrap();
        assert_eq!(detached_debug(root.path(), library, bytes).unwrap(), debug);

        let writer = crate::ProfileWriter::create(&root.path().join("fixture.hprof")).unwrap();
        let main = std::rc::Rc::new(Image::parse(bytes, SymbolSource::OffsetsOnly, true).unwrap());
        let identity = Identity::read(&File::open(&sibling).unwrap()).unwrap();
        let pid = std::process::id();
        let mut import = Import::new(
            writer.connection(),
            pid,
            root.path().to_owned(),
            main,
            identity,
        );
        import
            .initialize(
                vec![],
                vec![Mapping {
                    start: 0x188000,
                    end: 0x18a000,
                    offset: 0,
                    path,
                    dev: None,
                    inode: 0,
                }],
                0,
            )
            .unwrap();
        for (time, pc) in [(1, 0x18891d), (2, 0x189024)] {
            import
                .record(Record {
                    time,
                    pid,
                    tid: pid,
                    kind: Kind::Sample(Sample {
                        pc,
                        kernel: false,
                        cpu: None,
                        period: 1,
                        event: CPU_CLOCK,
                        registers: None,
                        stack: vec![],
                    }),
                })
                .unwrap();
        }
        let mut query = writer
            .connection()
            .prepare(
                "SELECT fn.name FROM cpu_sample s JOIN callsite c ON c.id=s.callsite_id
             JOIN frame f ON f.id=c.frame_id LEFT JOIN function fn ON fn.id=f.function_id
             ORDER BY s.ts_ns",
            )
            .unwrap();
        let names = query
            .query_map([], |row| row.get::<_, Option<String>>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(names[0].as_deref(), Some("__memcmp_avx2_movbe"));
        assert!(matches!(
            names[1].as_deref(),
            Some("__memmove_avx_unaligned_erms" | "__memcpy_avx_unaligned_erms")
        ));
    }
}
