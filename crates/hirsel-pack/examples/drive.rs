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

/// The program root for a grant-free `main() -> Result[(), Exit]`.
#[derive(Clone, Copy)]
#[repr(transparent)]
struct Main(unsafe extern "C" fn(*mut Context, *mut ExitResult, u64) -> u8);

#[repr(C)]
struct ExitResult {
    tag: u8,
    status: u8,
}

// SAFETY: native_entry::Plan::signature hashes the fixed result layout, Exit
// offset and empty provider parameter list. Program roots take a final call site.
unsafe impl Signature for Main {
    const DIGEST: ContentHash = ContentHash([
        90, 130, 168, 22, 134, 2, 110, 130, 136, 95, 75, 141, 3, 203, 123, 214, 37, 80, 251, 187,
        135, 210, 110, 87, 147, 55, 222, 172, 60, 88, 109, 53,
    ]);
}

unsafe extern "C" fn silent(_: *mut u8, _: *const u8, _: usize) {}

unsafe extern "C" fn root(
    context: *mut Context,
    config: *const Config,
    out: *mut MainResult,
) -> u8 {
    let mut result = ExitResult { tag: 0, status: 0 };
    // SAFETY: the run pins Main and its image. Only normal return initializes
    // the tag, and only the Error variant initializes its Exit payload.
    unsafe {
        let main = *(*config).state.cast::<Main>();
        let failed = (main.0)(context, &raw mut result, 0);
        if failed == 0 && result.tag == 1 {
            (*out).kind = 2;
            (*out).exit = result.status;
        }
        failed
    }
}

fn run_main(main: Main, execution: &Execution, root_limit: u64) -> Result<u8, EndReason> {
    let mut main = main;
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
    // SAFETY: the worker joins before the checkout releases its entry and configuration.
    unsafe { run_contained_worker(execution, &config, root) }
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
    loader.admit_module(&bytes).unwrap();
    let mut worker = loader.worker();
    for round in 0..rounds {
        let image = match loader.image(&bytes, true) {
            Ok(image) => image,
            Err(refusal) => {
                println!("round {round}: refused {refusal:?}");
                continue;
            }
        };
        let select = || {
            let entry = image.resolve::<Main>(&identity)?;
            let code = image.code::<Main>(&identity)?;
            let registered = code.register([entry.candidate()])?;
            Ok::<_, hirsel_code::Refusal>(registered)
        };
        let registered = match select() {
            Ok(selection) => selection,
            Err(refusal) => {
                println!("round {round}: refused {refusal:?}");
                continue;
            }
        };
        let slot = hirsel_pack::PlaceSlot::new(loader).unwrap();
        let place = slot
            .activate(0, |_| {
                Ok::<_, std::convert::Infallible>((registered, root_limit))
            })
            .unwrap();
        let outcome = place
            .dispatch(&mut worker, 0, |main, execution, &limit| {
                run_main(main, execution, limit)
            })
            .unwrap()
            .unwrap();
        match outcome {
            Ok(status) => println!("round {round}: returned {status}"),
            Err(EndReason::RunStop(stop)) => {
                let name = String::from_utf8_lossy(stop.as_bytes()).into_owned();
                let report = hirsel_pack::report(place.execution());
                let span = report.and_then(|report| report.span(&module));
                println!("round {round}: run stop {name} {report:?} {span:?}");
            }
            Err(EndReason::HostStop) => println!("round {round}: host stop"),
        }
    }
    worker.park();
    hirsel_core_rt::code::worker_park();
    drop(module);
    loader.reclaim();
    println!(
        "installed {} retired {}",
        loader.installed(),
        loader.retired()
    );
}
