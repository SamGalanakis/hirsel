//! A served `db@1` place's link between its runner host and a head that runs in its own
//! process (runners.md `db@1`, RUN-8): the host writes each served call it admits as an
//! arrival, and the head writes each end back. The host checks every call before it
//! arrives (RUN-8 checks 1, 2, 4, 6 and 7); the head checks the entries it serves.
//!
//! A message is a little-endian u32 body length, then the body: a tag byte, the arrival's
//! u64 id, and the arrival's call or the end's outcome. Strings and byte strings are a u32
//! length and their bytes. The head's process ending closes the place.
use crate::{CallForm, CallOutcome};
use std::io::{Read, Write};

/// A served call as the head's `arrival.request` states it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServedCall {
    pub form: CallForm,
    pub path: String,
    pub fingerprint: [u8; 32],
    pub code: [u8; 32],
    pub caller: String,
    pub stream: [u8; 32],
    pub epoch: u64,
    pub position: u64,
    pub digest: [u8; 32],
    pub acknowledged: u64,
    pub level: Option<String>,
    pub argument: Vec<u8>,
}

/// One message between a place's host and its head.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlaceMessage {
    /// Host to head: served call `id` arrives.
    Arrive(u64, ServedCall),
    /// Head to host: served call `id` ended.
    End(u64, CallOutcome),
}

/// The largest message body either side reads: the host's frame budget bounds a call's
/// argument and an end's bytes well below it.
const MAX_BODY: u32 = 1 << 30;

const ARRIVE: u8 = 0;
const END: u8 = 1;
const FORMS: [CallForm; 4] = [
    CallForm::Compute,
    CallForm::Call,
    CallForm::Transact,
    CallForm::Query,
];

impl PlaceMessage {
    /// Write this message to `out`.
    pub fn write(&self, out: &mut impl Write) -> std::io::Result<()> {
        let mut body = Vec::new();
        match self {
            Self::Arrive(id, call) => {
                body.push(ARRIVE);
                body.extend(id.to_le_bytes());
                body.push(FORMS.iter().position(|f| *f == call.form).unwrap() as u8);
                bytes(&mut body, call.path.as_bytes());
                body.extend(call.fingerprint);
                body.extend(call.code);
                bytes(&mut body, call.caller.as_bytes());
                body.extend(call.stream);
                body.extend(call.epoch.to_le_bytes());
                body.extend(call.position.to_le_bytes());
                body.extend(call.digest);
                body.extend(call.acknowledged.to_le_bytes());
                match &call.level {
                    None => body.push(0),
                    Some(level) => {
                        body.push(1);
                        bytes(&mut body, level.as_bytes());
                    }
                }
                bytes(&mut body, &call.argument);
            }
            Self::End(id, outcome) => {
                body.push(END);
                body.extend(id.to_le_bytes());
                let (tag, payload) = match outcome {
                    CallOutcome::Ok(b) => (0, Ok(b)),
                    CallOutcome::Failed(b) => (1, Ok(b)),
                    CallOutcome::Faulted(b) => (2, Ok(b)),
                    CallOutcome::NotRun(r) => (3, Err(r)),
                    CallOutcome::InDoubt(r) => (4, Err(r)),
                    CallOutcome::Canceled(r) => (5, Err(r)),
                };
                body.push(tag);
                match payload {
                    Ok(b) => bytes(&mut body, b),
                    Err(reason) => body.extend(reason.to_le_bytes()),
                }
            }
        }
        let length = u32::try_from(body.len())
            .ok()
            .filter(|n| *n <= MAX_BODY)
            .ok_or_else(|| std::io::Error::other("a place message past its budget"))?;
        out.write_all(&length.to_le_bytes())?;
        out.write_all(&body)?;
        out.flush()
    }

    /// Read the next message from `input`: `None` at a clean end of the stream, before a
    /// message starts.
    pub fn read(input: &mut impl Read) -> std::io::Result<Option<Self>> {
        let mut length = [0; 4];
        let mut filled = 0;
        while filled < 4 {
            match input.read(&mut length[filled..])? {
                0 if filled == 0 => return Ok(None),
                0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                n => filled += n,
            }
        }
        let length = u32::from_le_bytes(length);
        if length > MAX_BODY {
            return Err(malformed());
        }
        let mut body = vec![0; length as usize];
        input.read_exact(&mut body)?;
        let mut body = Body(&body);
        let message = match body.byte()? {
            ARRIVE => {
                let id = body.u64()?;
                let form = *FORMS.get(body.byte()? as usize).ok_or_else(malformed)?;
                let call = ServedCall {
                    form,
                    path: body.string()?,
                    fingerprint: body.array()?,
                    code: body.array()?,
                    caller: body.string()?,
                    stream: body.array()?,
                    epoch: body.u64()?,
                    position: body.u64()?,
                    digest: body.array()?,
                    acknowledged: body.u64()?,
                    level: match body.byte()? {
                        0 => None,
                        1 => Some(body.string()?),
                        _ => return Err(malformed()),
                    },
                    argument: body.bytes()?.to_vec(),
                };
                Self::Arrive(id, call)
            }
            END => {
                let id = body.u64()?;
                let outcome = match body.byte()? {
                    0 => CallOutcome::Ok(body.bytes()?.to_vec()),
                    1 => CallOutcome::Failed(body.bytes()?.to_vec()),
                    2 => CallOutcome::Faulted(body.bytes()?.to_vec()),
                    3 => CallOutcome::NotRun(body.u32()?),
                    4 => CallOutcome::InDoubt(body.u32()?),
                    5 => CallOutcome::Canceled(body.u32()?),
                    _ => return Err(malformed()),
                };
                Self::End(id, outcome)
            }
            _ => return Err(malformed()),
        };
        if !body.0.is_empty() {
            return Err(malformed());
        }
        Ok(Some(message))
    }
}

fn bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend((bytes.len() as u32).to_le_bytes());
    out.extend(bytes);
}

fn malformed() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, "a malformed place message")
}

struct Body<'a>(&'a [u8]);
impl<'a> Body<'a> {
    fn take(&mut self, n: usize) -> std::io::Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(malformed());
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn byte(&mut self) -> std::io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> std::io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> std::io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn array(&mut self) -> std::io::Result<[u8; 32]> {
        Ok(self.take(32)?.try_into().unwrap())
    }
    fn bytes(&mut self) -> std::io::Result<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    fn string(&mut self) -> std::io::Result<String> {
        String::from_utf8(self.bytes()?.to_vec()).map_err(|_| malformed())
    }
}
