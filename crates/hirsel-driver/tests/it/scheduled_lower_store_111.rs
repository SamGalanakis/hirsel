use hirsel_driver::*;
use hirsel_engine::products::LIVE_LOWER;
use hirsel_engine::products::LiveLowerProduct;
use hirsel_engine::*;
use hirsel_engine_runtime::*;
use hirsel_engine_store as store;
use hirsel_engine_store::MemStore;
use hirsel_engine_store::ResultStore;
use hirsel_intern::jobs::with_compiler_stack;

use hirsel_query_products::QueryKind;
use hirsel_query_products::reads::{ReadKey, Reads, ReadsBuilder};

use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use std::sync::Arc;

struct FreshBytes {
    bytes: MemStore,
    verifier: Arc<hirsel_validate::VerifierCache>,
}
impl store::ResultStore for FreshBytes {
    fn admit_product_memory(&self, bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
        self.bytes.admit_product_memory(bytes)
    }

    fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
        self.bytes.retention_scope()
    }

    fn capability(&self) -> hirsel_engine_store::ProviderCapability {
        self.bytes.capability()
    }

    fn verifier(&self) -> Option<Arc<hirsel_validate::VerifierCache>> {
        Some(self.verifier.clone())
    }
    fn get(&self, key: &store::EntryKey) -> Option<store::Bytes> {
        self.bytes.get(key)
    }
    fn put(&self, key: &store::EntryKey, value: &[u8]) -> hirsel_engine_store::WriteOutcome {
        self.bytes.put(key, value)
    }
    fn name(&self) -> &'static str {
        "mem@1"
    }
}

#[derive(Default)]
struct LiveStore {
    bytes: MemStore,
    missing: std::sync::atomic::AtomicBool,
}
impl ResultStore for LiveStore {
    fn admit_product_memory(&self, bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
        self.bytes.admit_product_memory(bytes)
    }

    fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
        self.bytes.retention_scope()
    }

    fn capability(&self) -> hirsel_engine_store::ProviderCapability {
        hirsel_engine_store::ProviderCapability::RequestLocal(
            hirsel_engine_store::Persistence::Disabled,
        )
    }

    fn get_content(&self, reference: store::Reference) -> Option<store::Bytes> {
        if self.missing.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        self.bytes.get_content(reference)
    }
    fn put_content(&self, domain: store::Domain, bytes: &[u8]) -> Option<store::Reference> {
        self.bytes.put_content(domain, bytes)
    }
    fn get(&self, key: &store::EntryKey) -> Option<store::Bytes> {
        self.bytes.get(key)
    }
    fn put(&self, key: &store::EntryKey, bytes: &[u8]) -> hirsel_engine_store::WriteOutcome {
        self.bytes.put(key, bytes)
    }
    fn verifier(&self) -> Option<Arc<hirsel_validate::VerifierCache>> {
        self.bytes.verifier()
    }
    fn name(&self) -> &'static str {
        "live-without-persistence"
    }
}

#[test]
fn stored_lower_reuses_unchanged_inputs_and_rechecks_edited_constant_dependencies() {
    check_lower_reuse(false);
    check_live_lower_observations(true);
}

#[test]
fn carried_lower_republishes_type_components_into_a_fresh_store() {
    check_lower_reuse(true);
}

fn check_lower_reuse(carry_across_stores: bool) {
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("main.hir");
        let manifest = directory.path().join("hirsel.json");
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"lower_reuse","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
        for verify in [Verify::On, Verify::Off] {
            for jobs in [1, 2] {
                let store: Arc<dyn store::ResultStore> = Arc::new(MemStore::default());
                let carry_budget = hirsel_engine_admission::ProcessBudget::new(32 << 20);
                let carry = LowerCarry::new(carry_budget.reserve(32 << 20).unwrap())
                    .ok()
                    .unwrap();
                let mut original_identity = None;
                for revision in 0..4 {
                    let store: Arc<dyn store::ResultStore> = if carry_across_stores {
                        Arc::new(FreshBytes {
                            bytes: MemStore::default(),
                            verifier: store.verifier().unwrap(),
                        })
                    } else {
                        store.clone()
                    };
                    let value = if revision == 3 { 2 } else { 1 };
                    let extra = if revision >= 2 { "+0u8" } else { "" };
                    std::fs::write(&source, format!("const A:u8=comptime {value}u8;\nfn helper()->u8{{A}}\npub fn main()->u8{{comptime helper(){extra}}}\n")).unwrap();
                    let mut program = COMPILER
                        .with_config(CompileConfig {
                            store: store.clone(),
                            verify,
                            jobs: hirsel_intern::jobs::Jobs::new(jobs),
                            ..Default::default()
                        })
                        .with_owned_engines()
                        .load_program(&manifest)
                        .unwrap();
                    let definitions: Vec<_> = ["A", "helper", "main"]
                        .iter()
                        .map(|name| {
                            let mut path = program.entry.clone();
                            path.0.name = (*name).into();
                            path
                        })
                        .collect();
                    let errors = crate::scheduled_front::errors();
                    let budget = hirsel_engine_admission::ProcessBudget::new(1 << 30);
                    let mut runtime =
                        AdmittedRuntime::new(budget.reserve(512 << 20).unwrap(), errors).unwrap();
                    let weight = MemoryWeight {
                        retained: 2 << 20,
                        scratch: 8 << 20,
                        stack: 0,
                    };
                    let front = program
                        .engine
                        .front_body_queries(&definitions, weight, &budget, errors)
                        .unwrap();
                    let lower = program
                        .engine
                        .front_lower_queries(
                            &definitions,
                            front.clone(),
                            weight,
                            64,
                            &budget,
                            errors,
                        )
                        .unwrap();
                    if carry_across_stores {
                        lower.install_carry(carry.clone()).ok().unwrap();
                    }
                    runtime.register(FRONT_BODY_KIND, front.factory()).unwrap();
                    runtime
                        .register_local(FRONT_LOWER_KIND, lower.factory())
                        .unwrap();
                    runtime
                        .register_local(FRONT_EVALUATE_KIND, lower.factory())
                        .unwrap();
                    let request = runtime.request(&[lower.query(&program.entry).unwrap()]);
                    let products = runtime.wait(request).unwrap_or_else(|error| {
                        panic!(
                            "revision={revision}, jobs={jobs}, verify={verify:?}: {:?}",
                            lower.failure(error)
                        )
                    });
                    let identity = (products[0].address, products[0].action);
                    if revision == 0 {
                        original_identity = Some(identity);
                    } else if revision == 1 {
                        assert_eq!(
                            Some(identity),
                            original_identity,
                            "unchanged Lower must preserve replay identity"
                        );
                    }
                    let actual = lower.outcome((products[0]).clone()).unwrap();
                    let scalar = program.engine.lower_to_tir(&program.entry).unwrap();
                    let records = NativeRecords::new(program.engine.types().clone(), store.clone());
                    assert_eq!(
                        records.encode(&program.engine, &actual.output.value),
                        records.encode(&program.engine, &scalar.value)
                    );
                    assert_eq!(actual.verified.is_some(), verify == Verify::On);
                    let mut live_grant = budget.reserve(16 << 20).unwrap();
                    let live_lower = actual
                        .into_live((products[0]).clone(), &mut live_grant)
                        .unwrap();
                    assert_eq!(live_lower.address(), products[0].address);
                    assert_eq!(live_lower.action(), products[0].action);
                    assert_eq!(live_lower.output().value.path.path(), &program.entry);
                    let front_request = runtime.request(&[front.query(&program.entry).unwrap()]);
                    let front_product = runtime.wait(front_request).unwrap()[0].clone();
                    let live_summary = front.live_summary(front_product, &mut live_grant).unwrap();
                    assert!(live_summary.equivalent(&live_summary.clone()));
                    if revision == 0 {
                        let mut scratch = budget.reserve(8 << 20).unwrap();
                        let archive = live_lower.archive(&mut scratch, 8 << 20).unwrap();
                        assert_eq!(archive.reference().domain, LIVE_LOWER);
                        assert_eq!(
                            store.put_content(LIVE_LOWER, archive.bytes()),
                            Some(archive.reference())
                        );
                        assert_eq!(
                            live_lower
                                .archive(&mut scratch, u64::MAX)
                                .unwrap()
                                .reference(),
                            archive.reference()
                        );
                    }
                    let work = lower.work_snapshot();
                    let lowerings: u64 = work.iter().map(|(_, w)| w.lowerings).sum();
                    let initializers: u64 = work.iter().map(|(_, w)| w.initializers).sum();
                    match revision {
                        0 => assert!(lowerings > 0 && initializers > 0),
                        1 => assert_eq!(
                            (lowerings, initializers),
                            (0, 0),
                            "fresh graph must adopt ordinary Lower products"
                        ),
                        2 => {
                            assert_eq!(lower.work(&definitions[0]).initializers, 0);
                            assert_eq!(lower.work(&definitions[1]).lowerings, 0);
                            assert_eq!(lower.work(&definitions[2]).lowerings, 1);
                        }
                        3 => assert!(initializers > 0, "edited constant must be evaluated again"),
                        _ => unreachable!(),
                    }
                    runtime.shutdown();
                    drop(runtime);
                    drop(products);
                    drop(front);
                    drop(lower);
                    drop(program);
                    drop(live_grant);
                    assert_eq!(
                        live_lower.output().value.path,
                        live_summary.value().definition
                    );
                    assert!(budget.reserved() > 0);
                    drop(live_lower);
                    drop(live_summary);
                    assert_eq!(
                        budget.reserved(),
                        0,
                        "last typed views release the complete producer grants"
                    );
                }
            }
        }
    });
}

#[test]
fn live_lower_retains_complete_observations_under_exact_admission() {
    check_live_lower_observations(false);
}

fn check_live_lower_observations(live_only: bool) {
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("main.hir");
        std::fs::write(&path, "pub fn main()->u8{comptime 7u8}\n").unwrap();
        let input = if live_only {
            let manifest = directory.path().join("hirsel.json");
            std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"live_lower","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
            manifest
        } else {
            path
        };
        for verify in [Verify::On, Verify::Off] {
            let storage = Arc::new(LiveStore::default());
            let store: Arc<dyn ResultStore> = if live_only {
                storage.clone()
            } else {
                Arc::new(MemStore::default().with_installed_products())
            };
            let mut program = COMPILER
                .with_config(CompileConfig {
                    store: store.clone(),
                    verify,
                    ..Default::default()
                })
                .with_owned_engines()
                .load_program(&input)
                .unwrap();
            let budget = hirsel_engine_admission::ProcessBudget::new(1 << 30);
            let errors = crate::scheduled_front::errors();
            let mut runtime =
                AdmittedRuntime::new(budget.reserve(512 << 20).unwrap(), errors).unwrap();
            let weight = MemoryWeight {
                retained: 2 << 20,
                scratch: 8 << 20,
                stack: 0,
            };
            let definitions = [program.entry.clone()];
            let source_bytes = program
                .engine
                .front_input_footprint(&definitions)
                .unwrap()
                .source_headers;
            let source_admission = Arc::new(budget.reserve(source_bytes).unwrap());
            let source_owner = Arc::downgrade(&source_admission);
            program.engine.sources =
                std::mem::take(&mut program.engine.sources).replace_owner(source_admission);
            let front = program
                .engine
                .front_body_queries(&definitions, weight, &budget, errors)
                .unwrap();
            let lower = program
                .engine
                .front_lower_queries(&definitions, front.clone(), weight, 64, &budget, errors)
                .unwrap();
            runtime.register(FRONT_BODY_KIND, front.factory()).unwrap();
            runtime
                .register_local(FRONT_LOWER_KIND, lower.factory())
                .unwrap();
            runtime
                .register_local(FRONT_EVALUATE_KIND, lower.factory())
                .unwrap();
            let request = runtime.request(&[lower.query(&program.entry).unwrap()]);
            let product = runtime
                .wait(request)
                .unwrap_or_else(|error| {
                    panic!(
                        "verify={verify:?}, resource={}, protocol={}, cycle={}, lower={:?}",
                        error == errors.resource,
                        error == errors.protocol,
                        error == errors.cycle,
                        lower.failure(error)
                    );
                })
                .remove(0);
            let actual = lower.outcome(product.clone()).unwrap();
            let published = product
                .live_value::<hirsel_engine::products::LiveLowerProduct>()
                .expect("ordinary Lower publishes its admitted live owner");
            assert!(Arc::ptr_eq(&published.output().value, &actual.output.value));
            let reserved = budget.reserved();
            let context = program
                .engine
                .native_original_context(&definitions)
                .unwrap();
            let reader = context
                .reader([lower.native_product(product.clone()).unwrap()])
                .unwrap();
            assert_eq!(
                budget.reserved(),
                reserved,
                "native consumers share the original admission"
            );
            let raw = reader.raw(&program.entry).unwrap();
            let original = reader.read(&program.entry).unwrap();
            assert!(Arc::ptr_eq(original.definition(), &actual.output.value));
            drop(original);
            assert!(
                !actual.materialized.is_empty(),
                "exercise a real staged value"
            );
            assert_eq!(actual.verified.is_some(), verify == Verify::On);
            let expected = actual.clone();
            let required = LiveLowerProduct::required_bytes(&actual).unwrap();
            let mut short = budget.reserve(required - 1).unwrap();
            assert!(
                actual
                    .clone()
                    .into_live(product.clone(), &mut short)
                    .is_none()
            );
            assert_eq!(short.bytes(), required - 1);
            drop(short);
            let mut exact = budget.reserve(required).unwrap();
            let live = actual.into_live(product, &mut exact).unwrap();
            assert_eq!(exact.bytes(), 0);
            assert_eq!(live.retained_bytes(), required);
            let observations = live.observations();
            assert_eq!(
                observations.implementation_revision,
                expected.implementation_revision
            );
            assert_eq!(observations.signature, expected.signature);
            assert_eq!(observations.verified, expected.verified);
            assert_eq!(
                observations.execution.is_some(),
                expected.execution.is_some()
            );
            assert_eq!(observations.execution_subject, expected.execution_subject);
            assert_eq!(observations.executed, expected.executed);
            assert_eq!(observations.selections, expected.selections);
            assert_eq!(observations.acceptance, expected.acceptance);
            assert_eq!(live.reads(), &expected.reads);
            assert_eq!(live.depth().first, expected.depth.first);
            let records = NativeRecords::new(program.engine.types().clone(), store);
            let mut scratch = budget.reserve(8 << 20).unwrap();
            let archive = live.archive(&mut scratch, 8 << 20).unwrap();
            let (
                output,
                reads,
                depth,
                materialized,
                implementation,
                signature,
                executed,
                selections,
            ): (
                hirsel_types::QueryOutput<hirsel_tir::DefinitionRef>,
                Reads,
                hirsel_query_products::depth::DepthEvidence<hirsel_span::Span>,
                Vec<(hirsel_syntax::ExprId, hirsel_tir::Materialized)>,
                hirsel_syntax::ContentHash,
                hirsel_syntax::ContentHash,
                bool,
                Vec<(hirsel_syntax::DefId, Vec<u64>)>,
            ) = records.decode(&program.engine, archive.bytes()).unwrap();
            assert_eq!(output.value, expected.output.value);
            assert_eq!(output.diagnostics, expected.output.diagnostics);
            assert_eq!(output.dependencies, expected.output.dependencies);
            assert_eq!(reads, expected.reads);
            assert_eq!(depth.first, expected.depth.first);
            assert_eq!(implementation, expected.implementation_revision);
            assert_eq!(signature, expected.signature);
            assert_eq!(executed, expected.executed);
            assert_eq!(selections, expected.selections);
            assert_eq!(
                records.encode(&program.engine, &materialized),
                records.encode(&program.engine, &expected.materialized)
            );
            let entry = program.entry.clone();
            storage
                .missing
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let producer = Arc::downgrade(&lower);
            let front_producer = Arc::downgrade(&front);
            drop(expected);
            drop(program);
            runtime.shutdown();
            drop(runtime);
            drop(front);
            drop(lower);
            assert!(
                producer.upgrade().is_none(),
                "a live value does not retain its producer table"
            );
            assert!(
                front_producer.upgrade().is_none(),
                "Lower retains source backing independently of P"
            );
            drop(scratch);
            drop(exact);
            assert!(
                source_owner.upgrade().is_some(),
                "the source grant survives its graph"
            );
            assert!(
                budget.reserved() > required,
                "live output pins its producer backing"
            );
            assert!(budget.reserved() >= required + source_bytes);
            drop(live);
            assert!(
                source_owner.upgrade().is_some(),
                "native consumer retains the original grant"
            );
            let original = reader
                .read(&entry)
                .expect("live owner survives a missing store and producer drop");
            assert_eq!(original.output().is_some(), verify == Verify::On);
            drop(original);
            drop(reader);
            assert!(source_owner.upgrade().is_some(), "raw view owns its grant");
            drop(raw);
            drop(context);
            assert!(source_owner.upgrade().is_none());
            assert_eq!(budget.reserved(), archive.retained_bytes());
            drop(archive);
            assert_eq!(budget.reserved(), 0);
        }
    });
}

/// What one scheduled Lower request answered for each definition: how many
/// times it lowered the body, and its canonical TIR bytes, V's verdict and
/// any diagnostic.
type Answers = std::collections::BTreeMap<String, (u64, Result<(Vec<u8>, bool), String>)>;

/// Lower every named definition of `source` against `store`.
fn lower_all(
    directory: &std::path::Path,
    source: &str,
    names: &[&str],
    store: Arc<dyn store::ResultStore>,
) -> Answers {
    lower_package(directory, &[("main.hir", source)], "[]", names, store)
}

/// Lower every named root definition of a package whose files are `files`
/// (paths relative to `directory`, `main.hir` its one module) and whose
/// dependencies are the manifest JSON `dependencies`.
fn lower_package(
    directory: &std::path::Path,
    files: &[(&str, &str)],
    dependencies: &str,
    names: &[&str],
    store: Arc<dyn store::ResultStore>,
) -> Answers {
    let manifest = directory.join("hirsel.json");
    // `main.hir` is the root module; any other `<m>.hir` is module `m`.
    let sources: Vec<String> = files
        .iter()
        .map(|(path, _)| match path.strip_suffix(".hir") {
            Some("main") | None => format!(r#"{{"path":"{path}","module":[]}}"#),
            Some(module) => format!(r#"{{"path":"{path}","module":["{module}"]}}"#),
        })
        .collect();
    let sources = sources.join(",");
    std::fs::write(&manifest, format!(r#"{{"schema":1,"package":{{"name":"universe_split","version":"1"}},"sources":[{sources}],"dependencies":{dependencies},"entry":{{"module":[],"name":"main"}}}}"#)).unwrap();
    for (path, text) in files {
        let path = directory.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let mut program = COMPILER
        .with_config(CompileConfig {
            store: store.clone(),
            verify: Verify::On,
            jobs: hirsel_intern::jobs::Jobs::ONE,
            ..Default::default()
        })
        .with_owned_engines()
        .load_program(&manifest)
        .unwrap();
    let definitions: Vec<_> = names
        .iter()
        .map(|name| {
            let mut path = program.entry.clone();
            path.0.name = (*name).into();
            path
        })
        .collect();
    let errors = crate::scheduled_front::errors();
    let budget = hirsel_engine_admission::ProcessBudget::new(1 << 30);
    let mut runtime = AdmittedRuntime::new(budget.reserve(512 << 20).unwrap(), errors).unwrap();
    let weight = MemoryWeight {
        retained: 2 << 20,
        scratch: 8 << 20,
        stack: 0,
    };
    let front = program
        .engine
        .front_body_queries(&definitions, weight, &budget, errors)
        .unwrap();
    let lower = program
        .engine
        .front_lower_queries(&definitions, front.clone(), weight, 64, &budget, errors)
        .unwrap();
    runtime.register(FRONT_BODY_KIND, front.factory()).unwrap();
    runtime
        .register_local(FRONT_LOWER_KIND, lower.factory())
        .unwrap();
    runtime
        .register_local(FRONT_EVALUATE_KIND, lower.factory())
        .unwrap();
    let records = NativeRecords::new(program.engine.types().clone(), store.clone());
    let mut answers = Answers::new();
    for (name, path) in names.iter().zip(&definitions) {
        let request = runtime.request(&[lower.query(path).unwrap()]);
        let answer = match runtime.wait(request) {
            Ok(products) => {
                let actual = lower.outcome(products[0].clone()).unwrap();
                Ok((
                    records
                        .encode(&program.engine, &actual.output.value)
                        .unwrap(),
                    actual.verified.is_some(),
                ))
            }
            // Anchors are process-local; the one source file's offsets place
            // each diagnostic.
            Err(error) => Err(format!("{:?}", lower.failure(error))
                .split("Anchor(")
                .enumerate()
                .map(|(i, part)| {
                    if i == 0 {
                        part
                    } else {
                        part.split_once(')').map_or(part, |(_, rest)| rest)
                    }
                })
                .collect()),
        };
        answers.insert(name.to_string(), (0, answer));
    }
    for (name, path) in names.iter().zip(&definitions) {
        answers.get_mut(*name).unwrap().0 = lower.work(path).lowerings;
    }
    runtime.shutdown();
    answers
}

/// The universe split's cutoff law: after an edit, a warm store reruns only
/// the bodies whose own reads (V's views among them) changed, and every
/// answer equals a fresh lowering and V run of every body with no stored
/// product and no receipt reuse.
#[test]
fn warm_lower_reruns_only_bodies_whose_validation_reads_changed() {
    with_compiler_stack(|| {
        let base = [
            "pub fn a()->u8{1u8}",
            "pub fn b()->u8{a()}",
            "pub fn c()->u8{2u8}",
            "pub fn d()->u8{c()}",
            "fn helper()->u8{b()}",
            "pub fn main()->u8{comptime helper()}",
        ];
        let names = ["a", "b", "c", "d", "helper", "main"];
        let text = |lines: &[&str]| lines.join("\n") + "\n";
        let reversed: Vec<_> = base.iter().rev().copied().collect();
        let with = |replace: &str, by: &str| {
            text(
                &base
                    .iter()
                    .map(|l| if *l == replace { by } else { l })
                    .collect::<Vec<_>>(),
            )
        };
        // (source, the bodies a correct cutoff reruns, hand-derived)
        let edits: Vec<(String, Vec<&str>)> = vec![
            // c's header changes, so c and d, which reads c's signature and
            // V's view of it, rerun; nothing else does.
            (
                with("pub fn c()->u8{2u8}", "fn c()->u8{2u8}"),
                vec!["c", "d"],
            ),
            // c is public again, which d's newest variant did not observe, so
            // d reruns; the new name zz, which no body looked up, reruns
            // nothing.
            (text(&base) + "pub fn zz()->u8{3u8}\n", vec!["d"]),
            // An invalid signature front refuses never enters V's universe,
            // so it reaches no other body: nothing reruns.
            (text(&base) + "pub fn bad()->Nope{3u8}\n", vec![]),
            // An invalid signature d reads fails c and d before either is
            // lowered; nothing else reruns.
            (with("pub fn c()->u8{2u8}", "pub fn c()->Nope{2u8}"), vec![]),
            // Fixing it recovers both from their stored variants: failures
            // are never stored, so d's newest variant observed the valid c.
            (text(&base), vec![]),
            // a's body alone changes: b keeps its view of a, and only main,
            // whose comptime evaluation executed a, reruns beside a.
            (
                with("pub fn a()->u8{1u8}", "pub fn a()->u8{5u8}"),
                vec!["a", "main"],
            ),
        ];
        // The reversed history moves every definition, so only the oracle
        // decides it; the base history also checks the rerun sets.
        for (history, check_reruns) in [(text(&base), true), (text(&reversed), false)] {
            let warm_dir = tempfile::tempdir().unwrap();
            let warm: Arc<dyn store::ResultStore> = Arc::new(MemStore::default());
            let cold = lower_all(warm_dir.path(), &history, &names, warm.clone());
            assert!(
                cold.values()
                    .all(|(n, answer)| *n == 1 && matches!(answer, Ok((_, true))))
            );
            for (source, expected) in &edits {
                let fresh_dir = tempfile::tempdir().unwrap();
                let oracle = lower_all(
                    fresh_dir.path(),
                    source,
                    &names,
                    Arc::new(MemStore::default()),
                );
                let actual = lower_all(warm_dir.path(), source, &names, warm.clone());
                for name in names {
                    assert_eq!(
                        actual[name].1, oracle[name].1,
                        "{name} after edit:\n{source}"
                    );
                }
                let reran: Vec<_> = names.iter().copied().filter(|n| actual[*n].0 > 0).collect();
                if check_reruns {
                    assert_eq!(&reran, expected, "rerun set after edit:\n{source}");
                }
            }
        }
    });
}

/// The checker's own view reads carry the cutoff where front reads nothing:
/// `operations` reaches the opaque `ptr.Pointer` only through
/// `List[Operation]` → `Path`, so front never reads Pointer's signature or
/// representation, yet V checked the body against Pointer's view. When
/// Pointer's representation changes, a correct cutoff reruns `operations`
/// (V's view of it changed) and nothing else, and every answer equals a
/// fresh lowering and V run. Shape of std `json_patch.operations`.
#[test]
fn warm_lower_reruns_a_body_that_reaches_an_opaque_type_only_through_the_checker() {
    with_compiler_stack(|| {
        let main = "pub opaque type Path: copyable + inert + noalloc_drop uses {} {\n  pointer: ptr.Pointer,\n}\npub type Operation = Add(Path) | Clear\npub opaque type Patch: copyable + inert + noalloc_drop uses {} {\n  operations: List[Operation],\n}\npub fn operations(patch: Patch) -> List[Operation] in patch uses {} {\n  patch.operations\n}\npub fn main()->u8{3u8}\n";
        let base = "pub opaque type Pointer: copyable + inert + noalloc_drop uses {} {\n  tokens: List[String],\n}\n";
        let grown = base.replace(
            "tokens: List[String],",
            "tokens: List[String],\n  depth: u8,",
        );
        let names = ["operations", "main"];
        let warm_dir = tempfile::tempdir().unwrap();
        let warm: Arc<dyn store::ResultStore> = Arc::new(MemStore::default());
        let cold = lower_package(
            warm_dir.path(),
            &[("main.hir", main), ("ptr.hir", base)],
            "[]",
            &names,
            warm.clone(),
        );
        assert!(
            cold.values()
                .all(|(n, answer)| *n == 1 && matches!(answer, Ok((_, true))))
        );
        // (ptr module source, the bodies a correct cutoff reruns, hand-derived)
        // Back at the base, operations reruns again: restore resolves the
        // newest variant, which observed the grown view.
        for (ptr, expected) in [
            (grown.as_str(), vec!["operations"]),
            (base, vec!["operations"]),
        ] {
            let files = [("main.hir", main), ("ptr.hir", ptr)];
            let fresh_dir = tempfile::tempdir().unwrap();
            let oracle = lower_package(
                fresh_dir.path(),
                &files,
                "[]",
                &names,
                Arc::new(MemStore::default()),
            );
            let actual = lower_package(warm_dir.path(), &files, "[]", &names, warm.clone());
            for name in names {
                assert_eq!(actual[name].1, oracle[name].1, "{name} with ptr:\n{ptr}");
            }
            let reran: Vec<_> = names.iter().copied().filter(|n| actual[*n].0 > 0).collect();
            assert_eq!(reran, expected, "rerun set with ptr:\n{ptr}");
        }
    });
}

/// What one lowering of the `client` entry `main`, which executes the
/// `library` dependency's `lib.width` at compile time, did: its encoded
/// answer and verdict, and how often `main` and `lib.width` were lowered
/// (zero for a body restored from the store).
#[derive(Debug, PartialEq)]
struct Lowered {
    answer: (Vec<u8>, bool),
    main_lowerings: u64,
    width_lowerings: u64,
}

/// Lowers `client`'s entry `main` = `comptime lib.width()` on a fresh
/// compiler over `store`. `lib.width` names the `library` package's
/// unexposed `internal.Plan`, whose source is `internal`.
fn lower_client(directory: &std::path::Path, internal: &str, store: &Arc<MemStore>) -> Lowered {
    std::fs::write(directory.join("library.json"), r#"{"schema":1,"package":{"name":"library","version":"1"},"sources":[{"path":"api.hir","module":["lib"]},{"path":"internal.hir","module":["internal"]}],"dependencies":[]}"#).unwrap();
    std::fs::write(
        directory.join("api.hir"),
        "pub fn width() -> u32 uses {} {\n  let plans: List[internal.Plan] = []\n  7u32\n}\n",
    )
    .unwrap();
    std::fs::write(directory.join("internal.hir"), internal).unwrap();
    let manifest = directory.join("client.json");
    std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"client","version":"1"},"sources":[{"path":"client.hir","module":[]}],"dependencies":[{"manifest":"library.json","package":{"name":"library","version":"1"},"expose":[{"head":"lib","module":["lib"]}]}],"entry":{"module":[],"name":"main"}}"#).unwrap();
    std::fs::write(
        directory.join("client.hir"),
        "pub fn main()->u32{comptime lib.width()}\n",
    )
    .unwrap();
    let mut program = COMPILER
        .with_config(CompileConfig {
            store: store.clone(),
            verify: Verify::On,
            jobs: hirsel_intern::jobs::Jobs::ONE,
            ..Default::default()
        })
        .with_owned_engines()
        .load_program(&manifest)
        .unwrap();
    let main = program.entry.clone();
    let width = program
        .engine
        .front_source_roots()
        .into_iter()
        .find(|path| path.0.name == "width")
        .unwrap();
    let definitions = [main.clone(), width.clone()];
    let errors = crate::scheduled_front::errors();
    let budget = hirsel_engine_admission::ProcessBudget::new(1 << 30);
    let mut runtime = AdmittedRuntime::new(budget.reserve(512 << 20).unwrap(), errors).unwrap();
    let weight = MemoryWeight {
        retained: 2 << 20,
        scratch: 8 << 20,
        stack: 0,
    };
    let front = program
        .engine
        .front_body_queries(&definitions, weight, &budget, errors)
        .unwrap();
    let lower = program
        .engine
        .front_lower_queries(&definitions, front.clone(), weight, 64, &budget, errors)
        .unwrap();
    runtime.register(FRONT_BODY_KIND, front.factory()).unwrap();
    runtime
        .register_local(FRONT_LOWER_KIND, lower.factory())
        .unwrap();
    runtime
        .register_local(FRONT_EVALUATE_KIND, lower.factory())
        .unwrap();
    let records = NativeRecords::new(program.engine.types().clone(), store.clone());
    let request = runtime.request(&[lower.query(&main).unwrap()]);
    let products = runtime
        .wait(request)
        .unwrap_or_else(|error| panic!("main refused: {:?}", lower.failure(error)));
    let actual = lower.outcome(products[0].clone()).unwrap();
    drop(products);
    let lowered = Lowered {
        answer: (
            records
                .encode(&program.engine, &actual.output.value)
                .unwrap(),
            actual.verified.is_some(),
        ),
        main_lowerings: lower.work(&main).lowerings,
        width_lowerings: lower.work(&width).lowerings,
    };
    runtime.shutdown();
    lowered
}

const PLAN: &str =
    "pub opaque type Plan: copyable + inert + noalloc_drop uses {} {\n  tokens: List[String],\n}\n";

/// A dependency body's narrowed V reads keep their package when a consumer
/// that executes it merges them (argvplan's `plan_json` executing std bodies
/// that view the unexposed `std/internal/substring.Plan`, the manifest gate's
/// refusal): they are observed in the dependency's universe, which admits
/// `internal.Plan`, not in the client's, which does not. So a second lowering
/// on unchanged sources restores the first: nothing is lowered again.
#[test]
fn a_comptime_call_into_a_dependency_reruns_nothing_on_unchanged_sources() {
    with_compiler_stack(|| {
        let scratch = tempfile::tempdir().unwrap();
        let store = Arc::new(MemStore::default());
        let cold = lower_client(scratch.path(), PLAN, &store);
        assert!(cold.answer.1, "main is verified");
        assert_eq!((cold.main_lowerings, cold.width_lowerings), (1, 1));
        let warm = lower_client(scratch.path(), PLAN, &store);
        assert_eq!(
            warm,
            Lowered {
                main_lowerings: 0,
                width_lowerings: 0,
                ..cold
            }
        );
    });
}
