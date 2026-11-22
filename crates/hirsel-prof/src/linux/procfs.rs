use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};

use super::perf::Mapping;
use crate::profile::Result;

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
    use std::os::unix::ffi::OsStrExt;

    let bytes = fs::read(process(pid).join("maps"))?;
    let mut result = Vec::new();
    for mut line in bytes
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        fn field<'a>(line: &mut &'a [u8]) -> Result<&'a str> {
            let end = line
                .iter()
                .position(u8::is_ascii_whitespace)
                .ok_or("short /proc/PID/maps line")?;
            let value = &line[..end];
            *line = line[end..].trim_ascii_start();
            Ok(std::str::from_utf8(value)?)
        }
        let range = field(&mut line)?;
        let perms = field(&mut line)?;
        if perms.as_bytes().get(2) != Some(&b'x') {
            continue;
        }
        let offset = field(&mut line)?;
        let device = field(&mut line)?;
        // An anonymous mapping has no pathname and may end just after inode.
        let inode_end = line
            .iter()
            .position(u8::is_ascii_whitespace)
            .unwrap_or(line.len());
        let inode = std::str::from_utf8(&line[..inode_end])?.parse()?;
        let path = Path::new(std::ffi::OsStr::from_bytes(
            line[inode_end..].trim_ascii_start(),
        ));
        let (start, end) = range
            .split_once('-')
            .ok_or("malformed /proc/PID/maps address range")?;
        let (major, minor) = device
            .split_once(':')
            .ok_or("malformed /proc/PID/maps device")?;
        result.push(Mapping {
            start: u64::from_str_radix(start, 16)?,
            end: u64::from_str_radix(end, 16)?,
            offset: u64::from_str_radix(offset, 16)?,
            path: path.to_owned(),
            dev: Some((
                u32::from_str_radix(major, 16)?,
                u32::from_str_radix(minor, 16)?,
            )),
            inode,
        });
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Identity {
    dev: (u32, u32),
    inode: u64,
}

impl Identity {
    pub(crate) fn read(file: &File) -> io::Result<Self> {
        let meta = file.metadata()?;
        Ok(Self {
            dev: (libc::major(meta.dev()), libc::minor(meta.dev())),
            inode: meta.ino(),
        })
    }

    pub(crate) fn mapping(mapping: &Mapping) -> Option<Self> {
        let dev = mapping.dev?;
        (mapping.inode != 0).then_some(Self {
            dev,
            inode: mapping.inode,
        })
    }
}

pub(crate) fn mapped_file(pid: u32, mapping: &Mapping) -> Result<File> {
    if !mapping.path.is_absolute() {
        return Err("mapping has no file".into());
    }
    let proc = process(pid);
    let map_file = proc.join(format!("map_files/{:x}-{:x}", mapping.start, mapping.end));
    let mut path = mapping.path.as_os_str().as_encoded_bytes();
    if let Some(original) = path.strip_suffix(b" (deleted)") {
        path = original;
    }
    use std::os::unix::ffi::OsStrExt;
    let target_path = proc
        .join("root")
        .join(std::ffi::OsStr::from_bytes(&path[1..]));
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
