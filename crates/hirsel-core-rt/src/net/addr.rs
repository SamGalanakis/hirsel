use super::*;
use hirsel_core_net::{AddressDirection, Rule, RuleRow};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Check an address before issue. A tape records the denial for its inner grant
/// and delivers it at its ordinary provider boundary.
pub(crate) unsafe fn authorize(
    context: *mut Context,
    provider: &Provider,
    request: &mut Request,
) -> Option<Outcome> {
    if unix_operation(request.operation) {
        // SAFETY: the adapter pins this extension through the provider call.
        return unsafe { !(*request.path.cast::<crate::unix_net::UnixRequest>()).authorized }
            .then_some(DENIED);
    }
    if request.operation == RESOLVE {
        // SAFETY: the resolve adapter pins its request and the Net grant.
        return unsafe { !crate::resolve::authorize(provider, request) }.then_some(DENIED);
    }
    if !matches!(request.operation, CONNECT | LISTEN) {
        return None;
    }
    // SAFETY: CONNECT and LISTEN carry the Net adapter's pinned extension.
    unsafe {
        #[cfg(feature = "record")]
        if crate::tape::owns(provider) {
            if let Some((_, handle)) = crate::tape::source(provider, request.handle) {
                let denial = request_denial(request, handle);
                count_not_public(context, provider, request, handle, denial);
                mark_public(request, handle, denial);
                (*request.path.cast_mut().cast::<NetworkRequest>()).denial = denial;
            } else {
                // Replay: the tape delivers the recorded answer. The counts and the
                // public-only mark come from the same check on the recorded rules.
                let handle = crate::tape::net_rules(provider, request.handle);
                let denial = request_denial(request, handle);
                count_not_public(context, provider, request, handle, denial);
                mark_public(request, handle, denial);
            }
            return None;
        }
        let denial = request_denial(request, request.handle);
        count_not_public(context, provider, request, request.handle, denial);
        mark_public(request, request.handle, denial);
        denial.map(crate::os_net::signal)
    }
}

/// NET-18 part 2: mark a permitted CONNECT that only `Public` rules cover, so
/// its provider refuses an address the host holds.
unsafe fn mark_public(request: &Request, handle: u64, denial: Option<u8>) {
    if request.operation != CONNECT || denial.is_some() {
        return;
    }
    // SAFETY: network adapters pin the scalar address and the grant's Rules.
    unsafe {
        let network = request.path.cast_mut().cast::<NetworkRequest>();
        let address = (*network).address;
        let rules = Rules::from_handle(handle).as_slice();
        (*network).public = hirsel_core_net::public_only(
            rules.iter().filter_map(GrantRule::as_address),
            address.family,
            address.hi,
            address.lo,
            address.port,
        );
    }
}

/// NET-18 part 2, counted from the delivery so a replay counts it too: a
/// served Net's `Denied` after issue to a public-only connect, or the host
/// provider's `Denied` marked `OWN_ADDRESS`.
pub(super) fn count_own_address(
    files: &crate::host_file::Files,
    provider: &Provider,
    network: &NetworkRequest,
    outcome: &Outcome,
    served: bool,
    handle: u64,
) {
    if network.public
        && outcome.error.tag == 1
        && (served || outcome.error.code == hirsel_core_net::OWN_ADDRESS)
    {
        if crate::provide::port_denials(provider, handle, Some(1)).is_some() {
            return;
        }
        let run = files.registry.run_grants();
        *run.own_address
            .lock()
            .unwrap()
            .entry(provider.state as usize)
            .or_default() += 1;
    }
}

/// NET-18: a connect refused because its address is not public, though a rule
/// covers its address and port, counts once against the provider's `not_public`.
unsafe fn count_not_public(
    context: *mut Context,
    provider: &Provider,
    request: &Request,
    handle: u64,
    denial: Option<u8>,
) {
    if request.operation != CONNECT || denial != Some(1) {
        return;
    }
    // SAFETY: network adapters pin the scalar address and the grant's Rules.
    unsafe {
        let address = &(*request.path.cast::<NetworkRequest>()).address;
        let rules = Rules::from_handle(handle).as_slice();
        if hirsel_core_net::not_public(
            rules.iter().filter_map(GrantRule::as_address),
            AddressDirection::Connect,
            address.family,
            address.hi,
            address.lo,
            address.port,
        ) {
            if crate::provide::port_denials(provider, handle, Some(0)).is_some() {
                return;
            }
            let run = (*(*context).host_files).registry.run_grants();
            *run.not_public
                .lock()
                .unwrap()
                .entry(provider.state as usize)
                .or_default() += 1;
        }
    }
}

/// # Safety
/// The class of an address under NET-18's table, in `value`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_class(
    _context: *mut Context,
    family: u8,
    hi: u64,
    lo: u64,
    _site: u64,
) -> Outcome {
    Outcome {
        value: hirsel_core_net::class::class_of(family, hi, lo) as u64,
        ..Outcome::default()
    }
}

/// # Safety
/// `n` is a live Net designation of this run. Its provider's `not_public`
/// count, in `value`, and its `own_address` count, in `written`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_denials(
    context: *mut Context,
    n: u64,
    site: u64,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let provider = &*crate::entry::hirsel_core_capability_provider(context, n);
        let run = (*(*context).host_files).registry.run_grants();
        let handle = crate::entry::hirsel_core_capability_handle(context, n);
        if crate::provide::retired(provider) {
            return crate::provide::withdrawn(context, site);
        }
        if let Some(counts) = crate::provide::port_denials(provider, handle, None) {
            let Ok([value, written]) = counts else {
                return crate::provide::withdrawn(context, site);
            };
            return Outcome {
                value,
                written,
                ..Outcome::default()
            };
        }
        let key = provider.state as usize;
        let count = |counts: &Mutex<BTreeMap<usize, u64>>| {
            counts.lock().unwrap().get(&key).copied().unwrap_or(0)
        };
        Outcome {
            value: count(&run.not_public),
            written: count(&run.own_address),
            ..Outcome::default()
        }
    }
}

unsafe fn request_denial(request: &Request, handle: u64) -> Option<u8> {
    // SAFETY: network adapters pin the scalar address and the grant's Rules.
    unsafe {
        let address = &(*request.path.cast::<NetworkRequest>()).address;
        let rules = Rules::from_handle(handle).as_slice();
        hirsel_core_net::address_denial(
            rules.iter().filter_map(GrantRule::as_address),
            if request.operation == LISTEN {
                AddressDirection::Listen
            } else {
                AddressDirection::Connect
            },
            address.family,
            address.hi,
            address.lo,
            address.port,
        )
    }
}

/// SAFETY: handle is the live Net grant's immutable rule slice.
pub(crate) unsafe fn permits_address(
    handle: u64,
    address: &Address,
    dir: AddressDirection,
) -> bool {
    unsafe {
        let rules = Rules::from_handle(handle);
        rules.permits(|r| {
            r.as_address().is_some_and(|rule| {
                rule.permits(dir, address.family, address.hi, address.lo, address.port)
            })
        })
    }
}

/// # Safety
/// A live TCP socket or listener from this run, borrowed through delivery.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_local_addr(
    context: *mut Context,
    key: u64,
    site: u64,
) -> AddressOutcome {
    // SAFETY: the caller pins the carrier for this operation.
    unsafe { query_address(context, key, LOCAL_ADDR, site) }
}

/// # Safety
/// A live socket from this run, borrowed through delivery.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_peer_addr(
    context: *mut Context,
    key: u64,
    site: u64,
) -> AddressOutcome {
    // SAFETY: the caller pins the carrier for this operation.
    unsafe { query_address(context, key, PEER_ADDR, site) }
}

unsafe fn query_address(context: *mut Context, key: u64, op: u8, site: u64) -> AddressOutcome {
    // SAFETY: the borrowed carrier belongs to this run. The operation has no
    // buffer or deadline.
    unsafe {
        owned_op(
            context,
            op,
            key,
            ptr::null_mut(),
            ptr::null(),
            0,
            0,
            0,
            0,
            site,
        )
    }
}

#[cfg(feature = "record")]
pub(crate) fn address_bytes(address: Address) -> [u8; 19] {
    let mut bytes = [0; 19];
    bytes[0] = address.family;
    bytes[1..9].copy_from_slice(&address.hi.to_le_bytes());
    bytes[9..17].copy_from_slice(&address.lo.to_le_bytes());
    bytes[17..].copy_from_slice(&address.port.to_le_bytes());
    bytes
}

#[cfg(feature = "record")]
pub(crate) unsafe fn denial(request: &Request) -> Option<u8> {
    if matches!(request.operation, CONNECT | LISTEN) {
        // SAFETY: the network adapter supplies this operand for address operations.
        unsafe { (*request.path.cast::<NetworkRequest>()).denial }
    } else {
        None
    }
}

/// # Safety
/// Live list narrow `builder` from `hirsel_core_net_narrow_begin`. Rule scalars
/// come from the caller's Rule.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_net_narrow(
    _context: *mut Context,
    builder: u64,
    dir: u8,
    family: u8,
    hi: u64,
    lo: u64,
    bits: u8,
    reach: u8,
    first: u16,
    last: u16,
    _site: u64,
) -> Outcome {
    let rule = Rule::from_core(RuleRow {
        dir,
        family,
        hi,
        lo,
        bits,
        reach,
        first,
        last,
    });
    unsafe { narrow_add(builder, rule.map(GrantRule::Address)) }
}

/// NET-6: each parent rule intersected with every rule of the list.
pub(crate) unsafe fn narrow(
    context: *mut Context,
    n: u64,
    list: &[GrantRule],
    site: u64,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let provider = *hirsel_core_capability_provider(context, n);
        let parent = hirsel_core_capability_handle(context, n);
        if !crate::provide::grant_live(&provider, parent) {
            return crate::provide::withdrawn(context, site);
        }
        #[cfg(feature = "record")]
        let taped = crate::tape::owns(&provider);
        #[cfg(feature = "record")]
        let rules_handle = if taped {
            crate::tape::net_rules(&provider, parent)
        } else {
            parent
        };
        #[cfg(not(feature = "record"))]
        let rules_handle = parent;
        let rules = Rules::from_handle(rules_handle).as_slice();
        let intersections: Vec<_> = rules
            .iter()
            .flat_map(|r| list.iter().filter_map(move |rule| r.intersect(rule)))
            .collect();
        let charge = intersections
            .iter()
            .try_fold(0u64, |total, r| total.checked_add(r.logical_size()))
            .and_then(|n| n.checked_add(7))
            .map(|n| n & !7);
        let payer = (*context).payer();
        if let Err(capacity) = payer.admit_run_owned(charge) {
            return Outcome {
                value: capacity.requested,
                error: IoError { tag: 2, code: 0 },
                written: capacity.available,
                status: 0,
                kind: 0,
            };
        }
        let rules = RulesStorage::new(intersections);
        let handle = rules.handle();
        let files = (*context).host_files;
        (*files)
            .registry
            .run_grants()
            .net_grants
            .lock()
            .unwrap()
            .push(Grant {
                _rules: rules,
                payer,
                charge: charge.unwrap(),
            });
        if !crate::provide::grant_child(&provider, parent, handle) {
            return crate::provide::withdrawn(context, site);
        }
        #[cfg(feature = "record")]
        let value = if taped {
            let (handle, _group) = crate::tape::narrow_net(&provider, parent, handle);
            (*files).registry.grant(provider, handle)
        } else {
            (*files).registry.grant(provider, handle)
        };
        #[cfg(not(feature = "record"))]
        let value = (*files).registry.grant(provider, handle);
        Outcome {
            value,
            ..Outcome::default()
        }
    }
}
