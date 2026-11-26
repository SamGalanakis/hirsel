//! What a capture provider hands the importer: samples, mappings and thread
//! events on one clock, whichever host produced them.
use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::profile::Result;

#[derive(Clone, Copy)]
pub(crate) struct Event {
    pub(crate) name: &'static str,
    pub(crate) unit: &'static str,
    pub(crate) kind: u32,
    pub(crate) config: u64,
}

// Runtime timing does not open a sampling event.
pub(crate) const RUNTIME: Event = Event {
    name: "runtime",
    unit: "nanoseconds",
    kind: 0,
    config: 0,
};

pub(crate) const CPU_CLOCK: Event = Event {
    name: "cpu-clock",
    unit: "nanoseconds",
    kind: 1,
    config: 0,
};

pub(crate) struct Record {
    pub(crate) time: u64,
    pub(crate) pid: u32,
    pub(crate) tid: u32,
    pub(crate) kind: Kind,
}

pub(crate) enum Kind {
    Sample(Sample),
    Mapping(Mapping),
    Comm { name: String, exec: bool },
    Fork { parent: u32 },
    Exit,
    Lost(u64),
}

#[derive(Clone)]
pub(crate) struct Mapping {
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) offset: u64,
    pub(crate) path: std::path::PathBuf,
    pub(crate) dev: Option<(u32, u32)>,
    pub(crate) inode: u64,
}

pub(crate) struct Sample {
    pub(crate) pc: u64,
    pub(crate) kernel: bool,
    /// The CPU, when the host reports it.
    pub(crate) cpu: Option<u32>,
    pub(crate) period: u64,
    pub(crate) event: Event,
    pub(crate) registers: Option<(u64, u64, u64)>,
    pub(crate) stack: Vec<u8>,
}

/// The executable mappings of `/proc/PID/maps` text, the form both hosts'
/// runtimes write into their observations.
pub(crate) fn parse_mappings(bytes: &[u8]) -> Result<Vec<Mapping>> {
    use std::os::unix::ffi::OsStrExt;

    let mut result = Vec::new();
    for mut line in bytes
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        fn field<'a>(line: &mut &'a [u8]) -> Result<&'a str> {
            let end = line
                .iter()
                .position(u8::is_ascii_whitespace)
                .ok_or("short mapping line")?;
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
            .ok_or("malformed mapping address range")?;
        let (major, minor) = device.split_once(':').ok_or("malformed mapping device")?;
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
            dev: (
                libc::major(meta.dev() as libc::dev_t) as u32,
                libc::minor(meta.dev() as libc::dev_t) as u32,
            ),
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
