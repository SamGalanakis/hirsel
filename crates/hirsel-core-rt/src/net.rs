//! NET-6 checks precede issue. W1a declares the provider but makes no OS attempt.
use crate::{
    Context,
    entry::{hirsel_core_capability_handle, hirsel_core_capability_provider},
    provider::*,
};
use hirsel_core_net::Rule;
use std::ptr;

#[repr(C)]
pub struct Rules {
    pub data: *const Rule,
    pub len: usize,
}
impl Rules {
    /// # Safety
    /// The run pins the configured or narrowed immutable rule list.
    pub unsafe fn as_slice(&self) -> &[Rule] {
        if self.len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.data, self.len) }
        }
    }
}
pub(crate) struct Grant {
    _storage: Box<[Rule]>,
    _view: Box<Rules>,
    payer: crate::limit::Payer,
    charge: u64,
}
impl Drop for Grant {
    fn drop(&mut self) {
        self.payer.refund(self.charge);
    }
}
#[repr(C)]
pub(crate) struct Address {
    family: u8,
    hi: u64,
    lo: u64,
    port: u16,
}
pub(crate) unsafe fn permitted(request: &Request) -> bool {
    // SAFETY: network adapters pin the scalar address and the grant's Rules.
    unsafe {
        let address = &*request.path.cast::<Address>();
        let rules = &*(request.handle as *const Rules);
        rules.as_slice().iter().any(|r| {
            r.permits(
                u8::from(request.operation == LISTEN),
                address.family,
                address.hi,
                address.lo,
                address.port,
            )
        })
    }
}
fn buffer() -> Buffer {
    Buffer {
        data: ptr::null_mut(),
        len: 0,
        capacity: 0,
    }
}
/// # Safety
/// Live run, typed Net grant, borrowed Until carrier and source site.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_connect(
    context: *mut Context,
    n: u64,
    family: u8,
    hi: u64,
    lo: u64,
    port: u16,
    until: *const u8,
    site: u64,
) -> Outcome {
    unsafe { address_op(context, CONNECT, n, family, hi, lo, port, until, 0, site) }
}
/// # Safety
/// Live run and typed Net grant.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_listen(
    context: *mut Context,
    n: u64,
    family: u8,
    hi: u64,
    lo: u64,
    port: u16,
    backlog: u16,
    site: u64,
) -> Outcome {
    unsafe {
        address_op(
            context,
            LISTEN,
            n,
            family,
            hi,
            lo,
            port,
            ptr::null(),
            backlog,
            site,
        )
    }
}
#[allow(clippy::too_many_arguments)]
unsafe fn address_op(
    context: *mut Context,
    op: u8,
    n: u64,
    family: u8,
    hi: u64,
    lo: u64,
    port: u16,
    _until: *const u8,
    _backlog: u16,
    site: u64,
) -> Outcome {
    unsafe {
        let address = Address {
            family,
            hi,
            lo,
            port,
        };
        let mut request = Request {
            operation: op,
            mode: 0,
            handle: hirsel_core_capability_handle(context, n),
            path: (&address as *const Address).cast(),
            path_len: size_of::<Address>(),
            buffer: buffer(),
            site,
        };
        hirsel_core_host(
            context,
            hirsel_core_capability_provider(context, n),
            &mut request,
        )
    }
}
unsafe fn owned_op(
    context: *mut Context,
    op: u8,
    key: u64,
    _operand: *mut Buffer,
    _until: *const u8,
    site: u64,
) -> Outcome {
    unsafe {
        let mut request = Request {
            operation: op,
            mode: 0,
            handle: key,
            path: ptr::null(),
            path_len: 0,
            buffer: buffer(),
            site,
        };
        hirsel_core_host(context, (*(*context).run).provider, &mut request)
    }
}
/// # Safety
/// Live listener and Until carrier, borrowed through delivery.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_accept(
    context: *mut Context,
    key: u64,
    until: *const u8,
    site: u64,
) -> Outcome {
    unsafe { owned_op(context, ACCEPT, key, ptr::null_mut(), until, site) }
}
/// # Safety
/// Live read half, mutable caller buffer and borrowed Until carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_recv(
    context: *mut Context,
    key: u64,
    operand: *mut Buffer,
    until: *const u8,
    site: u64,
) -> Outcome {
    unsafe { owned_op(context, RECV, key, operand, until, site) }
}
/// # Safety
/// Live write half, immutable caller buffer and borrowed Until carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_send(
    context: *mut Context,
    key: u64,
    operand: *mut Buffer,
    until: *const u8,
    site: u64,
) -> Outcome {
    unsafe { owned_op(context, SEND, key, operand, until, site) }
}
/// # Safety
/// Consumes the sole Socket carrier. No allocation, admission or replay point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_split(
    context: *mut Context,
    key: u64,
    _site: u64,
) -> Outcome {
    unsafe {
        let (read, write) = (*(*context).host_files).sockets.split(key);
        Outcome {
            value: read,
            written: write,
            ..Outcome::default()
        }
    }
}
/// # Safety
/// Consumes one live socket, directional half or listener carrier exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_socket_drop(context: *mut Context, key: u64) {
    unsafe {
        (*(*context).host_files).sockets.release(key);
    }
}
/// # Safety
/// Live run and typed Net grant. Rule scalars come from the caller's Rule.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_narrow(
    context: *mut Context,
    n: u64,
    dir: u8,
    family: u8,
    hi: u64,
    lo: u64,
    bits: u8,
    first: u16,
    last: u16,
    _site: u64,
) -> Outcome {
    let rule = Rule {
        dir,
        family,
        hi,
        lo,
        bits,
        first,
        last,
    };
    if !rule.valid() {
        return Outcome {
            error: IoError { tag: 1, code: 0 },
            ..Outcome::default()
        };
    }
    unsafe {
        let provider = *hirsel_core_capability_provider(context, n);
        let rules = &*(hirsel_core_capability_handle(context, n) as *const Rules);
        let rules = rules.as_slice();
        let count = rules.iter().filter_map(|r| r.intersect(rule)).count();
        let charge = (count as u64)
            .checked_mul(37)
            .and_then(|n| n.checked_add(7))
            .map(|n| n & !7);
        let payer = (*context).payer(None);
        if let Err(capacity) = payer.admit(charge) {
            return Outcome {
                value: capacity.requested,
                error: IoError { tag: 2, code: 0 },
                written: capacity.available,
                status: 0,
            };
        }
        let storage: Box<[Rule]> = rules.iter().filter_map(|r| r.intersect(rule)).collect();
        let view = Box::new(Rules {
            data: storage.as_ptr(),
            len: storage.len(),
        });
        let handle = (&*view as *const Rules) as u64;
        let registry = &mut *(*context).host_files;
        registry.net_grants.push(Grant {
            _storage: storage,
            _view: view,
            payer,
            charge: charge.unwrap(),
        });
        Outcome {
            value: registry.grant(provider, handle),
            ..Outcome::default()
        }
    }
}
