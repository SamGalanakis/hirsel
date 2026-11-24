//! Import a single TLS 1.3 family traffic direction from Secret storage.
use super::schedule::{Schedule, admit, owned, refusal, track};
use crate::{
    Context,
    provider::{IoError, Outcome},
    secret,
};
use hirsel_core_net::{
    ImportKind, ImportRefusal,
    packet_epoch::{Epoch, IssueError, SCHEDULE_CHARGE, traffic_profile},
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
    // SAFETY: the source owns one live holder and passes it exclusively.
    unsafe { crate::process::deny_describe_capability(context) };
    let import = unsafe { secret::resolve(context, key) };
    let prepared = import.begin_import(ImportKind::Traffic, || {
        let (format, algorithm) = traffic_profile(profile, suite).ok_or(IssueError::Invalid)?;
        // SAFETY: the context is running during pre-admission.
        let charge = admit(unsafe { &*context }, SCHEDULE_CHARGE).map_err(|available| {
            IssueError::Capacity {
                requested: SCHEDULE_CHARGE,
                available,
            }
        })?;
        Ok((format, algorithm, charge))
    });
    let (authority, (format, algorithm, charge)) = match prepared {
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
    let mut epoch = None;
    let mut derive = || {
        let Some(bytes) = import.bytes.as_ref() else {
            unreachable!("a replay must supply the opaque delivery")
        };
        epoch = Epoch::import_traffic(format, algorithm, authority, bytes).ok();
        if epoch.is_some() {
            Outcome::default()
        } else {
            refusal(IssueError::Invalid)
        }
    };
    let origin = {
        #[cfg(feature = "record")]
        {
            if import.origin.opaque()
                && unsafe { (*(*context).host_files).secret_mac_tape.is_some() }
            {
                use super::capture::{self, Delivery};
                use crate::provider::Buffer;
                let (kind, id) = import.origin.wire();
                let policy = import.policy.text();
                let args = capture::arguments(
                    2,
                    0,
                    &[
                        &[kind],
                        &id.to_le_bytes(),
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
                        2,
                        args,
                        Delivery::Child {
                            admits_invalid: true,
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
    let epoch = epoch
        .unwrap_or_else(|| Epoch::opaque_traffic_import(format, algorithm, authority).unwrap());
    // SAFETY: this call still owns the admitted charge, tied to its running context.
    let charge = track(unsafe { &*context }, charge);
    owned(Schedule {
        epoch,
        space: 2,
        origin,
        _charge: charge,
    })
}
