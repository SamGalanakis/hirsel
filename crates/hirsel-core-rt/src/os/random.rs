//! getrandom(2) into a request's spare capacity or a caller's buffer.
use super::Request;
use crate::provider::Outcome;
use std::io;

#[cfg(test)]
type Read = fn(*mut u8, usize, u32) -> io::Result<usize>;

#[cfg(test)]
thread_local! {
    static READ: std::cell::Cell<Option<Read>> = const { std::cell::Cell::new(None) };
}

pub(super) fn issue(request: &mut Request) -> Outcome {
    struct Pinned<'a>(&'a mut Request);
    // SAFETY: the unit is parked, with exclusive access to the pinned request
    // and its buffer transferred to the helper until it returns.
    unsafe impl Send for Pinned<'_> {}

    // Only an early boot's unseeded pool can wait.
    let result = match draw(request, hirsel_core_net::sys::GRND_NONBLOCK) {
        Err(error) if error.raw_os_error() == Some(libc::EAGAIN) => {
            let pinned = Pinned(request);
            crate::units::off_slot(move || {
                let pinned = pinned;
                draw(pinned.0, 0)
            })
        }
        result => result,
    };
    // host.random has no error result. A failed draw ends the run before it
    // can publish bytes or record a delivery.
    result.unwrap_or_else(|error| crate::process::stop(format!("host.random: {error}").as_bytes()))
}

fn draw(request: &mut Request, flags: u32) -> io::Result<Outcome> {
    let spare = request.buffer.capacity.saturating_sub(request.buffer.len);
    if spare != 0 {
        // SAFETY: the request pins the writable spare capacity, which may be uninitialized.
        let data = unsafe { request.buffer.data.add(request.buffer.len) };
        fill(data, spare, flags)?;
    }
    request.buffer.len += spare;
    Ok(Outcome {
        written: spare as u64,
        ..Outcome::default()
    })
}

pub(super) fn fill(mut data: *mut u8, mut remaining: usize, flags: u32) -> io::Result<()> {
    while remaining != 0 {
        let n = read(data, remaining, flags)?;
        if n == 0 {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        data = data.wrapping_add(n);
        remaining -= n;
    }
    Ok(())
}

fn read(data: *mut u8, len: usize, flags: u32) -> io::Result<usize> {
    #[cfg(test)]
    if let Some(read) = READ.get() {
        return read(data, len, flags);
    }
    // SAFETY: fill's callers provide a live writable range of the given length.
    super::syscall(unsafe { hirsel_core_net::sys::getrandom(data.cast(), len, flags) } as isize)
        .map(|n| n as usize)
}

#[cfg(test)]
#[test]
fn host_random_failure_ends_run() {
    use super::{OsProvider, probe};
    use crate::provider::{Buffer, RANDOM};

    const CASE: &str = "HIRSEL_TEST_HOST_RANDOM_FAILURE";
    const DELIVERY: &str = "host.random reached delivery";

    if let Ok(case) = std::env::var(CASE) {
        READ.set(Some(|data, len, _| {
            if std::env::var(CASE).as_deref() == Ok("partial") && len == 32 {
                // SAFETY: the syscall seam was given 32 writable bytes.
                unsafe { data.write_bytes(0x5a, 8) };
                Ok(8)
            } else {
                Err(io::Error::from_raw_os_error(libc::EIO))
            }
        }));
        assert!(case == "immediate" || case == "partial");
        let mut bytes = [0_u8; 32];
        let mut request = Request {
            operation: RANDOM,
            mode: 0,
            handle: 0,
            path: std::ptr::null(),
            path_len: 0,
            buffer: Buffer {
                data: bytes.as_mut_ptr(),
                len: 0,
                capacity: bytes.len(),
            },
            site: 0,
        };
        let mut os = OsProvider::from_streams(None, None);
        let provider = os.provider();
        probe::CALLS.set(Some(Vec::new()));
        // SAFETY: os and the exclusive request buffer stay pinned through delivery.
        unsafe {
            let step = (provider.issue)(provider.state, &mut request, 0);
            (provider.deliver)(provider.state, &mut request, step.outcome);
        }
        if probe::CALLS
            .take()
            .unwrap()
            .iter()
            .any(|c| c.0 == probe::DELIVER)
        {
            eprintln!("{DELIVERY}");
        }
        return;
    }

    for case in ["immediate", "partial"] {
        let output = crate::test_process::command()
            .args([
                "--exact",
                "os::random::host_random_failure_ends_run",
                "--nocapture",
            ])
            .env(CASE, case)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains(DELIVERY), "{case}: {stderr}");
        assert_eq!(output.status.code(), Some(102), "{case}: {stderr}");
        let error = io::Error::from_raw_os_error(libc::EIO);
        assert!(
            stderr.contains(&format!("host.random: {error}")),
            "{case}: {stderr}"
        );
    }
}
