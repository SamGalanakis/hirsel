use std::collections::VecDeque;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::profile::Result;

const RECORD_ID: u64 = (1 << 1) | (1 << 2) | (1 << 7); // TID, TIME, CPU
const SAMPLE_FIELDS: u64 = RECORD_ID | (1 << 0) // IP
    | (1 << 8) // PERIOD
    | (1 << 12) // REGS_USER
    | (1 << 13); // STACK_USER

#[cfg(target_arch = "x86_64")]
const REGISTERS: u64 = (1 << 6) | (1 << 7) | (1 << 8); // BP, SP, IP
#[cfg(target_arch = "aarch64")]
const REGISTERS: u64 = (1 << 29) | (1 << 30) | (1 << 31) | (1 << 32); // X29, LR, SP, PC

const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 1 << 3;
const PERF_ENABLE: libc::c_ulong = 0x2400;
const PERF_DISABLE: libc::c_ulong = 0x2401;

// Linux perf_event_attr, through PERF_ATTR_SIZE_VER3 (96 bytes).
#[repr(C)]
struct PerfAttr {
    kind: u32,
    size: u32,
    config: u64,
    freq: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
    wakeup_events: u32,
    bp_type: u32,
    config1: u64,
    config2: u64,
    branch_sample_type: u64,
    sample_regs_user: u64,
    sample_stack_user: u32,
    clockid: i32,
}

#[derive(Clone, Copy)]
pub(crate) struct Event {
    pub(crate) name: &'static str,
    pub(crate) unit: &'static str,
    kind: u32,
    config: u64,
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

// PERF_COUNT_SW_DUMMY does not sample. An enabled event still delivers mmap,
// comm and task records, including while the sampling events are disabled.
pub(crate) const TRACKING: Event = Event {
    name: "mapping tracking",
    unit: "",
    kind: 1,
    config: 9,
};

pub(crate) fn events(list: &str) -> Result<Vec<Event>> {
    let mut result = Vec::new();
    for name in list.split(',') {
        let (name, config) = match name.trim() {
            "cycles" => ("cycles", 0),
            "instructions" => ("instructions", 1),
            "cache-references" => ("cache-references", 2),
            "cache-misses" => ("cache-misses", 3),
            "branches" => ("branches", 4),
            "branch-misses" => ("branch-misses", 5),
            "bus-cycles" => ("bus-cycles", 6),
            "ref-cycles" => ("ref-cycles", 9),
            "cpu-clock" => {
                if result
                    .iter()
                    .any(|event: &Event| event.name == CPU_CLOCK.name)
                {
                    return Err("repeated --pmu event cpu-clock".into());
                }
                result.push(CPU_CLOCK);
                continue;
            }
            _ => return Err(format!("unknown --pmu event {name:?}").into()),
        };
        if result.iter().any(|event: &Event| event.name == name) {
            return Err(format!("repeated --pmu event {name}").into());
        }
        result.push(Event {
            name,
            unit: name,
            kind: 0,
            config,
        });
    }
    Ok(result)
}

pub(crate) fn monotonic_ns() -> io::Result<u64> {
    // SAFETY: zero is a valid initialization and clock_gettime fills the buffer.
    let mut ts: libc::timespec = unsafe { zeroed() };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64)
}

pub(crate) fn validate_settings(freq: u64, bytes: u32) -> Result<()> {
    if freq == 0 {
        return Err("--freq must be positive".into());
    }
    let path = "/proc/sys/kernel/perf_event_max_sample_rate";
    if let Some(max) = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        && freq > max
    {
        return Err(format!("--freq {freq} exceeds {path} ({max}); lower --freq").into());
    }
    if bytes == 0 || bytes > 65_408 || !bytes.is_multiple_of(8) {
        return Err("--stack-bytes must be a multiple of 8 between 8 and 65408".into());
    }
    Ok(())
}

pub(crate) fn open_error(err: io::Error, event: Event) -> String {
    let mut result = format!("perf_event_open({}): {err}", event.name);
    match err.raw_os_error() {
        Some(libc::EACCES | libc::EPERM | libc::ENOSYS) => {
            let paranoid = std::fs::read_to_string("/proc/sys/kernel/perf_event_paranoid");
            if let Ok(value) = paranoid {
                result.push_str(&format!("; perf_event_paranoid={}", value.trim()));
            }
            let seccomp = std::fs::read_to_string("/proc/thread-self/status").is_ok_and(|status| {
                status.lines().any(|line| {
                    line.strip_prefix("Seccomp:")
                        .is_some_and(|mode| mode.trim() != "0")
                })
            });
            if seccomp {
                result
                    .push_str("; a seccomp filter is active, check that it allows perf_event_open");
            }
            result.push_str("; confirm target access and request CAP_PERFMON or a host perf policy that permits user-space sampling");
        }
        Some(libc::EINVAL) => {
            result.push_str("; check the PMU event and /proc/sys/kernel/perf_event_max_sample_rate")
        }
        _ => {}
    }
    result
}

pub(crate) fn online_cpus() -> Result<Vec<i32>> {
    let contents = std::fs::read_to_string("/sys/devices/system/cpu/online")?;
    let mut cpus = Vec::new();
    for entry in contents.trim().split(',') {
        let (first, last) = entry.split_once('-').unwrap_or((entry, entry));
        cpus.extend(first.parse::<i32>()?..=last.parse::<i32>()?);
    }
    if cpus.is_empty() {
        return Err("no online CPUs for perf_event_open".into());
    }
    Ok(cpus)
}

pub(crate) struct PerfSettings {
    pub(crate) freq: u64,
    pub(crate) stack_bytes: u32,
    pub(crate) sideband: bool,
    pub(crate) enable_on_exec: bool,
}

pub(crate) struct PerfGroup {
    rings: Vec<Ring>,
    sorter: EventSorter,
}

impl PerfGroup {
    pub(crate) fn new() -> Self {
        Self {
            rings: Vec::new(),
            sorter: EventSorter::default(),
        }
    }

    pub(crate) fn open(
        &mut self,
        tid: i32,
        cpu: i32,
        event: Event,
        settings: PerfSettings,
    ) -> io::Result<()> {
        self.add(tid, cpu, event, Some(settings))
    }

    pub(crate) fn open_tracking(&mut self, tid: i32) -> io::Result<()> {
        self.add(tid, -1, TRACKING, None)?;
        self.rings
            .last()
            .expect("new tracking ring")
            .control(PERF_ENABLE)
    }

    fn add(
        &mut self,
        tid: i32,
        cpu: i32,
        event: Event,
        settings: Option<PerfSettings>,
    ) -> io::Result<()> {
        let mut flags = (1 << 0) | (1 << 5) | (1 << 6) | (1 << 18) | (1 << 25);
        if settings.as_ref().is_none_or(|settings| settings.sideband) {
            flags |= (1 << 8) | (1 << 9) | (1 << 13) | (1 << 23) | (1 << 24);
        }
        let (freq, sample_type, sample_regs_user, sample_stack_user) =
            if let Some(settings) = settings {
                flags |= 1 << 10; // Frequency, rather than a fixed period.
                if settings.enable_on_exec {
                    flags |= 1 << 12;
                    if cpu != -1 {
                        flags |= (1 << 1) | (1 << 35); // Inherit within the thread group.
                    }
                }
                (
                    settings.freq,
                    SAMPLE_FIELDS,
                    REGISTERS,
                    settings.stack_bytes,
                )
            } else {
                (0, RECORD_ID, 0, 0)
            };
        let attr = PerfAttr {
            kind: event.kind,
            size: size_of::<PerfAttr>() as u32,
            config: event.config,
            freq,
            sample_type,
            read_format: 0,
            flags,
            wakeup_events: 1,
            bp_type: 0,
            config1: 0,
            config2: 0,
            branch_sample_type: 0,
            sample_regs_user,
            sample_stack_user,
            clockid: libc::CLOCK_MONOTONIC,
        };
        // SAFETY: the kernel reads a properly laid out, initialized attribute.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                &attr,
                tid,
                cpu,
                -1,
                PERF_FLAG_FD_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a successful syscall transfers the descriptor to this process.
        let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        self.rings.push(Ring::map(tid, fd, event)?);
        Ok(())
    }

    pub(crate) fn collect(&mut self, done: bool) -> Result<VecDeque<Record>> {
        let watermark = monotonic_ns()?;
        for ring in &mut self.rings {
            ring.read(&mut self.sorter.pending)?;
        }
        Ok(self.sorter.advance(watermark, done))
    }

    pub(crate) fn wait(&self) -> io::Result<()> {
        let mut fds: Vec<_> = self
            .rings
            .iter()
            .map(|ring| libc::pollfd {
                fd: ring.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: poll gets the current vector length and never retains its pointer.
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, 10) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        Ok(())
    }
    pub(crate) fn enable(&self, from: usize) -> io::Result<()> {
        for ring in &self.rings[from..] {
            ring.control(PERF_ENABLE)?;
        }
        Ok(())
    }

    pub(crate) fn size(&self) -> usize {
        self.rings.len()
    }

    pub(crate) fn disable(&self) -> io::Result<()> {
        for ring in &self.rings {
            ring.control(PERF_DISABLE)?;
        }
        Ok(())
    }
    pub(crate) fn truncate(&mut self, len: usize) {
        self.rings.truncate(len);
    }

    pub(crate) fn remove_tid(&mut self, tid: u32) -> Result<()> {
        let mut index = 0;
        while index < self.rings.len() {
            if self.rings[index].tid as u32 == tid {
                let mut ring = self.rings.swap_remove(index);
                ring.read(&mut self.sorter.pending)?;
            } else {
                index += 1;
            }
        }
        Ok(())
    }
}

struct Ring {
    tid: i32,
    fd: OwnedFd,
    base: NonNull<u8>,
    map_len: usize,
    data_offset: usize,
    data_size: usize,
    tail: u64,
    event: Event,
}

// data_head, data_tail, data_offset and data_size in perf_event_mmap_page.
const HEAD: usize = 1024;
const TAIL: usize = 1032;
const OFFSET: usize = 1040;
const SIZE: usize = 1048;

impl Ring {
    fn map(tid: i32, fd: OwnedFd, event: Event) -> io::Result<Self> {
        // SAFETY: sysconf has no pointer argument.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page = usize::try_from(page).map_err(|_| io::Error::last_os_error())?;
        let map_len = page * 65;
        // SAFETY: this maps the kernel-owned perf metadata and ring at offset zero.
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                map_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let base: NonNull<u8> = NonNull::new(raw.cast()).expect("mmap with a null hint");
        // SAFETY: the mapping is live, page-aligned, and both fields lie in its metadata page.
        let (offset, size) = unsafe {
            (
                base.as_ptr().add(OFFSET).cast::<u64>().read_volatile() as usize,
                base.as_ptr().add(SIZE).cast::<u64>().read_volatile() as usize,
            )
        };
        let ring = Self {
            tid,
            fd,
            base,
            map_len,
            data_offset: offset,
            data_size: size,
            tail: 0,
            event,
        };
        if offset < page || !size.is_power_of_two() || offset.checked_add(size) != Some(map_len) {
            return Err(io::Error::other("unexpected perf mmap ring layout"));
        }
        Ok(ring)
    }

    fn control(&self, request: libc::c_ulong) -> io::Result<()> {
        // SAFETY: these perf ioctls take a zero argument and a live perf descriptor.
        if unsafe { libc::ioctl(self.fd.as_raw_fd(), request, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn copy(&self, tail: u64, out: &mut [u8]) {
        let index = (tail as usize) & (self.data_size - 1);
        let first = out.len().min(self.data_size - index);
        // SAFETY: each segment is in the mmap ring. Its producer cannot reuse
        // these bytes until we release data_tail after completing the copies.
        unsafe {
            let data = self.base.as_ptr().add(self.data_offset);
            std::ptr::copy_nonoverlapping(data.add(index), out.as_mut_ptr(), first);
            std::ptr::copy_nonoverlapping(data, out.as_mut_ptr().add(first), out.len() - first);
        }
    }

    fn read(&mut self, records: &mut Vec<Record>) -> Result<()> {
        // SAFETY: the aligned ABI fields live in the metadata page.
        let head = unsafe { AtomicU64::from_ptr(self.base.as_ptr().add(HEAD).cast()) }
            .load(Ordering::Acquire);
        if head.wrapping_sub(self.tail) > self.data_size as u64 {
            return Err("invalid kernel perf ring head".into());
        }
        loop {
            let remaining = head.wrapping_sub(self.tail);
            if remaining == 0 {
                break;
            }
            let mut header = [0u8; 8];
            if remaining < header.len() as u64 {
                return Err("short kernel perf record header".into());
            }
            self.copy(self.tail, &mut header);
            let kind = u32::from_ne_bytes(header[..4].try_into()?);
            let misc = u16::from_ne_bytes(header[4..6].try_into()?);
            let size = u16::from_ne_bytes(header[6..].try_into()?) as usize;
            if size < header.len() || size as u64 > remaining {
                return Err("invalid kernel perf record size".into());
            }
            let mut payload = vec![0; size - header.len()];
            self.copy(self.tail.wrapping_add(header.len() as u64), &mut payload);
            self.tail = self.tail.wrapping_add(size as u64);
            if let Some(record) = decode(kind, misc, &payload, self.event)? {
                records.push(record);
            }
        }
        // SAFETY: tail is an aligned, writable ABI field in the metadata page.
        unsafe { AtomicU64::from_ptr(self.base.as_ptr().add(TAIL).cast()) }
            .store(self.tail, Ordering::Release);
        Ok(())
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        // SAFETY: this ring owns exactly this mapping.
        unsafe { libc::munmap(self.base.as_ptr().cast(), self.map_len) };
    }
}

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
    pub(crate) cpu: u32,
    pub(crate) period: u64,
    pub(crate) event: Event,
    pub(crate) registers: Option<(u64, u64, u64)>,
    pub(crate) stack: Vec<u8>,
}

#[derive(Default)]
struct EventSorter {
    pending: Vec<Record>,
    previous: u64,
}

impl EventSorter {
    fn advance(&mut self, watermark: u64, done: bool) -> VecDeque<Record> {
        // Let a different CPU publish records through the following round.
        // For equal timestamps, apply metadata before using it for a sample.
        self.pending
            .sort_by_key(|record| (record.time, matches!(record.kind, Kind::Sample(_))));
        let ready = if done {
            self.pending.len()
        } else {
            self.pending
                .partition_point(|record| record.time < self.previous)
        };
        self.previous = watermark;
        self.pending.drain(..ready).collect()
    }
}

struct Bytes<'a>(&'a [u8]);

impl<'a> Bytes<'a> {
    fn read(&mut self, count: usize) -> Result<&'a [u8]> {
        let (prefix, rest) = self
            .0
            .split_at_checked(count)
            .ok_or("short kernel perf record")?;
        self.0 = rest;
        Ok(prefix)
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_ne_bytes(self.read(8)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_ne_bytes(self.read(4)?.try_into()?))
    }
    fn name(&self) -> &'a [u8] {
        self.0.split(|&byte| byte == 0).next().unwrap_or_default()
    }
}

fn decode(kind: u32, misc: u16, bytes: &[u8], event: Event) -> Result<Option<Record>> {
    let mut payload = Bytes(bytes);
    if kind == 9 {
        let pc = payload.u64()?;
        let pid = payload.u32()?;
        let tid = payload.u32()?;
        let time = payload.u64()?;
        let cpu = payload.u32()?;
        payload.u32()?; // reserved
        let period = payload.u64()?;
        let abi = payload.u64()?;
        let registers = if abi == 0 {
            None
        } else {
            let bp = payload.u64()?;
            let second = payload.u64()?;
            let third = payload.u64()?;
            #[cfg(target_arch = "x86_64")]
            let (sp, pc_or_lr) = (second, third);
            #[cfg(target_arch = "aarch64")]
            let (sp, pc_or_lr) = {
                payload.u64()?; // PC is also the unadjusted PERF_SAMPLE_IP.
                (third, second)
            };
            (abi == 2).then_some((sp, bp, pc_or_lr))
        };
        let saved = usize::try_from(payload.u64()?)?;
        let stack = if saved > 0 {
            let raw = payload.read(saved)?;
            let count = usize::try_from(payload.u64()?)?;
            raw.get(..count)
                .ok_or("invalid perf stack capture size")?
                .to_vec()
        } else {
            Vec::new()
        };
        return Ok(Some(Record {
            time,
            pid,
            tid,
            kind: Kind::Sample(Sample {
                pc,
                cpu,
                period,
                event,
                registers,
                stack,
            }),
        }));
    }
    if !matches!(kind, 1 | 2 | 3 | 4 | 7 | 10) {
        return Ok(None);
    }
    let trailer_size = 24; // TID, TIME, CPU.
    let split = bytes
        .len()
        .checked_sub(trailer_size)
        .ok_or("short perf record trailer")?;
    let mut trailer = Bytes(&bytes[split..]);
    let pid = trailer.u32()?;
    let tid = trailer.u32()?;
    let time = trailer.u64()?;
    let mut payload = Bytes(&bytes[..split]);
    let (pid, tid, kind) = match kind {
        1 | 10 => {
            let pid = payload.u32()?;
            let tid = payload.u32()?;
            let start = payload.u64()?;
            let end = start
                .checked_add(payload.u64()?)
                .ok_or("perf mapping overflows the address space")?;
            let offset = payload.u64()?;
            let (dev, inode) = if kind == 10 {
                let major = payload.u32()?;
                let minor = payload.u32()?;
                let inode = payload.u64()?;
                payload.read(16)?; // inode generation, prot, flags.
                (Some((major, minor)), inode)
            } else {
                (None, 0)
            };
            use std::os::unix::ffi::OsStrExt;
            let path = Path::new(std::ffi::OsStr::from_bytes(payload.name())).to_owned();
            (
                pid,
                tid,
                Kind::Mapping(Mapping {
                    start,
                    end,
                    offset,
                    path,
                    dev,
                    inode,
                }),
            )
        }
        2 => {
            payload.u64()?;
            (pid, tid, Kind::Lost(payload.u64()?))
        }
        3 => {
            let pid = payload.u32()?;
            let tid = payload.u32()?;
            let name = String::from_utf8_lossy(payload.name()).into_owned();
            (
                pid,
                tid,
                Kind::Comm {
                    name,
                    exec: misc & (1 << 13) != 0,
                },
            )
        }
        4 | 7 => {
            let pid = payload.u32()?;
            payload.u32()?; // parent process
            let tid = payload.u32()?;
            let parent = payload.u32()?;
            (
                pid,
                tid,
                if kind == 4 {
                    Kind::Exit
                } else {
                    Kind::Fork { parent }
                },
            )
        }
        _ => unreachable!(),
    };
    Ok(Some(Record {
        time,
        pid,
        tid,
        kind,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perf_permission_errors_include_host_settings_for_a_blocked_syscall() {
        const CASE: &str = "linux::perf::tests::perf_permission_errors_include_host_settings_for_a_blocked_syscall";
        const CHILD: &str = "HIRSEL_PROF_TEST_PERF_DENIED";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([CASE, "--exact", "--nocapture"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            // Verify the child executed this body rather than reporting an empty filter.
            assert!(String::from_utf8_lossy(&output.stdout).contains("checked perf refusal"));
            return;
        }

        // The child blocks one syscall and leaves the host's perf policy intact.
        let mut instructions = [
            libc::sock_filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 0,
            }, // seccomp_data.nr
            libc::sock_filter {
                code: 0x15,
                jt: 0,
                jf: 1,
                k: libc::SYS_perf_event_open as u32,
            },
            libc::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: libc::SECCOMP_RET_ERRNO | libc::EACCES as u32,
            },
            libc::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: libc::SECCOMP_RET_ALLOW,
            },
        ];
        let program = libc::sock_fprog {
            len: instructions.len() as u16,
            filter: instructions.as_mut_ptr(),
        };
        // SAFETY: the filter points to initialized instructions. The changes
        // apply to this isolated test thread and its descendants.
        unsafe {
            assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
            assert_eq!(
                libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program),
                0
            );
        }
        let mut group = PerfGroup::new();
        let failure = group
            .open(
                0,
                -1,
                CPU_CLOCK,
                PerfSettings {
                    freq: 997,
                    stack_bytes: 32768,
                    sideband: false,
                    enable_on_exec: false,
                },
            )
            .unwrap_err();
        assert_eq!(failure.raw_os_error(), Some(libc::EACCES));
        let diagnostic = open_error(failure, CPU_CLOCK);
        assert!(diagnostic.contains("perf_event_paranoid="), "{diagnostic}");
        assert!(
            diagnostic.contains("seccomp filter is active"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("CAP_PERFMON"), "{diagnostic}");
        println!("checked perf refusal");
    }
}
