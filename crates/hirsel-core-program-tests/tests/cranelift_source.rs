//! Source agreement through reached definition units and a C caller.
#![allow(dead_code)]
const NATIVE_TARGET: &str = "x86_64-unknown-linux-gnu";
hirsel_guard::install!();
include!("support/cranelift_source.rs");
#[test]
fn every_front_end_run_case_matches_evaluator() {
    check_front_end_run_cases();
}

#[test]
fn failure_sites_in_dev_and_speed() {
    check_failure_sites();
}

#[test]
fn float_nan_paths_in_dev_and_speed() {
    check_float_nan_paths();
}

#[test]
fn growth_shadow_copies_back_data_and_capacity() {
    check_growth_shadow_words();
}

#[test]
fn blocked_channel_is_a_named_run_stop() {
    let source = r#"
fn waiting() -> u64 waits {
    let (tx, rx): (Sender[u64], Receiver[u64]) = result.unwrap(channel.new(1))
    result.unwrap(channel.receive(rx))
}
"#;
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    assert_eq!(
        eval::run(&module, names["waiting"], &[], 1_000_000).outcome,
        Outcome::Stopped("Deadlock")
    );
    execute(&module, &[(names["waiting"], vec![])], false);
}

use hirsel_core_program_tests::wire_socket;
#[test]
fn wire_socket_loopback_carries_addresses_deadlines_and_bytes() {
    let (program, names) =
        hirsel_core_program_tests::SourceProgram::lower(wire_socket::SOURCE, None);
    let scratch = Scratch::new();
    fs::write(
        scratch.0.join("input.o"),
        program.cranelift_object(NATIVE_TARGET, false, false, false),
    )
    .unwrap();
    let symbol = program
        .identity(program.functions[names["wire"].index()].instance)
        .symbol();
    let source = format!(
        r#"
#include <stdint.h>
#include <assert.h>
#include "hirsel_core_rt.h"
{binding}
{sites}
typedef struct {{ uint64_t value; uint8_t status; }} WireResult;
extern WireResult {symbol}(HirselCoreContext *, uint64_t, uint64_t, uint64_t);
static uint8_t entry(HirselCoreContext *ctx, const HirselCoreProcessConfig *cfg, HirselCoreMainResult *out) {{
    (void)cfg; (void)out;
    WireResult r={symbol}(ctx,1,2,0);
    assert(r.status==0 && r.value==42);
    return 0;
}}
int main(void) {{ assert(hirsel_core_run(wire_config(),entry)==0); return 0; }}
"#,
        binding = wire_socket::BINDINGS,
        sites = program.site_bindings()
    );
    let executable = link_program(&scratch, &source);
    let output = Command::new(executable).output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
