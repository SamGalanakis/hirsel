//! Process entry, grants and host provider ABI. FORM decision 44.

/// Process/entry ABI (CAP-4, PROG-3), FORM decision 44 (e68de117).
/// Every capability and Args is a plain u64 provider handle. Main receives one
/// u64 per parameter directly from Config.grants[i], in declaration order.
/// The program never cleans these values. The run closes its grants AFTER
/// main's cleanup and diagnostics, without allocation, admission or failure.
///
/// os_process_new copies argv, pins explicit directory and stream grants, and
/// fills Config.grants using OsConfig.parameters (kind + ordinal). A missing
/// requirement refuses configuration before any task starts. Kinds: 0 Dir,
/// 1 Net, 2 Clock, 3 Random, 4 Stdout, 5 Stderr, 6 Stdin, 7 Env, 8 Process,
/// 9 Tasks, 10 Args. No capability is implicit; Args is present even when empty.
///
/// entry_grant(cfg,kind,index) returns a u64 handle, zero for a missing grant.
/// capability_provider(ctx,handle)/capability_handle(ctx,handle) expose the
/// selected provider and its local designation to core provider adapters.
/// args_len/get take (context, Args handle [, index]); views remain run-owned.
/// Stdin shares one input stream. Env reads a run-owned snapshot. Process supplies
/// the first stop. Net has explicit rule lists and
/// try-first socket operations over a per-run epoll driver. Tasks authorizes the
/// source spawn; FORM #121 carries that designation through its provider. No
/// handle contains an address or reveals a descriptor to source code.
///
/// Config.close_grants is the run-owned teardown callback; os_process_free
/// follows run and all borrowed views, freeing host argv/metadata. MainResult
/// starts Ok; the generated entry writes Error/Exit only after its cleanup.
/// C declarations: include/hirsel_core_rt_host.h.
pub use crate::entry::{
    hirsel_core_capability_handle, hirsel_core_capability_provider, hirsel_core_entry_grant,
    hirsel_core_entry_grants_present, hirsel_core_os_process_config, hirsel_core_os_process_free,
    hirsel_core_os_process_new, hirsel_core_os_run,
};

/// FORM #50: closed provider rows resolve the capability or resource provider.
/// Read, read_at, next_entries and mkdtemp can change carrier length.
/// Outcome is value@0, error-tag@8, code@12, written@16, status@24 and kind@25.
/// Stat uses value for size and written for signed modification nanoseconds.
pub use crate::host_ops::{
    hirsel_core_host_at, hirsel_core_host_op, hirsel_core_host_rename,
    hirsel_core_host_sleep_until, hirsel_core_host_sleep_until_wall, hirsel_core_host_stat,
};
pub use crate::process::{
    hirsel_core_args_get, hirsel_core_args_len, hirsel_core_main_error, hirsel_core_run,
};
pub use crate::provider::hirsel_core_host;

/// HostFile is an opaque u64 provider handle, with no cell, grant or charge
/// (FORM #47, 778f4f0a). No field offsets are read inline. Open publishes
/// provider identity + native designation in run-owned bookkeeping. File calls
/// retain that provider; explicit CLOSE and mechanical drop consume the key.
/// Drop bypasses cancellation, admits nothing and ignores the close outcome.
/// Remaining files close before grants on return; a run stop skips cleanup and
/// _exit closes OS descriptors. A carrier never escapes its run.
pub use crate::host_file::{
    hirsel_core_host_file, hirsel_core_host_file_open, hirsel_core_hostfile_drop,
};

/// # Safety
/// The live cause owns/pins its message; a nonnull Fail message uses the FORM
/// StringMessage carrier. String bytes are already UTF-8 by source typing.
pub(crate) unsafe fn format_cause(cause: &crate::causes::Cause) -> String {
    // SAFETY: same pinned Cause contract; numeric fallback needs no source.
    unsafe { format_cause_at(cause, format_args!("{}", cause.site)) }
}

/// # Safety
/// Same pinned Cause/message contract as format_cause.
#[cold]
#[cfg_attr(hirsel_runtime_size, optimize(size))]
pub(crate) unsafe fn format_cause_at(
    cause: &crate::causes::Cause,
    site: std::fmt::Arguments<'_>,
) -> String {
    let name = [
        "Overflow",
        "Bounds",
        "Division",
        "Unwrap",
        "Fail",
        "Cancelled",
        "Capacity",
        "OutOfTime",
    ][cause.kind() as usize];
    // SAFETY: cause chain pins its canonical immutable String carrier and bytes.
    unsafe {
        if let Some(message) = cause.message() {
            let data = if message.len == 0 {
                &[]
            } else {
                std::slice::from_raw_parts(message.data, message.len as usize)
            };
            format!(
                "{name}({:?}) at {}",
                std::str::from_utf8_unchecked(data),
                site
            )
        } else {
            format!("{name} at {site}")
        }
    }
}

/// PROG-3 not-started boundary: os_run takes (OsConfig*, parameter-name View*, Entry).
/// It probes the Linux openat2 confinement once for the first declared Dir, before
/// main, emits the named one-line refusal to stderr and returns 103 on setup refusal.
/// No Dir parameter means no probe. Raw os_process_new retains the lower-level owner
/// API and reports the same refusal as IoError; generated entries use os_run.
pub use crate::process::NOT_STARTED;
