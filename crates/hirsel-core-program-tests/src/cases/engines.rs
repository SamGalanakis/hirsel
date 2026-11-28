//! The flat engine against its oracle (COMPTIME-DIRECTION 3.2, E3): every run line
//! also runs on the pure core twice, on the flat engine and on the tree walker. The
//! two must end alike, with the same bytes charged, peak, steps, executed
//! instructions, failure chain (kinds, messages and sites) and destination bytes,
//! or stop with the same message. Both runs hash maps with one fixed key, since
//! a fresh key moves probe lengths and so the instruction count. How deep the host stack goes differs between the
//! engines, so a `Stack` failure (FAIL-17) or stop compares its outcome only.
use hirsel_core_eval as eval;
use hirsel_core_ir::{FuncId, Module};

type Facts = (u64, u64, u64, u64, Vec<String>, Option<Vec<u8>>);

fn run(
    module: &eval::PureModule<'_>,
    f: FuncId,
    args: &[u64],
    engine: eval::Engine,
) -> Result<(String, Option<Facts>), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let input = eval::PureInput {
            engine,
            map_hash_key: Some([0x5a; 16]),
            ..eval::PureInput::default()
        };
        let r = eval::run_pure(module, f, args, input);
        let stack = r.outcome == eval::Outcome::Stopped("Stack")
            || r.chain.iter().any(|cause| cause.starts_with("Stack"));
        let facts = (!stack).then_some((
            r.used,
            r.peak_reserved,
            r.steps,
            r.instructions,
            r.chain,
            r.destination,
        ));
        (format!("{:?}", r.outcome), facts)
    }))
    .map_err(|stop| {
        let why = stop.downcast_ref::<String>().map(String::as_str);
        why.or_else(|| stop.downcast_ref::<&str>().copied())
            .unwrap_or("no message")
            .to_owned()
    })
}

pub(super) fn agree(module: &Module, f: FuncId, args: &[u64], call: &str) -> Option<String> {
    let module = eval::PureModule::new(module);
    let tree = run(&module, f, args, eval::Engine::Tree);
    let flat = run(&module, f, args, eval::Engine::Flat);
    (flat != tree)
        .then(|| format!("run {call}: the flat engine gave {flat:?}, the tree walker {tree:?}"))
}

#[test]
fn zst_list_and_literal_capacity() {
    let fixture = super::root().join("cases/body/list-zst-literal-capacity.hirsel");
    let failures = super::run_case(&fixture);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
