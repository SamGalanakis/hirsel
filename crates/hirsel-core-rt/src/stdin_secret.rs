//! `stdin.read_secret`: one echo-off line from the run's terminal, sealed by
//! the host as a Secret the program never held (core/std/api/stdin.md,
//! Secret input). The line crosses from the stdin helper into run-owned secret
//! storage; a tape records the outcome and the designation's position only.
use crate::{
    Context, entry,
    os_net::{Due, signal},
    provider::*,
    secret::{Made, Origin},
    stdin::{Stream, Until, other},
    term_restore::Armed,
};
use hirsel_core_net::secret_aead::SecretBytes;
use std::{ptr, sync::Arc};
use zeroize::Zeroizing;

/// The longest line accepted, on every host: the read is non-canonical, so no
/// host line limit applies.
pub(crate) const LINE_MAX: usize = 1024;
const EMPTY: u8 = 3;
const TOO_LONG: u8 = 4;
const EOF: u8 = 5;
const TIMED_OUT: u8 = 6;
const ABORTED: u8 = 7;
/// The policy's refusal; code 1 is Invalid and 2 Capacity.
const POLICY: u8 = 9;

pub(crate) struct SecretRequest {
    pub(crate) until: Until,
    /// The policy's canonical text, which the tape's issue holds.
    pub(crate) policy: String,
    deadline: u64,
    armed: Option<Armed>,
    line: Option<Zeroizing<Vec<u8>>>,
}

/// The keys that edit a secret line, taken from the terminal's modes before
/// the read; a key the terminal has switched off is `None`.
#[derive(Clone, Copy)]
pub(crate) struct Keys {
    erase: Option<u8>,
    kill: Option<u8>,
    eof: Option<u8>,
}

impl Keys {
    pub(crate) fn of(modes: &libc::termios) -> Self {
        let key = |n: usize| Some(modes.c_cc[n]).filter(|&c| c != crate::host::sys::VDISABLE);
        Self {
            erase: key(libc::VERASE),
            kill: key(libc::VKILL),
            eof: key(libc::VEOF),
        }
    }
}

/// The line editor a secret read runs over the terminal's non-canonical
/// input, the same on every host. It holds at most `LINE_MAX + 1` bytes and a
/// newline, in storage allocated once and wiped as it shrinks, so no copy of
/// a typed byte outlives the read.
pub(crate) struct Editor {
    keys: Keys,
    line: Zeroizing<Vec<u8>>,
    /// A byte past `LINE_MAX + 1` was typed and dropped: the line is TooLong
    /// until a kill, whatever is erased.
    lost: bool,
}

impl Editor {
    pub(crate) fn new(keys: Keys) -> Self {
        Self {
            keys,
            line: Zeroizing::new(Vec::with_capacity(LINE_MAX + 2)),
            lost: false,
        }
    }

    /// Takes the bytes the terminal delivered, and answers true once the line
    /// has ended: at a newline, which the line keeps, at the end-of-file key, or
    /// at end of input (`typed` empty). Bytes after the end are not kept.
    pub(crate) fn feed(&mut self, typed: &[u8]) -> bool {
        if typed.is_empty() {
            return true;
        }
        for &byte in typed {
            if byte == b'\n' {
                self.line.push(byte);
                return true;
            }
            if Some(byte) == self.keys.eof {
                return true;
            }
            if Some(byte) == self.keys.erase {
                if !self.lost {
                    self.erase();
                }
            } else if Some(byte) == self.keys.kill {
                zeroize::Zeroize::zeroize(&mut *self.line);
                self.lost = false;
            } else if self.line.len() <= LINE_MAX {
                self.line.push(byte);
            } else {
                self.lost = true;
            }
        }
        false
    }

    /// Removes the last character: its last byte and, when that is a UTF-8
    /// continuation byte, the bytes back to its lead byte (at most 4 in all).
    fn erase(&mut self) {
        for n in 1..=4 {
            let Some(last) = self.line.last_mut() else {
                return;
            };
            let byte = std::mem::take(last);
            self.line.pop();
            if byte & 0xc0 != 0x80 || n == 4 {
                return;
            }
        }
    }

    /// The line as it ended: its bytes, then the newline if one ended it.
    pub(crate) fn into_line(self) -> Zeroizing<Vec<u8>> {
        self.line
    }
}

fn done(outcome: Outcome) -> Step {
    Step {
        outcome,
        ..Step::default()
    }
}

fn refuse(tag: u8) -> Outcome {
    Outcome {
        error: IoError { tag, code: 0 },
        ..Outcome::default()
    }
}

/// The line in `bytes`: up to a newline, without it or a `\r` before it.
fn line(read: &mut SecretRequest, bytes: &[u8]) -> Outcome {
    let Some(last) = bytes.last() else {
        return refuse(EOF);
    };
    let body = if *last == b'\n' {
        let body = &bytes[..bytes.len() - 1];
        body.strip_suffix(b"\r").unwrap_or(body)
    } else {
        bytes
    };
    if body.len() > LINE_MAX {
        refuse(TOO_LONG)
    } else if body.is_empty() {
        refuse(EMPTY)
    } else {
        read.line = Some(Zeroizing::new(body.to_vec()));
        Outcome::default()
    }
}

/// The OS provider's issue and resume of `STDIN_SECRET`.
///
/// # Safety
/// The adapter pins the request and its `SecretRequest`, and the live OS
/// grant's handle is the run's `Stream`.
pub(crate) unsafe fn step(request: *mut Request, token: u64, issue: bool) -> Step {
    // SAFETY: as above; the helper never borrows them.
    unsafe {
        let request = &mut *request;
        let read = &mut *request.path.cast_mut().cast::<SecretRequest>();
        let stream = &*(request.handle as *const Stream);
        let due = crate::os_net::deadline(read.until, token);
        if issue {
            if due == Due::Passed {
                return done(signal(TIMED_OUT));
            }
            read.deadline = due.host().unwrap_or(0);
            match stream.begin_secret(token) {
                Ok(armed) => read.armed = Some(armed),
                Err(outcome) => return done(outcome),
            }
        }
        let outcome = match stream.take_secret(token, read.deadline, &mut read.armed) {
            Some(Ok(bytes)) => line(read, &bytes),
            Some(Err(code)) => other(code),
            None if due == Due::Passed => {
                stream.cancel_secret(token, &mut read.armed);
                signal(TIMED_OUT)
            }
            None => {
                return Step {
                    kind: STEP_PENDING,
                    deadline_ns: read.deadline,
                    ..Step::default()
                };
            }
        };
        done(outcome)
    }
}

/// # Safety
/// A pending `STDIN_SECRET` pins its request and stream until abort.
pub(crate) unsafe fn abort(request: *mut Request, token: u64, reason: u8) -> Outcome {
    // SAFETY: as above.
    unsafe {
        let request = &mut *request;
        let read = &mut *request.path.cast_mut().cast::<SecretRequest>();
        let stream = &*(request.handle as *const Stream);
        let delivered = if reason == ABORT_DEADLINE {
            stream.take_secret(token, read.deadline, &mut read.armed)
        } else {
            None
        };
        let outcome = match delivered {
            Some(Ok(bytes)) => line(read, &bytes),
            Some(Err(code)) => other(code),
            None => {
                stream.cancel_secret(token, &mut read.armed);
                signal(if reason == ABORT_CANCEL {
                    ABORTED
                } else {
                    TIMED_OUT
                })
            }
        };
        outcome
    }
}

/// `stdin.read_secret(i, uses, until)`. The policy and the charge are decided
/// before the provider, so their refusals pass no replay point.
///
/// # Safety
/// The grants belong to the live run and `uses` is a pinned initialized String
/// carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_stdin_read_secret(
    context: *mut Context,
    grant: u64,
    uses: *const Buffer,
    until_kind: u8,
    clock: u64,
    at: u64,
    site: u64,
) -> Outcome {
    // SAFETY: forwarded from the checked call; the request lives on this frame
    // until the provider settles it.
    unsafe {
        crate::process::deny_describe_capability(context);
        let Some(policy) = crate::secret::policy(uses) else {
            return Outcome {
                error: IoError {
                    tag: POLICY,
                    code: 1,
                },
                ..Outcome::default()
            };
        };
        let charge = match crate::secret::admit((*context).payer(), &policy.send, LINE_MAX as u64) {
            Ok(charge) => charge,
            Err(refused) => {
                return Outcome {
                    error: IoError {
                        tag: POLICY,
                        code: 2,
                    },
                    ..refused
                };
            }
        };
        let mut read = SecretRequest {
            until: crate::net::deadline(context, until_kind, clock, at),
            policy: policy.text(),
            deadline: 0,
            armed: None,
            line: None,
        };
        let mut request = Request {
            operation: STDIN_SECRET,
            mode: 0,
            handle: entry::hirsel_core_capability_handle(context, grant),
            path: ptr::from_mut(&mut read).cast(),
            path_len: 0,
            buffer: Buffer {
                data: ptr::null_mut(),
                len: 0,
                capacity: 0,
            },
            site,
        };
        let provider = entry::hirsel_core_capability_provider(context, grant);
        let answer = crate::provider::drive(context, provider, &mut request, None);
        read.armed.take();
        if answer.status != 0 || answer.error != IoError::OK {
            return answer;
        }
        // Under a tape the value is the delivery's position; a replay has no
        // bytes behind the designation, as for a granted secret.
        let bytes = read
            .line
            .take()
            .map(|line| Arc::new(SecretBytes::new(line)));
        crate::secret::store_sealed(
            context,
            policy,
            bytes,
            charge,
            Some(Origin::here(Made::Prompted(answer.value))),
        )
    }
}
