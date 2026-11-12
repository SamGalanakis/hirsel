//! The observation matrix: one program's reference observation
//! against the heap executor and native execution under every legal compiler
//! choice. A configuration names its target, placement, reuse, engine,
//! optimization level and build topology, so a report row is reproducible
//! from its name alone.
use hirsel_engine::ValidatedNativeInput;
use hirsel_lir::emission::OptLevel;
use hirsel_lir::layout_types::TargetSpec;
use hirsel_lir::stage::ObjectArtifact;
use hirsel_native::Fixture;
use hirsel_native::emit::Backend;
use hirsel_native::link::Partition;
use hirsel_native::link::RuntimeBuild;
use hirsel_native::link::Toolchain;
use hirsel_native::link::emit_backend_objects;
use hirsel_native::link::link_fixture_runnable;
use hirsel_native::link::observe;
use hirsel_native::observe::Observation;
use hirsel_native::observe::Sites;
use hirsel_native::stages::NativeError;
use hirsel_native::stages::Options;
use hirsel_native::stages::ReadyProgram;
use hirsel_native::stages::StageName;
use hirsel_native::{HeapCampaignMeasure, HeapMeasure};
use hirsel_native_batch::build;
use hirsel_syntax::DefPath;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Bump when a configuration is added, removed or renamed: stored reports
/// and mismatch artifacts name the matrix they ran.
pub const MATRIX_VERSION: u32 = 4;

/// A Ready program the matrix executes and the failure sites it reports
/// against: a request's owned Ready execution view, or a harness program
/// built over a snapshot.
pub trait ReadyExecution {
    fn program(&self) -> &ReadyProgram;
    fn sites(&self) -> Sites<'_>;
    /// The program's objects under `emission`, emitted afresh; a [`Built`] program
    /// emits each once.
    fn objects(&self, emission: Emission) -> Objects {
        emit_backend_objects(
            self.program(),
            emission.backend,
            emission.opt,
            emission.partition,
            emission.measurement,
            None,
        )
        .map(Arc::from)
    }
}
impl<T: ReadyExecution + ?Sized> ReadyExecution for Arc<T> {
    fn program(&self) -> &ReadyProgram {
        self.as_ref().program()
    }
    fn sites(&self) -> Sites<'_> {
        self.as_ref().sites()
    }
    fn objects(&self, emission: Emission) -> Objects {
        self.as_ref().objects(emission)
    }
}

/// What one backend emission of a Ready program is made under: its backend,
/// optimization level, partition and runtime measurement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Emission {
    pub backend: Backend,
    pub opt: OptLevel,
    pub partition: Partition,
    pub measurement: bool,
}

/// One emission's objects, shared by every link of them.
pub type Objects = Result<Arc<[ObjectArtifact]>, NativeError>;

/// A Ready program that emits each [`Emission`] once, however many
/// configurations and campaigns link it; a caller asking for one another is
/// emitting waits for it.
pub struct Built<P> {
    ready: P,
    emitted: Mutex<Vec<(Emission, Arc<OnceLock<Objects>>)>>,
}
impl<P: ReadyExecution> ReadyExecution for Built<P> {
    fn program(&self) -> &ReadyProgram {
        self.ready.program()
    }
    fn sites(&self) -> Sites<'_> {
        self.ready.sites()
    }
    fn objects(&self, emission: Emission) -> Objects {
        let cell = {
            let mut emitted = self.emitted.lock().unwrap_or_else(PoisonError::into_inner);
            match emitted.iter().find(|(e, _)| *e == emission) {
                Some((_, cell)) => cell.clone(),
                None => {
                    let cell = Arc::new(OnceLock::new());
                    emitted.push((emission, cell.clone()));
                    cell
                }
            }
        };
        cell.get_or_init(|| self.ready.objects(emission)).clone()
    }
}

/// One case's builds: a Ready program per distinct pass options
/// ([`same_options`]), built by `supply` the first time a campaign asks for it,
/// with its emissions. Campaigns over the same `Builds` build nothing twice,
/// and campaigns on several threads may share it: one build runs at a time,
/// and a campaign asking for a program another is building waits for it.
pub struct Builds<P, F> {
    supply: Mutex<F>,
    built: Mutex<Vec<(Options, Arc<OnceLock<BuiltReady<P>>>)>>,
}
type BuiltReady<P> = Result<Arc<Built<P>>, NativeError>;
impl<P: ReadyExecution, F: FnMut(&Options) -> Result<P, NativeError>> Builds<P, F> {
    pub fn new(supply: F) -> Self {
        Builds {
            supply: Mutex::new(supply),
            built: Mutex::default(),
        }
    }
    /// The program `options` builds; a failed build fails every request for it.
    pub fn ready(&self, options: &Options) -> BuiltReady<P> {
        let cell = {
            let mut built = self.built.lock().unwrap_or_else(PoisonError::into_inner);
            match built.iter().find(|(o, _)| same_options(o, options)) {
                Some((_, cell)) => cell.clone(),
                None => {
                    let cell = Arc::new(OnceLock::new());
                    built.push((options.clone(), cell.clone()));
                    cell
                }
            }
        };
        cell.get_or_init(|| {
            let mut supply = self.supply.lock().unwrap_or_else(PoisonError::into_inner);
            (*supply)(options).map(|ready| {
                Arc::new(Built {
                    ready,
                    emitted: Mutex::default(),
                })
            })
        })
        .clone()
    }
}
/// A harness program built over a validated snapshot.
pub struct SnapshotReady<'a> {
    program: ReadyProgram,
    input: &'a ValidatedNativeInput,
}
impl ReadyExecution for SnapshotReady<'_> {
    fn program(&self) -> &ReadyProgram {
        &self.program
    }
    fn sites(&self) -> Sites<'_> {
        Sites::new(self.input.snapshot())
    }
}
impl<'a> SnapshotReady<'a> {
    fn build(input: &'a ValidatedNativeInput, options: &Options) -> Result<Self, NativeError> {
        Ok(Self {
            program: build(input, options)?,
            input,
        })
    }
}
/// Supplies the Ready program of each distinct pass `Options`.
pub type ReadySupplier<'a, P> = dyn FnMut(&Options) -> Result<P, NativeError> + 'a;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Engine {
    /// The checked heap executor; with `campaign`, also one audited run per
    /// failure-capable site with that site failed.
    Heap { campaign: bool },
    /// A linked executable on one runtime archive.
    Native {
        opt: OptLevel,
        partition: Partition,
        runtime: Runtime,
    },
    /// A linked executable whose objects and launcher Cranelift emitted (fast debug
    /// builds), held to the LLVM configurations' observations by the parity gate.
    Cranelift {
        partition: Partition,
        runtime: Runtime,
    },
}

/// The runtime archive a native configuration links, and how it launches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Runtime {
    /// The diagnostic archive with poisoning, header checks and the exit
    /// leak audit on. Its compiled fast-path words stay zero, so every
    /// allocation, free and dup reaches a checked runtime entry.
    Diagnostic,
    /// The archive `hirsel run --native` ships: the inlined fast paths run.
    Default,
    /// The system-allocator archive: each fixed-size block is one libc
    /// allocation, so memcheck sees its use after free.
    System,
    /// The shipped size-class allocator built with Valgrind client requests:
    /// memcheck sees slab blocks, region reservations and arena blocks, and
    /// every allocation, free and arena bump reaches the runtime.
    Memcheck,
}

impl Runtime {
    pub fn name(self) -> &'static str {
        match self {
            Self::Diagnostic => "diagnostic",
            Self::Default => "default",
            Self::System => "system",
            Self::Memcheck => "memcheck",
        }
    }

    pub fn build(self) -> RuntimeBuild {
        RuntimeBuild {
            diagnostic: self == Self::Diagnostic,
            system_allocator: self == Self::System,
            measurement: false,
            memcheck: self == Self::Memcheck,
            ownership_trace: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Configuration {
    pub options: Options,
    pub engine: Engine,
}

impl Configuration {
    pub fn name(&self) -> String {
        let o = &self.options;
        let arch = o.target.triple.split('-').next().unwrap_or("?");
        let passes = format!(
            "{arch}/{}/{}/{}/{}",
            if o.regions { "regions" } else { "counted" },
            if o.elide { "elide" } else { "insert-only" },
            if o.reuse { "reuse" } else { "no-reuse" },
            if o.flat_walk {
                "flat-walk"
            } else {
                "no-flat-walk"
            },
        );
        let name = match self.engine {
            Engine::Heap { campaign } => {
                format!("{passes}/heap{}", if campaign { "+inject" } else { "" })
            }
            Engine::Native {
                opt,
                partition,
                runtime,
            } => format!(
                "{passes}/llvm-{}/{}/{}",
                match opt {
                    OptLevel::None => "O0",
                    OptLevel::Default => "O2",
                    OptLevel::Speed => "O3",
                },
                partition.name(),
                runtime.name(),
            ),
            Engine::Cranelift { partition, runtime } => {
                format!("{passes}/cranelift/{}/{}", partition.name(), runtime.name())
            }
        };
        let mut name = name;
        for (field, seed) in [
            ("hash", o.launch.hash_seed),
            ("random", o.launch.random_seed),
            ("test", o.launch.test_seed),
        ] {
            if seed != 0 {
                name.push_str(&format!("/{field}-{seed}"));
            }
        }
        name
    }
}

fn options(target: &TargetSpec, regions: bool, elide: bool, reuse: bool) -> Options {
    Options {
        validation: hirsel_native::Validation::EveryStage,
        launch: Default::default(),
        target: target.clone(),
        regions,
        elide,
        reuse,
        flat_walk: true,
        scoped_specialization: true,
        discharge: Default::default(),
        places: Vec::new(),
    }
}

/// The per-push smoke matrix: the default pipeline on the heap executor with
/// fault injection, and one optimized x86-64 executable on the diagnostic
/// archive and one on the shipped archive.
pub fn smoke_matrix() -> Vec<Configuration> {
    let x86 = TargetSpec::x86_64_unknown_linux_gnu();
    let mut out = vec![Configuration {
        options: Options::new(x86.clone()),
        engine: Engine::Heap { campaign: true },
    }];
    for runtime in [Runtime::Diagnostic, Runtime::Default] {
        out.push(Configuration {
            options: Options::new(x86.clone()),
            engine: Engine::Native {
                opt: OptLevel::Speed,
                partition: Partition::Whole,
                runtime,
            },
        });
    }
    out
}

/// The full matrix: both executed targets; regions on and off; insertion
/// only, elision, and reuse; the heap executor on every pass configuration;
/// native code on the diagnostic archive at O0 and O3 as one object and one
/// object per unit; and, on the default passes at O3 as one object and one
/// object per unit, the shipped and system-allocator archives.
pub fn full_matrix() -> Vec<Configuration> {
    let mut out = Vec::new();
    for target in [
        TargetSpec::x86_64_unknown_linux_gnu(),
        TargetSpec::aarch64_unknown_linux_gnu(),
    ] {
        for regions in [true, false] {
            for (elide, reuse) in [(false, false), (true, false), (true, true)] {
                let options = options(&target, regions, elide, reuse);
                let default = regions && elide && reuse;
                out.push(Configuration {
                    options: options.clone(),
                    engine: Engine::Heap { campaign: default },
                });
                for opt in [OptLevel::None, OptLevel::Speed] {
                    for partition in [Partition::Whole, Partition::Singleton] {
                        // The off-default pass configurations run as one
                        // object; topology is varied on the default.
                        if partition == Partition::Singleton && !default {
                            continue;
                        }
                        out.push(Configuration {
                            options: options.clone(),
                            engine: Engine::Native {
                                opt,
                                partition,
                                runtime: Runtime::Diagnostic,
                            },
                        });
                    }
                }
                if default {
                    for runtime in [Runtime::Default, Runtime::System] {
                        for partition in [Partition::Whole, Partition::Singleton] {
                            out.push(Configuration {
                                options: options.clone(),
                                engine: Engine::Native {
                                    opt: OptLevel::Speed,
                                    partition,
                                    runtime,
                                },
                            });
                        }
                    }
                }
            }
        }
    }
    for target in [
        TargetSpec::x86_64_unknown_linux_gnu(),
        TargetSpec::aarch64_unknown_linux_gnu(),
    ] {
        let mut options = Options::new(target);
        options.flat_walk = false;
        out.push(Configuration {
            options,
            engine: Engine::Native {
                opt: OptLevel::Speed,
                partition: Partition::Whole,
                runtime: Runtime::Default,
            },
        });
    }
    out
}

/// The `--trace-roots` matrix (S-14): the default passes as one unoptimized x86-64 LLVM
/// executable and one Cranelift executable, both on the diagnostic archive, whose traces
/// must equal the interpreter's. Root releases are runtime calls placed by the passes, which
/// no LLVM optimization reorders, so the parity set's level checks them at its cost.
pub fn trace_roots_matrix() -> Vec<Configuration> {
    let x86 = TargetSpec::x86_64_unknown_linux_gnu();
    vec![
        Configuration {
            options: Options::new(x86.clone()),
            engine: Engine::Native {
                opt: OptLevel::None,
                partition: Partition::Whole,
                runtime: Runtime::Diagnostic,
            },
        },
        Configuration {
            options: Options::new(x86),
            engine: Engine::Cranelift {
                partition: Partition::Whole,
                runtime: Runtime::Diagnostic,
            },
        },
    ]
}

/// The `--migrate` matrix (design §9 W1): one optimized x86-64 LLVM executable and one
/// Cranelift executable, both on the optimized default archive, run with every suspension
/// migrating its task to another compute worker; their observations must equal the
/// interpreter's.
pub fn migrate_matrix() -> Vec<Configuration> {
    let x86 = TargetSpec::x86_64_unknown_linux_gnu();
    vec![
        Configuration {
            options: Options::new(x86.clone()),
            engine: Engine::Native {
                opt: OptLevel::Speed,
                partition: Partition::Whole,
                runtime: Runtime::Default,
            },
        },
        Configuration {
            options: Options::new(x86),
            engine: Engine::Cranelift {
                partition: Partition::Whole,
                runtime: Runtime::Default,
            },
        },
    ]
}

/// Every archive variant `matrix`'s native configurations link, the
/// diagnostic archive first (and alone when there are none).
pub fn runtime_builds(matrix: &[Configuration]) -> Vec<RuntimeBuild> {
    let mut builds = vec![Runtime::Diagnostic.build()];
    for configuration in matrix {
        if let Engine::Native { runtime, .. } | Engine::Cranelift { runtime, .. } =
            configuration.engine
            && !builds.contains(&runtime.build())
        {
            builds.push(runtime.build());
        }
    }
    builds
}

/// The in-tree toolchain for `matrix`: its targets, and every archive its
/// native configurations link, with the diagnostic archive selected.
pub fn toolchain_for_matrix(
    build_dir: &Path,
    matrix: &[Configuration],
) -> Result<Toolchain, NativeError> {
    let mut targets: Vec<&TargetSpec> = Vec::new();
    for c in matrix {
        if !targets.contains(&&c.options.target) {
            targets.push(&c.options.target);
        }
    }
    Toolchain::in_tree_with_runtimes(build_dir, &targets, &runtime_builds(matrix))
}
/// The topology matrix (PROG-5): one object, one object per unit and
/// `seeds` seeded random partitions of the default pipeline on x86-64 at O3.
pub fn topology_matrix(seeds: u64) -> Vec<Configuration> {
    let options = Options::new(TargetSpec::x86_64_unknown_linux_gnu());
    [Partition::Whole, Partition::Singleton]
        .into_iter()
        .chain((1..=seeds).map(Partition::Random))
        .map(|partition| Configuration {
            options: options.clone(),
            engine: Engine::Native {
                opt: OptLevel::Speed,
                partition,
                runtime: Runtime::Diagnostic,
            },
        })
        .collect()
}

/// What one configuration did with one program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The engine's observation equals the reference's.
    Agree,
    /// The engine observed this instead.
    Mismatch(Observation),
    /// A tool outcome: no observation was produced.
    Unavailable(NativeError),
    /// heap+inject over a site bound (`Fixture::heap_campaign_sites_max`): the
    /// clean run's observation equals the reference's, and its `sites` fault
    /// sites are injected by campaign parts, not by this run.
    Parted { sites: u64 },
}

/// Whether two pass selections build the same Ready program.
pub fn same_options(a: &Options, b: &Options) -> bool {
    a.launch == b.launch
        && a.target == b.target
        && a.regions == b.regions
        && a.elide == b.elide
        && a.reuse == b.reuse
        && a.flat_walk == b.flat_walk
        && a.scoped_specialization == b.scoped_specialization
        && a.places == b.places
}

/// Run `entry` under each configuration and compare with `reference`.
/// Configurations that share pass options share one Ready program, and native
/// configurations that differ only by runtime archive share its objects.
pub fn differential(
    validated: &ValidatedNativeInput,
    entry: &DefPath,
    reference: &Observation,
    configurations: &[Configuration],
    toolchain: &Toolchain,
    dir: &Path,
) -> Vec<(String, Outcome)> {
    differential_ready(
        &mut |options: &Options| SnapshotReady::build(validated, options),
        entry,
        reference,
        configurations,
        toolchain,
        dir,
        &Fixture::default(),
    )
}

/// `options` building `fixture`'s program: with the place heads its runners name
/// (`Options::places`), whatever its passes.
fn fixture_options(options: &Options, fixture: &Fixture) -> Options {
    Options {
        places: fixture.place_heads(),
        ..options.clone()
    }
}

/// The matrix over supplied Ready programs, one per distinct pass options.
pub fn differential_ready<P: ReadyExecution>(
    ready: &mut ReadySupplier<'_, P>,
    entry: &DefPath,
    reference: &Observation,
    configurations: &[Configuration],
    toolchain: &Toolchain,
    dir: &Path,
    fixture: &Fixture,
) -> Vec<(String, Outcome)> {
    let world = crate::HeapWorld {
        campaign_partition: fixture.heap_campaign_partition,
        instance: [0; 16],
        runners: fixture.runners.clone(),
        callees: fixture.callees.clone(),
        authorities: fixture.authorities.clone(),
        cpu: fixture.cpu.clone(),
        tasks_free: fixture.tasks_free,
        stdin: fixture.stdin.clone(),
        args: fixture.args.clone(),
        files: fixture.files.clone(),
        mounts: fixture.mounts.clone(),
        file_reads: fixture.file_reads.clone(),
        file_faults: fixture.file_faults.clone(),
        deny_output: fixture.deny_output,
        monotonic: fixture.monotonic,
        process: fixture.process.clone(),
        net: fixture.net.clone(),
        signals: fixture.signals.clone(),
        env: fixture.env.clone(),
        steps: fixture.steps,
    };
    let builds = Builds::new(|options: &Options| ready(options));
    let mut out = Vec::with_capacity(configurations.len());
    // Configurations that differ only by runtime archive share their objects.
    let native = |backend: Backend,
                  opt: OptLevel,
                  partition: Partition,
                  runtime: Runtime,
                  ready: &Built<P>| {
        let (program, sites) = (ready.program(), &ready.sites());
        let toolchain = &toolchain.with_runtime(runtime.build());
        let objects = ready.objects(Emission {
            backend,
            opt,
            partition,
            measurement: toolchain.runtime_build.measurement,
        })?;
        let runnable = link_fixture_runnable(program, entry, &objects, toolchain, dir, fixture)?;
        let target = &program.target;
        let diagnostic = runtime == Runtime::Diagnostic;
        let launched = hirsel_native::link::launch_fixture(
            &runnable,
            target,
            toolchain,
            dir,
            diagnostic,
            hirsel_native::link::Tape::Live {
                record: None,
                revocations: None,
            },
            fixture,
        )?;
        let mut observation = observe(&launched, program, sites)?;
        if fixture.trace_roots {
            observation.roots = std::fs::read_to_string(dir.join(hirsel_native::link::ROOT_TRACE))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect();
        }
        Ok(observation)
    };
    for configuration in configurations {
        let observed = builds
            .ready(&fixture_options(&configuration.options, fixture))
            .and_then(|ready| match configuration.engine {
                Engine::Heap { campaign: true } => {
                    let (program, sites) = (ready.program(), &ready.sites());
                    // What the runs cost, for sizing this campaign's parts.
                    let measure = |sites: u64, clean, campaign| {
                        if let Some(measures) = &fixture.heap_measures {
                            measures.lock().expect("heap measures").push(HeapMeasure {
                                configuration: configuration.name(),
                                sites,
                                clean,
                                campaign,
                            });
                        }
                    };
                    let campaign = || {
                        let started = std::time::Instant::now();
                        crate::heap_campaign_in(program, entry, sites, &world).map(|run| {
                            let campaign = HeapCampaignMeasure {
                                partition: world.campaign_partition,
                                wall: started.elapsed(),
                                timing: run.timing.unwrap_or_default(),
                            };
                            (run.observation, run.sites, campaign)
                        })
                    };
                    match fixture.heap_campaign_sites_max {
                        Some(max) if world.campaign_partition.is_none() => {
                            let started = std::time::Instant::now();
                            crate::run_heap_in(program, entry, sites, &world).and_then(|clean| {
                                let wall = Some(started.elapsed());
                                if clean.sites > max {
                                    measure(clean.sites, wall, None);
                                    Ok((clean.observation, Some(clean.sites)))
                                } else {
                                    campaign().map(|(observation, sites, run)| {
                                        measure(sites, wall, Some(run));
                                        (observation, None)
                                    })
                                }
                            })
                        }
                        _ => campaign().map(|(observation, sites, run)| {
                            measure(sites, None, Some(run));
                            (observation, None)
                        }),
                    }
                }
                Engine::Heap { campaign: false } => {
                    crate::run_heap_in(ready.program(), entry, &ready.sites(), &world)
                        .map(|run| (run.observation, None))
                }
                Engine::Native {
                    opt,
                    partition,
                    runtime,
                } => native(Backend::Llvm, opt, partition, runtime, &ready).map(|o| (o, None)),
                Engine::Cranelift { partition, runtime } => native(
                    Backend::Cranelift,
                    OptLevel::None,
                    partition,
                    runtime,
                    &ready,
                )
                .map(|o| (o, None)),
            });
        let outcome = match observed {
            Ok((observation, Some(sites))) if observation == *reference => {
                Outcome::Parted { sites }
            }
            Ok((observation, None)) if observation == *reference => Outcome::Agree,
            Ok((observation, _)) => Outcome::Mismatch(observation),
            Err(error) => Outcome::Unavailable(error),
        };
        out.push((configuration.name(), outcome));
    }
    out
}

/// Compare executable and runner launches of supplied Ready programs.
pub fn runner_oracle_ready<P: ReadyExecution>(
    ready: &mut ReadySupplier<'_, P>,
    entry: &DefPath,
    configurations: &[Configuration],
    toolchain: &Toolchain,
    dir: &Path,
    fixture: &Fixture,
    tapes: bool,
) -> Vec<(String, Outcome)> {
    let builds = Builds::new(|options: &Options| ready(options));
    let mut out = Vec::new();
    for configuration in configurations {
        let (backend, opt, partition, runtime) = match configuration.engine {
            Engine::Heap { .. } => continue,
            Engine::Native {
                opt,
                partition,
                runtime,
            } => (Backend::Llvm, opt, partition, runtime),
            Engine::Cranelift { partition, runtime } => {
                (Backend::Cranelift, OptLevel::None, partition, runtime)
            }
        };
        let toolchain = toolchain.with_runtime(runtime.build());
        let diagnostic = runtime == Runtime::Diagnostic;
        let run = || -> Result<(), NativeError> {
            let ready = builds.ready(&fixture_options(&configuration.options, fixture))?;
            let (program, sites) = (ready.program(), ready.sites());
            let target = &program.target;
            let objects = ready.objects(Emission {
                backend,
                opt,
                partition,
                measurement: toolchain.runtime_build.measurement,
            })?;
            let executable = Toolchain {
                execution: hirsel_native::runner::Execution::Executable,
                ..toolchain.clone()
            };
            let paths = [
                link_fixture_runnable(program, entry, &objects, &executable, dir, fixture)?,
                link_fixture_runnable(program, entry, &objects, &toolchain, dir, fixture)?,
            ];
            let launch = |runnable: &hirsel_native::link::Runnable,
                          tape: hirsel_native::link::Tape<'_>| {
                hirsel_native::link::launch_fixture(
                    runnable, target, &toolchain, dir, diagnostic, tape, fixture,
                )
            };
            let live = hirsel_native::link::Tape::Live {
                record: None,
                revocations: None,
            };
            let [linked, loaded] = [launch(&paths[0], live)?, launch(&paths[1], live)?];
            for (field, a, b) in [
                (
                    "exit status",
                    format!("{:?}", linked.exit),
                    format!("{:?}", loaded.exit),
                ),
                (
                    "stdout",
                    String::from_utf8_lossy(&linked.stdout).into_owned(),
                    String::from_utf8_lossy(&loaded.stdout).into_owned(),
                ),
                (
                    "stderr",
                    String::from_utf8_lossy(&linked.stderr).into_owned(),
                    String::from_utf8_lossy(&loaded.stderr).into_owned(),
                ),
                (
                    "observation record",
                    linked.record.clone(),
                    loaded.record.clone(),
                ),
            ] {
                if a != b {
                    return Err(NativeError::new(
                        StageName::Launch,
                        format!(
                            "runner oracle: the {field} differs: executable {a:?}, runner {b:?}"
                        ),
                    ));
                }
            }
            if !tapes {
                return Ok(());
            }
            let mut replays = Vec::new();
            for (recorder, replayer) in [(0, 1), (1, 0)] {
                let tape = dir.join(format!("oracle-tape-{recorder}"));
                let _ = std::fs::remove_file(&tape);
                let recorded = launch(
                    &paths[recorder],
                    hirsel_native::link::Tape::Live {
                        record: Some(&tape),
                        revocations: None,
                    },
                )?;
                let mut recorded = observe(&recorded, program, &sites)?;
                let replayed = launch(&paths[replayer], hirsel_native::link::Tape::Replay(&tape))?;
                let replayed = observe(&replayed, program, &sites)?;
                // A run with runners replays without its callees, printing only its own: an
                // ordered tape's prints, or a Free phase tape's, which its replay checks.
                if !fixture.runners.is_empty() {
                    recorded.output = match tape_events(&tape)? {
                        Some(events) => recorded_prints(&events),
                        None => replayed.output.clone(),
                    };
                }
                if replayed != recorded {
                    return Err(NativeError::new(
                        StageName::Launch,
                        format!(
                            "runner oracle: a tape recorded by the {} replays differently",
                            ["executable", "runner"][recorder]
                        ),
                    ));
                }
                replays.push(replayed);
            }
            if replays[0] != replays[1] {
                return Err(NativeError::new(
                    StageName::Launch,
                    "runner oracle: the cross replays observe differently",
                ));
            }
            Ok(())
        };
        let outcome = match run() {
            Ok(()) => Outcome::Agree,
            Err(error) => Outcome::Unavailable(error),
        };
        out.push((format!("{}/runner-oracle", configuration.name()), outcome));
    }
    out
}

/// The events of the Io tape at `tape`; none for a Free phase tape, whose events are its
/// tasks' logs.
fn tape_events(tape: &Path) -> Result<Option<Vec<hirsel_rt::io::Event>>, NativeError> {
    let io = |e: String| NativeError::new(StageName::Launch, format!("the Io tape: {e}"));
    let bytes = std::fs::read(tape).map_err(|e| io(e.to_string()))?;
    if hirsel_rt::launch::is_free_tape(&bytes) {
        return Ok(None);
    }
    let text = hirsel_rt::launch::read_recording_text(tape).map_err(|e| io(e.to_string()))?;
    hirsel_rt::launch::parse_recording(&text)
        .map(|record| Some(record.events))
        .map_err(io)
}

/// What a tape's run printed, in order.
fn recorded_prints(events: &[hirsel_rt::io::Event]) -> Vec<u8> {
    let mut printed = Vec::new();
    for event in events {
        // Test log and snapshot records reach stdout through Test reporting, not Print;
        // their bytes are re-rendered in order. An invalid snapshot name records nothing.
        match &event.request {
            hirsel_rt::io::Request::Print { bytes, .. } => printed.extend_from_slice(bytes),
            hirsel_rt::io::Request::TestLog { text, .. } => {
                printed.extend(hirsel_rt::kernels::report::log(text))
            }
            hirsel_rt::io::Request::TestSnapshot { name, text, .. }
                if event.completion == hirsel_rt::io::Completion::Checked(true) =>
            {
                printed.extend(hirsel_rt::kernels::report::snapshot(name, text))
            }
            _ => {}
        }
    }
    printed
}

/// Real Io replay: run the program as an optimized x86-64
/// executable on real Io with its completion tape recorded, then run the same
/// executable on simulated Io consuming exactly that tape. Both runs must
/// observe what the reference does, and the tape's prints, in order, must be
/// the reference's output. A run with runners replays without its callees (RUN-11): its
/// replay observes the recorded run with only the tape's own prints. The first
/// disagreement is the outcome.
pub fn real_io_replay(
    validated: &ValidatedNativeInput,
    entry: &DefPath,
    reference: &Observation,
    toolchain: &Toolchain,
    dir: &Path,
) -> Outcome {
    real_io_replay_with_config(
        validated,
        entry,
        reference,
        toolchain,
        dir,
        Default::default(),
    )
}

pub fn real_io_replay_with_config(
    validated: &ValidatedNativeInput,
    entry: &DefPath,
    reference: &Observation,
    toolchain: &Toolchain,
    dir: &Path,
    launch_config: hirsel_native::LaunchInputs,
) -> Outcome {
    real_io_replay_ready(
        &mut |options: &Options| SnapshotReady::build(validated, options),
        entry,
        reference,
        toolchain,
        dir,
        launch_config,
        &Fixture::default(),
    )
}

/// The Ready pass options real Io replay builds under `launch`.
pub fn real_io_replay_options(launch: hirsel_native::LaunchInputs) -> Options {
    let mut options = Options::new(TargetSpec::x86_64_unknown_linux_gnu());
    options.launch = launch;
    options
}

/// Record and replay a supplied Ready program.
pub fn real_io_replay_ready<P: ReadyExecution>(
    ready: &mut ReadySupplier<'_, P>,
    entry: &DefPath,
    reference: &Observation,
    toolchain: &Toolchain,
    dir: &Path,
    launch_config: hirsel_native::LaunchInputs,
    fixture: &Fixture,
) -> Outcome {
    // Replay checks the tape against the diagnostic archive's audited run.
    let toolchain = &toolchain.with_runtime(Runtime::Diagnostic.build());
    let mut run = || {
        let options = real_io_replay_options(launch_config.clone());
        let target = options.target.clone();
        let ready = ready(&fixture_options(&options, fixture))?;
        let (program, sites) = (ready.program(), &ready.sites());
        let objects = ready.objects(Emission {
            backend: Backend::Llvm,
            opt: OptLevel::Speed,
            partition: Partition::Whole,
            measurement: toolchain.runtime_build.measurement,
        })?;
        let runnable = link_fixture_runnable(program, entry, &objects, toolchain, dir, fixture)?;
        let tape = dir.join("io-tape");
        let _ = std::fs::remove_file(&tape);
        let recorded = hirsel_native::link::launch_fixture(
            &runnable,
            &target,
            toolchain,
            dir,
            true,
            hirsel_native::link::Tape::Live {
                record: Some(&tape),
                revocations: None,
            },
            fixture,
        )?;
        let real = observe(&recorded, program, sites)?;
        if real != *reference {
            return Ok(real);
        }
        // A tasks.free@1 run's Free phase tape: its replay checks each print against the
        // task log that recorded it, so the replay's prints are the tape's.
        let bytes = std::fs::read(&tape)
            .map_err(|e| NativeError::new(StageName::Launch, format!("the Io tape: {e}")))?;
        if hirsel_rt::launch::is_free_tape(&bytes) {
            if reference.signaled.is_some() {
                return Err(NativeError::new(
                    StageName::Launch,
                    "a signaled tasks.free@1 fixture has no replay check",
                ));
            }
            let replayed = hirsel_native::link::launch_fixture(
                &runnable,
                &target,
                toolchain,
                dir,
                true,
                hirsel_native::link::Tape::Replay(&tape),
                fixture,
            )?;
            let replayed = observe(&replayed, program, sites)?;
            // A run with runners replays without its callees (RUN-11), printing only its own.
            if !fixture.runners.is_empty() {
                let expected = Observation {
                    output: replayed.output.clone(),
                    ..real.clone()
                };
                return Ok(if replayed == expected { real } else { replayed });
            }
            return Ok(replayed);
        }
        let text = hirsel_rt::launch::read_recording_text(&tape)
            .map_err(|e| NativeError::new(StageName::Launch, format!("the Io tape: {e}")))?;
        // A run a signal's default action ended leaves a recording with no trailer, as a
        // killed process does (determinism.md, The Signaled end): nothing to replay.
        if reference.signaled.is_some() {
            return match hirsel_rt::launch::parse_recording_prefix(&text) {
                Ok((_, false)) => Ok(real),
                Ok((_, true)) => Err(NativeError::new(
                    StageName::Launch,
                    "a signaled run's recording is complete",
                )),
                Err(error) => Err(NativeError::new(
                    StageName::Launch,
                    format!("the Io tape: {error}"),
                )),
            };
        }
        let events = hirsel_rt::launch::parse_recording(&text)
            .map(|record| record.events)
            .map_err(|e| NativeError::new(StageName::Launch, format!("the Io tape: {e}")))?;
        let printed = recorded_prints(&events);
        // A Test report (a failed check, a scripted peer's or child's Expect) is no Print:
        // replay re-derives it, and the replayed observation must still be the reference.
        let reported = events
            .iter()
            .any(|e| e.completion == hirsel_rt::io::Completion::Checked(false));
        // A run with runners replays without its callees (RUN-11): its tape holds only its
        // own prints, which its replay alone reproduces; each callee's are on its own tape.
        let alone = !fixture.runners.is_empty();
        if !alone
            && printed != reference.output
            && !(reported && reference.output.starts_with(&printed))
        {
            return Err(NativeError::new(
                StageName::Launch,
                "the recorded prints are not the reference output",
            ));
        }
        let replayed = hirsel_native::link::launch_fixture(
            &runnable,
            &target,
            toolchain,
            dir,
            true,
            hirsel_native::link::Tape::Replay(&tape),
            fixture,
        )?;
        let replayed = observe(&replayed, program, sites)?;
        if alone {
            let expected = Observation {
                output: printed,
                ..real.clone()
            };
            return Ok(if replayed == expected { real } else { replayed });
        }
        Ok(replayed)
    };
    match run() {
        Ok(observation) if observation == *reference => Outcome::Agree,
        Ok(observation) => Outcome::Mismatch(observation),
        Err(error) => Outcome::Unavailable(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ready_program_cache_distinguishes_every_launch_input() {
        let baseline = Options::new(TargetSpec::x86_64_unknown_linux_gnu());
        assert!(same_options(&baseline, &baseline.clone()));
        for field in 0..4 {
            let mut changed = baseline.clone();
            match field {
                0 => changed.launch.hash_seed = 1,
                1 => changed.launch.random_seed = 1,
                2 => changed.launch.test_seed = 1,
                _ => {
                    changed.launch.scheduler = Some(hirsel_run_schema::Scheduler {
                        algorithm: "unsupported".into(),
                        version: 1,
                        seed: 1,
                    })
                }
            }
            assert!(!same_options(&baseline, &changed));
        }
    }
}
