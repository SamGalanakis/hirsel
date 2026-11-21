//! Source agreement through reached definition units and a C caller.
#![allow(dead_code)]
const NATIVE_TARGET: &str = "x86_64-unknown-linux-gnu";
hirsel_guard::install!();
include!("support/cranelift_source.rs");
include!("support/flow.rs");
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
fn native_flow_pipeline_returns_42_and_refunds_the_owner() {
    let (program, names) = hirsel_core_program_tests::SourceProgram::lower(FLOW_SOURCE, None);
    let scratch = NativeArtifacts::new();
    fs::write(
        scratch.path().join("input.o"),
        program.cranelift_object(NATIVE_TARGET, false, false, false),
    )
    .unwrap();
    let mut declarations = String::new();
    let mut cases = String::new();
    for name in FLOW_CASES {
        let symbol = program
            .identity(program.functions[names[name].index()].instance)
            .symbol();
        declarations.push_str(&format!(
            "extern FlowResult {symbol}(HirselCoreContext *, uint64_t);\n"
        ));
        cases.push_str(&format!("    check(ctx, \"{name}\", {symbol});\n"));
    }
    let foreign = program
        .identity(program.functions[names["foreign_push"].index()].instance)
        .symbol();
    let caller = format!(
        r#"
#include <stdint.h>
#include <stdio.h>
#include <assert.h>
#include "hirsel_core_rt.h"
{sites}
extern uint64_t hirsel_core_test_available(const HirselCoreContext *);
extern void hirsel_core_test_flow_run(size_t, bool, uint8_t, uint64_t, uint8_t *, void (*)(HirselCoreContext *, uint8_t *));
typedef struct {{ uint64_t value; uint8_t status; }} FlowResult;
typedef FlowResult (*Case)(HirselCoreContext *, uint64_t);
{declarations}
extern FlowResult {foreign}(HirselCoreContext *, uint64_t);
static void check(HirselCoreContext *ctx, const char *name, Case run) {{
    uint64_t start=hirsel_core_test_available(ctx);
    FlowResult r=run(ctx,0);
    if (r.status || r.value!=42) fprintf(stderr,"%s: status %d, value %llu\n",name,r.status,(unsigned long long)r.value);
    assert(r.status==0 && r.value==42);
    assert(hirsel_core_test_available(ctx)==start);
}}
static void cases(HirselCoreContext *ctx, uint8_t *state) {{
{cases}
    FlowResult rejected = {foreign}(ctx,0);
    assert(rejected.status==1);
    HirselCoreCauseCell *cause = hirsel_core_causes_take(ctx);
    assert(cause && hirsel_core_cause_kind(hirsel_core_cause_value(cause))==1);
    assert(!hirsel_core_cause_next(cause));
    hirsel_core_causes_drop(cause);
}}
int main(void) {{
    hirsel_core_test_flow_run(1,false,0,4000,0,cases);
    hirsel_core_test_flow_run(1,true,0,4000,0,cases);
    for (uint8_t pause=0; pause<4; ++pause) {{
        hirsel_core_test_flow_run(16,false,pause,4000,0,cases);
    }}
    return 0;
}}
"#,
        sites = program.site_bindings()
    );
    let executable = link_program(&scratch, &caller);
    let output = Command::new(executable).output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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
    let scratch = NativeArtifacts::new();
    fs::write(
        scratch.path().join("input.o"),
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

#[test]
fn static_bytes_preserve_budget_law() {
    let source = include_str!("../../../core/front/cases/rules/MEM-28-static-bytes.hirsel");
    let (program, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    execute(
        &program,
        &[
            (names["literal_refusal"], vec![0]),
            (names["literal_refusal"], vec![1]),
            (names["refund_after_handover"], vec![0]),
            (names["refund_after_handover"], vec![1]),
            (names["empty_is_free"], vec![]),
            (names["cloned_and_counted"], vec![]),
            (names["inline_capacity"], vec![]),
        ],
        false,
    );
}
