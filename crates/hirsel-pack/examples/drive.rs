//! Load a `.hirsel-mod` whose root is `main() -> Result[(), Exit]` through the pack
//! host join and run it as the root of a contained execution, `rounds` times.
//! Prints each round's outcome: the entry's status, or the run stop and fault report.
//!
//! usage: drive <module> [rounds] [root limit bytes]

use hirsel_code::Signature;
use hirsel_core_ids::ContentHash;
use hirsel_core_rt::{
    Context,
    process::{Config, MainResult},
    run_stop::{EndReason, Execution, run_contained_worker},
    tasks::SchedulerProvider,
};
use hirsel_mod::Mask;

hirsel_guard::install!();

/// `main(context, result_place) -> failed` (PACK-15 typed export ABI).
#[derive(Clone, Copy)]
#[repr(transparent)]
struct Main(unsafe extern "C" fn(*mut Context, *mut u8) -> u8);
// SAFETY: the driver's module is checked to have this signature digest, which is the
// compiler's PACK-14 `hirsel.pack-export.v1` for `main() -> Result[(), Exit]`.
unsafe impl Signature for Main {
    const DIGEST: ContentHash = ContentHash([
        90, 130, 168, 22, 134, 2, 110, 130, 136, 95, 75, 141, 3, 203, 123, 214, 37, 80, 251, 187,
        135, 210, 110, 87, 147, 55, 222, 172, 60, 88, 109, 53,
    ]);
}

unsafe extern "C" fn silent(_: *mut u8, _: *const u8, _: usize) {}

unsafe extern "C" fn root(context: *mut Context, config: *const Config, _: *mut MainResult) -> u8 {
    #[repr(align(16))]
    struct Room([u8; 64]);
    let mut room = Room([0; 64]);
    // SAFETY: `state` is the `Main` the driver put there, live for the run.
    unsafe {
        let main = *(*config).state.cast::<Main>();
        (main.0)(context, room.0.as_mut_ptr())
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("a module path");
    let rounds: usize = args.next().map_or(1, |n| n.parse().unwrap());
    let root_limit: u64 = args.next().map_or(256 << 10, |n| n.parse().unwrap());
    let bytes = std::fs::read(path).unwrap();
    let module = hirsel_mod::Module::parse(&bytes).unwrap();
    let identity = module.entries().next().expect("an entry").identity;
    let loader = hirsel_pack::start(hirsel_pack::config(
        module.header.policy,
        Mask::PURE,
        64 << 20,
        32 << 20,
    ))
    .unwrap();
    loader.admit(module.header.key(), hirsel_mod::digest(&bytes));
    for round in 0..rounds {
        let code = match loader.load::<Main>(&bytes, &identity, true) {
            Ok(code) => code,
            Err(refusal) => {
                println!("round {round}: refused {refusal:?}");
                continue;
            }
        };
        let mut main = code.entry();
        let config = Config {
            launch: std::ptr::null(),
            describe_output: None,
            root_limit,
            args: std::ptr::null(),
            args_len: 0,
            grants: std::ptr::null(),
            grants_len: 0,
            provider: std::ptr::null(),
            state: (&raw mut main).cast(),
            diagnostic: silent,
            describe: None,
            scheduler: SchedulerProvider::default(),
            stop_fd: 2,
            capabilities: std::ptr::null(),
            capabilities_len: 0,
            close_grants: None,
        };
        let execution = Execution::contained();
        // SAFETY: `config` and `code` outlive the call.
        match unsafe { run_contained_worker(&execution, &config, root) } {
            Ok(status) => println!("round {round}: returned {status}"),
            Err(EndReason::RunStop(stop)) => {
                let name = String::from_utf8_lossy(stop.as_bytes()).into_owned();
                let report = hirsel_pack::report(&execution);
                let span = report.and_then(|report| report.span(&module));
                println!("round {round}: run stop {name} {report:?} {span:?}");
            }
            Err(EndReason::HostStop) => println!("round {round}: host stop"),
        }
    }
    drop(module);
    loader.reclaim();
    println!(
        "installed {} retired {}",
        loader.installed(),
        loader.retired()
    );
}
