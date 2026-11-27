//! `hirsel-exec-helper`: the process that supervises an embedded launch's
//! Exec children (EMB-3 "Exec in an embedded launch"). The embedded runtime
//! starts it with its socket as descriptor 3; it runs the same Supervisor a
//! standalone run does.
//!
//! It installs no `hirsel_guard`: the guard's data limit would be inherited
//! by every child, and a child must start as a standalone run's would
//! (EXEC-3). Nor does it run std's `main`, which would ignore `SIGPIPE`.
#![no_main]

#[unsafe(no_mangle)]
extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    hirsel_core_rt::host::exec_helper_main()
}
