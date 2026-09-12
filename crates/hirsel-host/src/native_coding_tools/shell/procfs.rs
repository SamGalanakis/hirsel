use std::{ffi::OsStr, fs, io, os::unix::ffi::OsStrExt};

#[derive(Debug)]
pub(super) struct ProcessGroupMember {
    pub(super) pid: u32,
    pub(super) state: char,
}

pub(super) fn process_group_members(pgid: libc::pid_t) -> io::Result<Vec<ProcessGroupMember>> {
    let mut members = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = parse_proc_pid(&entry.file_name()) else {
            continue;
        };
        let stat = match fs::read(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if process_disappeared(&error) => continue,
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("failed to read /proc/{pid}/stat: {error}"),
                ));
            }
        };
        let member = parse_process_stat(pid, &stat)?;
        if member.pgid == pgid {
            members.push(ProcessGroupMember {
                pid,
                state: member.state,
            });
        }
    }
    Ok(members)
}

fn parse_proc_pid(name: &OsStr) -> Option<u32> {
    parse_ascii_number(name.as_bytes()).ok()
}

fn process_disappeared(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
}

struct ParsedProcessStat {
    state: char,
    pgid: libc::pid_t,
}

fn parse_process_stat(pid: u32, stat: &[u8]) -> io::Result<ParsedProcessStat> {
    let delimiter = stat
        .windows(2)
        .rposition(|window| window == b") ")
        .ok_or_else(|| malformed_stat(pid, "missing final command-name delimiter"))?;
    let mut fields = stat[delimiter + 2..]
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty());

    let state = match fields.next() {
        Some([state]) if state.is_ascii_alphabetic() => char::from(*state),
        _ => return Err(malformed_stat(pid, "invalid ASCII process state")),
    };
    let parent_pid = fields
        .next()
        .ok_or_else(|| malformed_stat(pid, "missing parent PID"))?;
    parse_ascii_number::<libc::pid_t>(parent_pid)
        .map_err(|_| malformed_stat(pid, "invalid ASCII parent PID"))?;
    let process_group = fields
        .next()
        .ok_or_else(|| malformed_stat(pid, "missing process-group ID"))?;
    let pgid = parse_ascii_number(process_group)
        .map_err(|_| malformed_stat(pid, "invalid ASCII process-group ID"))?;

    Ok(ParsedProcessStat { state, pgid })
}

fn parse_ascii_number<T>(field: &[u8]) -> Result<T, ()>
where
    T: std::str::FromStr,
{
    std::str::from_utf8(field)
        .map_err(|_| ())?
        .parse()
        .map_err(|_| ())
}

fn malformed_stat(pid: u32, detail: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("malformed /proc/{pid}/stat: {detail}"),
    )
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::{parse_process_stat, process_disappeared};

    #[test]
    fn stat_parser_treats_command_name_as_arbitrary_bytes() {
        let parsed =
            parse_process_stat(42, b"42 (name\xff) with ) delimiters) S 7 11 11 0").unwrap();
        assert_eq!(parsed.state, 'S');
        assert_eq!(parsed.pgid, 11);
    }

    #[test]
    fn stat_parser_rejects_malformed_required_fields() {
        for stat in [
            b"42 (name) SS 7 11".as_slice(),
            b"42 (name) S parent 11",
            b"42 (name) S 7 group",
            b"42 (unterminated",
        ] {
            assert!(parse_process_stat(42, stat).is_err(), "accepted {stat:?}");
        }
    }

    #[test]
    fn only_process_disappearance_is_suppressed_during_a_scan() {
        assert!(process_disappeared(&io::Error::from_raw_os_error(
            libc::ENOENT
        )));
        assert!(process_disappeared(&io::Error::from_raw_os_error(
            libc::ESRCH
        )));
        assert!(!process_disappeared(&io::Error::from_raw_os_error(
            libc::EACCES
        )));
        assert!(!process_disappeared(&io::Error::new(
            io::ErrorKind::InvalidData,
            "malformed stat"
        )));
    }
}
