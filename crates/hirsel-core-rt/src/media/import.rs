//! Import a traffic direction, an SRTP master or an SFrame base key from Secret storage.
use super::schedule::{Schedule, admit, owned, refusal, track, track_as};
use crate::{
    Context,
    provider::{IoError, Outcome},
    secret,
};
use hirsel_core_net::{
    ImportKind, ImportRefusal, ImportUse, SframeDirection,
    packet_epoch::{IssueError, SCHEDULE_CHARGE},
    packet_schedule::ImportProfile,
    packet_sframe::REGISTRY_CHARGE,
    srtp_cipher::SrtpProfile,
};

/// # Safety
/// The context is running and the Secret designation moves to this call.
/// A before-issue refusal returns its holder to the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_import_traffic(
    context: *mut Context,
    profile: u64,
    suite: u64,
    key: u64,
    site: u64,
) -> Outcome {
    // SAFETY: forwarded from the checked Secret move.
    unsafe { import(context, ImportKind::Traffic, profile, suite, key, site) }
}

/// # Safety
/// The running context takes custody of a checked Secret designation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_import_srtp(
    context: *mut Context,
    profile: u64,
    key: u64,
    site: u64,
) -> Outcome {
    // SAFETY: forwarded from the checked Secret move.
    unsafe { import(context, ImportKind::Srtp, profile, 0, key, site) }
}

/// # Safety
/// The running context takes custody of a checked Secret designation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_import_sframe(
    context: *mut Context,
    suite: u64,
    key: u64,
    site: u64,
) -> Outcome {
    // SAFETY: forwarded from the checked Secret move.
    unsafe { import(context, ImportKind::Sframe, 0, suite, key, site) }
}

unsafe fn import(
    context: *mut Context,
    kind: ImportKind,
    profile: u64,
    suite: u64,
    key: u64,
    site: u64,
) -> Outcome {
    // SAFETY: the source owns one live holder and passes it exclusively.
    unsafe { crate::process::deny_describe_capability(context) };
    let import = unsafe { secret::resolve(context, key) };
    // A send SFrame import admits its registry entry with the schedule.
    let entry = match import.policy.import() {
        Some(ImportUse::Sframe(SframeDirection::Send { .. })) if kind == ImportKind::Sframe => {
            REGISTRY_CHARGE
        }
        _ => 0,
    };
    let prepared = import.begin_import(kind, || {
        let shape = ImportProfile::from_case(kind, profile, suite).ok_or(IssueError::Invalid)?;
        let requested = SCHEDULE_CHARGE + entry;
        // SAFETY: the context is running during pre-admission.
        let mut charge =
            admit(unsafe { &*context }, requested).map_err(|available| IssueError::Capacity {
                requested,
                available,
            })?;
        let entry = (entry != 0).then(|| charge.partition(entry));
        Ok((shape, charge, entry))
    });
    let (authority, (shape, charge, mut entry), spending) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let mut outcome = match error {
                ImportRefusal::Denied => Outcome {
                    error: IoError { tag: 1, code: 0 },
                    ..Outcome::default()
                },
                ImportRefusal::Spent => refusal(IssueError::Spent),
                ImportRefusal::BeforeIssue(error) => refusal(error),
            };
            outcome.error.code = 1; // Custody returned before issue.
            return outcome;
        }
    };
    // SAFETY: the spent mark committed and this consumes the guest holder.
    unsafe { secret::hirsel_core_secret_drop(context, key) };
    #[cfg(feature = "record")]
    let send_key = entry.is_some();
    let mut state = None;
    let mut derive = || {
        spending.bytes(|bytes| {
            let Some(bytes) = bytes else {
                unreachable!("a replay must supply the opaque delivery")
            };
            let Ok(schedule) = shape.import(authority, Some(bytes)) else {
                return refusal(IssueError::Invalid);
            };
            if let Some(charge) = entry.take() {
                // SAFETY: the context is running; the entry lasts for the run.
                let paid = track_as(unsafe { &*context }, charge, super::REGISTRY_ENTRY);
                let registry = unsafe {
                    &super::run_state((*(*context).host_files).registry.run_grants()).send_keys
                };
                if registry.register(bytes, paid).is_err() {
                    return refusal(IssueError::Spent);
                }
            }
            state = Some(schedule);
            Outcome::default()
        })
    };
    let origin = {
        #[cfg(feature = "record")]
        {
            if import.origin.opaque()
                && unsafe { (*(*context).host_files).secret_mac_tape.is_some() }
            {
                use super::capture::{self, Delivery};
                use crate::provider::Buffer;
                let operation = match kind {
                    ImportKind::Traffic => 2,
                    ImportKind::Srtp => 16,
                    ImportKind::Sframe => 20,
                };
                let mut designation = Vec::new();
                import.origin.put(&mut designation);
                let policy = import.policy.text();
                let args = capture::arguments(
                    operation,
                    0,
                    &[
                        &designation,
                        &profile.to_le_bytes(),
                        &suite.to_le_bytes(),
                        policy.as_bytes(),
                    ],
                );
                let mut empty = Buffer {
                    data: std::ptr::null_mut(),
                    len: 0,
                    capacity: 0,
                };
                // SAFETY: the borrowed context and import bytes live through this issue.
                let answer = unsafe {
                    capture::issue(
                        context,
                        operation,
                        args,
                        Delivery::Child {
                            refusals: if send_key {
                                &[(2, 0), (4, 0)]
                            } else {
                                &[(2, 0)]
                            },
                        },
                        &mut empty,
                        |_| derive(),
                        site,
                    )
                };
                if answer.status != 0 || answer.error != IoError::OK {
                    return answer;
                }
                answer.value
            } else {
                let answer = derive();
                if answer.error != IoError::OK {
                    return answer;
                }
                0
            }
        }
        #[cfg(not(feature = "record"))]
        {
            let _ = site;
            let answer = derive();
            if answer.error != IoError::OK {
                return answer;
            }
            0
        }
    };
    drop(spending);
    if let Some(charge) = entry {
        // A replayed send import: its entry is held as live, never looked up.
        // SAFETY: the context is running; the entry lasts for the run.
        let paid = track_as(unsafe { &*context }, charge, super::REGISTRY_ENTRY);
        unsafe { super::run_state((*(*context).host_files).registry.run_grants()) }
            .send_keys
            .replayed(paid);
    }
    let state = state.unwrap_or_else(|| shape.import(authority, None).unwrap());
    // SAFETY: this call still owns the admitted charge, tied to its running context.
    let charge = track(unsafe { &*context }, charge);
    let profile = match kind {
        ImportKind::Traffic => [profile as u8, suite as u8],
        ImportKind::Srtp => [5, profile as u8],
        ImportKind::Sframe => [7, 0],
    };
    let side = match authority {
        ImportUse::TrafficSend
        | ImportUse::SrtpSend
        | ImportUse::Sframe(SframeDirection::Send { .. }) => 2,
        _ => 3,
    };
    owned(Schedule::new(state, origin, true, charge, profile, side))
}

/// # Safety
/// The running context holds an exclusive loan of a live schedule.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_srtp(
    context: *mut Context,
    parent: u64,
    profile: u64,
    site: u64,
) -> Outcome {
    let Some(selected) = SrtpProfile::from_case(profile) else {
        return refusal(IssueError::Invalid);
    };
    // SAFETY: the checked caller lends its schedule exclusively.
    let parent = unsafe { super::loan_mut::<Schedule>(parent) };
    if let Err(error) = parent.state.check_srtp() {
        return refusal(error);
    }
    let charge = match admit(unsafe { &*context }, SCHEDULE_CHARGE) {
        Ok(charge) => charge,
        Err(available) => {
            return refusal(IssueError::Capacity {
                requested: SCHEDULE_CHARGE,
                available,
            });
        }
    };
    // SAFETY: the running context pins the parent and profile through delivery.
    let origin = match unsafe {
        super::issue_child(context, parent.origin, 1, &[profile.to_le_bytes()], site)
    } {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let state = parent.state.srtp(selected);
    owned(Schedule::new(
        state,
        origin,
        true,
        track(unsafe { &*context }, charge),
        [5, profile as u8],
        #[cfg(feature = "record")]
        parent.side,
        #[cfg(not(feature = "record"))]
        0,
    ))
}
