use hirsel_engine::CompileConfig;
use hirsel_engine::requests::OutputReadiness;
use hirsel_engine::requests::RequestEngine;
use hirsel_engine::requests::observe_inputs;
use hirsel_engine::store::MemStore;
use hirsel_engine_runtime::QueryRuntime;
use hirsel_intern::jobs::with_compiler_stack;
use hirsel_query_products::LoweringPolicy;

use hirsel_front_phases::COMPILER;
use hirsel_requests::CompileOperation;
use hirsel_requests::CompileRequest;
use hirsel_requests::CompileResult;
use hirsel_requests::CompilerRequests;
use hirsel_requests::RequestLimits;
use std::sync::Arc;

#[test]
fn export_stages_reclaim_idle_graphs_before_refusing_process_pressure() {
    pressure(true);
}
#[test]
fn export_can_retry_the_same_snapshot_after_temporary_process_pressure() {
    pressure(false);
}
fn pressure(reclaim_idle: bool) {
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("hirsel.json");
        std::fs::write(
            directory.path().join("main.hir"),
            "pub fn main()->u8{7u8}\n",
        )
        .unwrap();
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"export_pressure","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(768 << 20);
        let config = CompileConfig {
            store: Arc::new(MemStore::default()),
            jobs: hirsel_intern::jobs::Jobs::new(1),
            hold_bytes: 0,
            ..Default::default()
        };
        let mut idle = reclaim_idle.then(|| {
            CompilerRequests::new(
                COMPILER,
                config.clone(),
                budget.clone(),
                RequestLimits {
                    runtime_bytes: 160 << 20,
                    snapshot_bytes: 32 << 20,
                    transport_bytes: 8 << 20,
                },
            )
        });
        if let Some(idle) = &mut idle {
            drop(
                idle.execute(
                    &manifest,
                    CompileOperation::Check {
                        lowering: LoweringPolicy::Reachable,
                    },
                )
                .unwrap(),
            );
        }
        let mut selector = CompilerRequests::new(
            COMPILER,
            config,
            budget.clone(),
            RequestLimits {
                runtime_bytes: 192 << 20,
                snapshot_bytes: 64 << 20,
                transport_bytes: 16 << 20,
            },
        );
        let compiler = selector.compiler().clone();
        let (loaded, inputs) = observe_inputs(
            selector.input_limits(),
            selector.capture_attempts(),
            |inputs| compiler.load_check_package_inputs(&manifest, inputs, true),
        )
        .unwrap();
        drop(loaded.unwrap());
        let request = CompileRequest {
            path: manifest,
            inputs: inputs.into(),
            operation: CompileOperation::ExportTir,
        };
        let mut selected = selector.select(&request).unwrap();
        let pressure = budget.reserve(budget.limit() - budget.reserved()).unwrap();
        let completed = {
            let mut runtime = selected.runtime.lock().unwrap();
            let pending = runtime.request(&selected.roots);
            runtime.wait(pending).unwrap()
        };
        drop(pressure);
        let first = selected.output.ready(&completed);
        drop(selected);
        if reclaim_idle {
            assert!(
                matches!(&first.unwrap().result, CompileResult::ExportTir(text) if !text.is_empty())
            );
        } else {
            let error = first
                .err()
                .expect("export must refuse while no bytes can be reclaimed");
            assert!(
                error.to_string().contains("shared process budget"),
                "{error}"
            );
            let mut selected = selector.select(&request).unwrap();
            let completed = {
                let mut runtime = selected.runtime.lock().unwrap();
                let pending = runtime.request(&selected.roots);
                runtime.wait(pending).unwrap()
            };
            let output = selected.output.ready(&completed).unwrap();
            assert!(matches!(&output.result, CompileResult::ExportTir(text) if !text.is_empty()));
        }
    });
}
/// Law: a retained CheckedProducts result owns what a retained Check result
/// of the same program owns (its sources) plus its own receipt, never its
/// graph's producers or admission. Sequential requests whose results stay
/// held fit a budget with room for one graph, and once they drop the process
/// allowance is back where it started.
#[test]
fn retained_checked_products_results_release_their_graph_admission() {
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("hirsel.json");
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"products_release","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
        let write = |value: u64| {
            std::fs::write(
                directory.path().join("main.hir"),
                format!("pub fn main()->u64{{{value}}}\n"),
            )
            .unwrap()
        };
        let limits = RequestLimits {
            runtime_bytes: 160 << 20,
            snapshot_bytes: 32 << 20,
            transport_bytes: 8 << 20,
        };
        let config = CompileConfig {
            store: Arc::new(MemStore::default()),
            jobs: hirsel_intern::jobs::Jobs::new(1),
            hold_bytes: 0,
            ..Default::default()
        };
        // What one retained Check result of this program keeps charged.
        let check_held = {
            let budget = hirsel_engine_admission::ProcessBudget::new(1 << 30);
            let mut selector =
                CompilerRequests::new(COMPILER, config.clone(), budget.clone(), limits);
            write(0);
            let output = selector
                .execute(
                    &manifest,
                    CompileOperation::Check {
                        lowering: LoweringPolicy::Comptime,
                    },
                )
                .unwrap();
            let CompileResult::Check(checked) = &output.result else {
                panic!("check returned another operation")
            };
            let checked = checked.clone();
            drop(output);
            drop(selector);
            let held = budget.reserved();
            drop(checked);
            assert_eq!(budget.reserved(), 0);
            held
        };
        const RESULTS: u64 = 4;
        // A digest-only receipt of a one-line program is far below 1 MiB.
        const RECEIPT_ROOM: u64 = 1 << 20;
        let graph = limits.runtime_bytes + limits.snapshot_bytes + limits.transport_bytes;
        // The selector's carry takes half the transport allowance up front.
        let budget = hirsel_engine_admission::ProcessBudget::new(
            limits.transport_bytes / 2 + graph + RESULTS * (check_held + RECEIPT_ROOM),
        );
        let mut selector = CompilerRequests::new(COMPILER, config, budget.clone(), limits);
        let mut held = vec![];
        for value in 0..RESULTS {
            write(value);
            let output = selector
                .execute(
                    &manifest,
                    CompileOperation::CheckedProducts {
                        lowering: LoweringPolicy::Comptime,
                        canonical: false,
                    },
                )
                .unwrap_or_else(|error| panic!("request {value}: {error}"));
            let CompileResult::CheckedProducts { checked, products } = &output.result else {
                panic!("request {value} returned another operation")
            };
            held.push((checked.clone(), products.clone()));
        }
        drop(held);
        drop(selector);
        assert_eq!(budget.reserved(), 0, "dropped results kept a charge");
    });
}
