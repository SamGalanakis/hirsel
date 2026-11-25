//! TLS-6. A SCRAM proof is only a host send buffer.
use crate::{
    Context,
    host_file::Files,
    limit::Payer,
    net::{self, NetworkRequest},
    provider::*,
    secret::{Secret, SendRequest},
    tls::bytes,
};
use hirsel_core_net::scram::{self, Binding, Transcript};

pub(crate) const CLOSE_SCRAM: u8 = u8::MAX;
pub(crate) const SIGNATURE_BYTES: u64 = 32;

/// NetworkRequest and SendRequest are the leading prefixes consumed by the
/// socket providers. Transcript inputs remain separate from the proof buffer.
#[repr(C)]
pub(crate) struct ScramRequest<'a> {
    pub(crate) send: SendRequest,
    derive: fn(&mut Self),
    pub(crate) client_first: &'a [u8],
    pub(crate) server_first: &'a [u8],
    transcript: Transcript<'a>,
    password: Option<&'a [u8]>,
    message: Vec<u8>,
    signature: Option<[u8; 32]>,
}

impl ScramRequest<'_> {
    fn derive(&mut self) {
        let (message, signature) = self.transcript.derive(self.password.take().unwrap());
        self.message = message;
        self.signature = Some(signature);
        self.send.secret = view(&self.message);
    }
}

/// A live provider receives the proof through SendRequest. Replay never
/// invokes a live provider and therefore never derives it.
/// # Safety
/// The request points at a pinned ScramRequest and this is its issue callback.
pub(crate) unsafe fn issue(request: &Request) {
    // SAFETY: the SCRAM adapter pins the request and installs the callback.
    let scram = unsafe { &mut *request.path.cast_mut().cast::<ScramRequest<'_>>() };
    (scram.derive)(scram);
}

struct Charge(Payer);
impl Drop for Charge {
    fn drop(&mut self) {
        self.0.refund(SIGNATURE_BYTES);
    }
}

struct State {
    signature: Option<[u8; 32]>,
    #[cfg(feature = "record")]
    taped: Option<u64>,
    _charge: Charge,
}

pub(crate) unsafe fn drop_state(handle: u64) {
    // SAFETY: a Scram's registry record removes sole ownership before cleanup.
    drop(unsafe { Box::from_raw(handle as *mut State) });
}

fn view(bytes: &[u8]) -> Buffer {
    Buffer {
        data: bytes.as_ptr().cast_mut(),
        len: bytes.len(),
        capacity: 0,
    }
}

fn refusal(tag: u8) -> Outcome {
    Outcome {
        error: IoError { tag, code: 0 },
        ..Outcome::default()
    }
}
fn net_error(outcome: Outcome) -> Outcome {
    if outcome.status != 0 {
        return outcome;
    }
    Outcome {
        error: IoError {
            tag: 5,
            code: outcome.error.code,
        },
        written: u64::from(outcome.error.tag),
        ..Outcome::default()
    }
}

unsafe fn check_tls(
    files: *mut Files,
    key: u64,
    secret: Option<&Secret>,
) -> Result<(Binding, bool), Outcome> {
    unsafe {
        let write = (*files).tls_writes.get(&(key & !3)).ok_or(net::DENIED)?;
        // A server half has no verified name (TLS-9), with or without a secret.
        let name = write.name.as_ref().ok_or(net::DENIED)?;
        if let Some(secret) = secret
            && !secret
                .policy
                .send
                .iter()
                .any(|pattern| pattern.covers(name))
        {
            return Err(net::DENIED);
        }
        Ok((write.binding, write.spent))
    }
}

/// # Safety
/// The half and both initialized carriers are live borrowed operands.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_scram_final_len(
    context: *mut Context,
    key: u64,
    client_first: *const Buffer,
    server_first: *const Buffer,
    _site: u64,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let (binding, _) = match check_tls((*context).host_files, key, None) {
            Ok(record) => record,
            Err(outcome) => return outcome,
        };
        match Transcript::parse(bytes(&*client_first), bytes(&*server_first), &binding) {
            Ok(transcript) => Outcome {
                value: transcript.final_len() as u64,
                ..Outcome::default()
            },
            Err(error) => refusal(error as u8),
        }
    }
}

/// # Safety
/// The write half is exclusively borrowed and the four carriers are pinned.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_scram_sha256(
    context: *mut Context,
    key: u64,
    secret: u64,
    client_first: *const Buffer,
    server_first: *const Buffer,
    before: *const Buffer,
    after: *const Buffer,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let files = (*context).host_files;
        let served = crate::provide::owns(&net::socket_provider(context, key));
        let secret = crate::secret::resolve(context, secret);
        // A served stream never satisfies a granted Secret (TLS-6).
        if served && secret.origin.opaque() {
            return net::DENIED;
        }
        let (binding, spent) = match check_tls(files, key, Some(&secret)) {
            Ok(record) => record,
            Err(outcome) => return outcome,
        };
        let client_first = bytes(&*client_first);
        let server_first = bytes(&*server_first);
        let transcript = match Transcript::parse(client_first, server_first, &binding) {
            Ok(transcript) => transcript,
            Err(error) => return refusal(error as u8),
        };
        if !crate::steps::take_many(context, transcript.iterations().into(), site) {
            return CANCELLED;
        }
        let payer = (*context).payer();
        if let Err(capacity) = payer.admit(Some(SIGNATURE_BYTES)) {
            return Outcome {
                value: capacity.requested,
                written: capacity.available,
                ..refusal(6)
            };
        }
        let charge = Charge(payer);
        if spent {
            return net_error(net::RESET);
        }
        if served {
            let Some(password) = secret.bytes.as_deref() else {
                return net::DENIED;
            };
            let (message, signature) = transcript.derive(password);
            let message = zeroize::Zeroizing::new(message);
            let sent = served_proof(
                context,
                key,
                [bytes(&*before), &message, bytes(&*after)],
                [until_kind.into(), clock, deadline_ns, site],
            );
            if let Err(outcome) = sent {
                return net_error(outcome);
            }
            let state = Box::new(State {
                signature: Some(signature),
                #[cfg(feature = "record")]
                taped: None,
                _charge: charge,
            });
            let value =
                (*files)
                    .registry
                    .held(VERIFY_PROVIDER, Box::into_raw(state) as u64, CLOSE_SCRAM);
            return Outcome {
                value,
                ..Outcome::default()
            };
        }
        let until = net::deadline(context, until_kind, clock, deadline_ns);
        let password = secret.bytes.as_deref().map(|bytes| bytes.as_slice());
        let mut scram = ScramRequest {
            send: SendRequest {
                network: NetworkRequest {
                    send_tail: after,
                    address: net::Address::default(),
                    sockets: &raw mut (*files).sockets,
                    until,
                    backlog: 0,
                    connecting: None,
                    denial: None,
                    public: false,
                    direction: 2,
                    os: &crate::os_net::OPERATIONS,
                    tls: &raw mut (*files).tls_sessions,
                    tls_send_pending: false,
                    max_receive: 0,
                },
                secret: net::empty_buffer(),
                designations: vec![(secret.origin, secret.policy.send.clone())],
                committed: 0,
                narrowed: None,
            },
            derive: ScramRequest::derive,
            client_first,
            server_first,
            transcript,
            password,
            message: Vec::new(),
            signature: None,
        };
        let request = Request {
            operation: SCRAM,
            mode: 0,
            handle: key,
            path: (&raw mut scram).cast(),
            path_len: size_of::<ScramRequest<'_>>(),
            buffer: *before,
            site,
        };
        let outcome = net::send_sink(context, request);
        if outcome.status != 0 || outcome.error != IoError::OK {
            return net_error(outcome);
        }
        let state = Box::new(State {
            signature: scram.signature,
            #[cfg(feature = "record")]
            taped: (*files).net_tape.is_some().then_some(outcome.value),
            _charge: charge,
        });
        let value =
            (*files)
                .registry
                .held(VERIFY_PROVIDER, Box::into_raw(state) as u64, CLOSE_SCRAM);
        Outcome {
            value,
            ..Outcome::default()
        }
    }
}

/// A served stream's proof reaches its server only as Send calls (core.md,
/// Net authority; NOTES 33): `before ‖ proof ‖ after`, offered again from the
/// first byte not yet taken until every byte is. A failure spends the half,
/// since the server reports no committed count. Its verdict is the run's own,
/// never taped.
unsafe fn served_proof(
    context: *mut Context,
    key: u64,
    parts: [&[u8]; 3],
    [until_kind, clock, deadline_ns, site]: [u64; 4],
) -> Result<(), Outcome> {
    unsafe {
        let mut joined = zeroize::Zeroizing::new(parts.concat());
        let mut at = 0;
        while at < joined.len() {
            let left = joined.len() - at;
            let mut head = Buffer {
                data: joined.as_mut_ptr().add(at),
                len: left,
                capacity: left,
            };
            let tail = net::empty_buffer();
            let outcome = net::owned_op(
                context,
                SEND_PAIR,
                key,
                &mut head,
                &tail,
                0,
                until_kind as u8,
                clock,
                deadline_ns,
                site,
            )
            .outcome();
            if outcome.status != 0 || outcome.error != IoError::OK {
                net::spend_write((*context).host_files, key);
                return Err(outcome);
            }
            at += (outcome.value as usize).min(left);
        }
        Ok(())
    }
}

pub(crate) const VERIFY_PROVIDER: Provider = Provider {
    issue: verify_issue,
    ..crate::secret::MAC_PROVIDER
};

unsafe extern "C" fn verify_issue(_: *mut u8, request: *mut Request, _: u64) -> Step {
    let value = unsafe {
        let r = &*request;
        let signature = &*r.path.cast::<[u8; 32]>();
        u64::from(scram::verify(signature, bytes(&r.buffer)))
    };
    Step {
        outcome: Outcome {
            value,
            ..Outcome::default()
        },
        ..Step::default()
    }
}

/// # Safety
/// Consumes one live Scram; the final-message carrier is initialized and pinned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_scram_verify(
    context: *mut Context,
    scram: u64,
    server_final: *const Buffer,
    site: u64,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let files = (*context).host_files;
        let (_, handle) = (*files).registry.take_held(scram);
        let state = Box::from_raw(handle as *mut State);
        #[cfg(feature = "record")]
        if let Some(handle) = state.taped {
            let tape = (*files).secret_mac_tape.unwrap();
            let mut request = Request {
                operation: SCRAM_VERIFY,
                mode: 0,
                handle,
                path: state
                    .signature
                    .as_ref()
                    .map_or(std::ptr::null(), |signature| {
                        std::ptr::from_ref(signature).cast()
                    }),
                path_len: 0,
                buffer: *server_final,
                site,
            };
            return crate::provider::drive(context, &tape.provider, &mut request, None);
        }
        let _ = site;
        let value = u64::from(scram::verify(
            state.signature.as_ref().unwrap(),
            bytes(&*server_final),
        ));
        Outcome {
            value,
            ..Outcome::default()
        }
    }
}
