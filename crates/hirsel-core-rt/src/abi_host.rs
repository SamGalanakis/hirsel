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
/// Net/Random/Stdin/Env/Process have no v1 operations. Tasks authorizes the
/// source spawn; its FORM row omits the designation. No handle contains an
/// address or reveals a descriptor to source code.
///
/// Config.close_grants is the run-owned teardown callback; os_process_free
/// follows run and all borrowed views, freeing host argv/metadata. MainResult
/// starts Ok; the generated entry writes Error/Exit only after its cleanup.
/// C declarations: include/hirsel_core_rt_host.h.
pub use crate::entry::{
    hirsel_core_capability_handle, hirsel_core_capability_provider, hirsel_core_entry_grant,
    hirsel_core_entry_grants_present, hirsel_core_os_process_config, hirsel_core_os_process_free,
    hirsel_core_os_process_new,
};

pub use crate::process::{hirsel_core_args_get, hirsel_core_args_len, hirsel_core_run};
pub use crate::provider::hirsel_core_host;
