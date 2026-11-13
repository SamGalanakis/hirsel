//! Native products compose the checked body and the two engines directly.
use super::*;
use hirsel_core_ir::InstanceKey;
use hirsel_core_layout::{Layout, Recipe};
use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[path = "entry_cleanup.rs"]
mod cleanup;
#[path = "release_native.rs"]
mod release;
use cleanup::result_cleanup;

pub const TARGET: &str = "x86_64-unknown-linux-gnu";
#[derive(Clone, Copy, Debug, Serialize)]
pub enum Backend {
    Cranelift,
    Llvm,
}
impl std::str::FromStr for Backend {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "cranelift" => Ok(Self::Cranelift),
            "llvm" => Ok(Self::Llvm),
            _ => Err("expected cranelift or llvm"),
        }
    }
}
/// Runtime and linker inputs are explicit, independently produced artifacts.
pub struct NativeOptions {
    pub backend: Backend,
    pub optimize: bool,
    pub target: String,
    pub runtime: PathBuf,
    pub include: PathBuf,
    pub cc: PathBuf,
    pub store: PathBuf,
    pub output: PathBuf,
    pub root_limit: u64,
    pub instance_limit: usize,
    /// Paths are opened relative to the executable's working directory at startup.
    pub directories: Vec<String>,
    /// Explicit host authority, using the runtime's grant-kind bit positions.
    pub grant_mask: u64,
    /// Diagnostic build: exact root reservation peak, unsuitable for timing samples.
    pub logical_charge_report: Option<PathBuf>,
}
#[derive(Debug)]
pub enum NativeStop {
    Check(String),
    Instances(InstanceStop),
    Entry(String),
    Emit {
        definition: DefPath,
        message: String,
    },
    Link(String),
    Input(io::Error),
}
impl fmt::Display for NativeStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Check(s) => write!(f, "check: {s}"),
            Self::Instances(s) => write!(f, "instances: {s:?}"),
            Self::Entry(s) => write!(f, "entry: {s}"),
            Self::Emit {
                definition,
                message,
            } => write!(f, "emit {definition}: {message}"),
            Self::Link(s) => write!(f, "link: {s}"),
            Self::Input(e) => write!(f, "input: {e}"),
        }
    }
}
impl std::error::Error for NativeStop {}
impl From<io::Error> for NativeStop {
    fn from(e: io::Error) -> Self {
        Self::Input(e)
    }
}
pub struct ObjectProduct {
    pub instance: InstanceKey,
    pub identity: ContentHash,
    pub bytes: Vec<u8>,
    pub compiled: bool,
}
#[derive(Serialize)]
pub struct BuildProduct {
    pub executable: PathBuf,
    pub identity: ContentHash,
    pub compiled: usize,
    pub linked: bool,
    /// True means diagnostic instrumentation; timings are not performance samples.
    pub observation_mode: bool,
}
fn key(domain: &str, value: &impl Serialize) -> io::Result<ContentHash> {
    Ok(ContentHash::of_parts(domain, [&encode(value)?[..]]))
}
fn selection(hash: ContentHash) -> String {
    hash.0.iter().map(|b| format!("{b:02x}")).collect()
}
fn cached(store: &Store, key: ContentHash) -> io::Result<Option<(ContentHash, Vec<u8>)>> {
    let Some(hash) = store.selection(&selection(key))? else {
        return Ok(None);
    };
    Ok(store.get(hash)?.map(|bytes| (hash, bytes)))
}
fn publish(store: &Store, key: ContentHash, bytes: &[u8]) -> io::Result<ContentHash> {
    let hash = store.put(bytes)?;
    store.select(&selection(key), hash)?;
    Ok(hash)
}
fn read_input(path: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}
// A unit is the producer's complete module, including type shapes and finalizers.
// The cache chooses its granularity; this boundary never strips type/drop inputs.
fn emit_unit(module: &Module, options: &NativeOptions) -> Result<Vec<u8>, String> {
    match options.backend {
        Backend::Cranelift => hirsel_cranelift::core::emit_object_with_charge_observation(
            module,
            &hirsel_core_layout::compute(&module.types).expect("producer layouts"),
            &hirsel_cranelift::core::Options {
                target: &options.target,
                optimize: options.optimize,
            },
            options.logical_charge_report.is_some(),
        )
        .map(|o| o.bytes)
        .map_err(|e| e.to_string()),
        Backend::Llvm => hirsel_llvm_lazy::core::emit_object_with_charge_observation(
            module,
            if options.optimize {
                hirsel_llvm_lazy::core::Optimization::Speed
            } else {
                hirsel_llvm_lazy::core::Optimization::None
            },
            options.logical_charge_report.is_some(),
        )
        .map_err(|e| e.to_string()),
    }
}

/// Recipe glue may be present in several independent definition products. Its
/// instance identity names one symbol; later units import the first definition.
fn define_once<'a>(module: &'a Module, emitted: &mut HashSet<InstanceKey>) -> Cow<'a, Module> {
    let mut result = Cow::Borrowed(module);
    for (i, function) in module.functions.iter().enumerate() {
        if function.body.is_some()
            && !emitted.insert(module.instances[function.instance.index()].clone())
        {
            result.to_mut().functions[i].body = None;
        }
    }
    result
}

impl CheckProduct {
    /// Dev keeps each checked body as an independent object. LLVM release
    /// composes owned bodies and published inline imports into one codegen unit.
    pub fn emit_reachable(
        &self,
        entry: &DefPath,
        options: &NativeOptions,
    ) -> Result<Vec<ObjectProduct>, NativeStop> {
        let reached = self.reachable(entry, options.instance_limit)?;
        self.emit_instances(entry, reached.require()?, &reached.materialized, options)
    }
    fn emit_instances(
        &self,
        entry: &DefPath,
        reached: &[InstanceKey],
        materialized: &crate::instances::InstanceSet,
        options: &NativeOptions,
    ) -> Result<Vec<ObjectProduct>, NativeStop> {
        if options.target != TARGET {
            return Err(NativeStop::Entry(format!(
                "native engines currently target {TARGET}"
            )));
        }
        let runtime = ContentHash::of_parts(
            "hirsel.core-runtime.v1",
            [&read_input(&options.runtime)?[..]],
        );
        let store = Store::new(&options.store)?;
        let library_store = self
            .image
            .as_ref()
            .map(|i| Store::new(&i.store_root().join("native")))
            .transpose()?;
        if matches!(options.backend, Backend::Llvm) && options.optimize {
            return self.emit_release(
                entry,
                reached,
                materialized,
                options,
                runtime,
                &store,
                library_store.as_ref(),
            );
        }
        let mut objects = Vec::new();
        let mut emitted = HashSet::new();
        for instance in reached {
            let store = if !self.definitions.contains_key(&instance.definition) {
                library_store.as_ref().unwrap_or(&store)
            } else {
                &store
            };
            let instance_body = materialized.get(instance);
            let body = &instance_body.product;
            let ir = define_once(instance_body.ir(), &mut emitted);
            let object_key = key(
                "hirsel.core-object.v1",
                &(
                    compiler_identity()?,
                    &options.target,
                    options.backend,
                    options.optimize,
                    options.logical_charge_report.is_some(),
                    matches!(options.backend, Backend::Llvm)
                        && !hirsel_llvm_lazy::core::facts_enabled(),
                    runtime,
                    &instance,
                    &body.key,
                    &*ir,
                ),
            )?;
            let (identity, bytes, compiled) =
                if let Some((hash, bytes)) = cached(store, object_key)? {
                    (hash, bytes, false)
                } else {
                    let bytes = emit_unit(&ir, options).map_err(|message| NativeStop::Emit {
                        definition: instance.definition.clone(),
                        message,
                    })?;
                    (publish(store, object_key, &bytes)?, bytes, true)
                };
            objects.push(ObjectProduct {
                instance: instance.clone(),
                identity,
                bytes,
                compiled,
            });
        }
        Ok(objects)
    }
    /// Build through the real process runtime. Unsupported checker/entry products
    /// stop explicitly; no alternate scalar-main or interpreter path exists.
    pub fn build(
        &self,
        entry: &DefPath,
        options: &NativeOptions,
    ) -> Result<BuildProduct, NativeStop> {
        let reached = self.reachable(entry, options.instance_limit)?;
        self.build_reachable(entry, &reached, options)
    }
    pub fn build_reachable(
        &self,
        entry: &DefPath,
        reached: &Reachable<'_>,
        options: &NativeOptions,
    ) -> Result<BuildProduct, NativeStop> {
        let instances = reached.require()?;
        let source = self.process_entry(entry, options)?;
        let mut objects = self.emit_instances(entry, instances, &reached.materialized, options)?;
        if let Some(module) = &self.entry_binding(entry)?.cleanup {
            let store = Store::new(&options.store)?;
            let object_key = key(
                "hirsel.core-entry-cleanup.v1",
                &(
                    compiler_identity()?,
                    &options.target,
                    options.backend,
                    options.optimize,
                    options.logical_charge_report.is_some(),
                    matches!(options.backend, Backend::Llvm)
                        && !hirsel_llvm_lazy::core::facts_enabled(),
                    module,
                ),
            )?;
            let (identity, bytes, compiled) =
                if let Some((identity, bytes)) = cached(&store, object_key)? {
                    (identity, bytes, false)
                } else {
                    let bytes = emit_unit(module, options).map_err(|message| NativeStop::Emit {
                        definition: entry.clone(),
                        message,
                    })?;
                    (publish(&store, object_key, &bytes)?, bytes, true)
                };
            objects.push(ObjectProduct {
                instance: module.instances.last().unwrap().clone(),
                identity,
                bytes,
                compiled,
            });
        }
        link(&source, &objects, options)
    }
    fn entry_binding(&self, entry: &DefPath) -> Result<&EntryAbi, NativeStop> {
        let binding = self
            .native_entry
            .as_ref()
            .filter(|(path, _)| path == entry)
            .map(|(_, abi)| abi)
            .or_else(|| self.test_entries.get(entry))
            .ok_or_else(|| {
                NativeStop::Entry("no checked entry selected for this definition".into())
            })?;
        binding.as_ref().map_err(|e| NativeStop::Entry(e.clone()))
    }
    fn process_entry(
        &self,
        entry: &DefPath,
        options: &NativeOptions,
    ) -> Result<String, NativeStop> {
        let binding = self.entry_binding(entry)?;
        let EntryAbi {
            recipe,
            exit_offset,
            parameters,
            cleanup,
        } = binding;
        let instance = InstanceKey {
            definition: entry.clone(),
            types: vec![],
            dictionaries: vec![],
        };
        entry_source(
            &hirsel_cranelift::core::symbol(&instance),
            recipe,
            *exit_offset,
            parameters,
            cleanup
                .as_ref()
                .map(|m| hirsel_cranelift::core::symbol(m.instances.last().unwrap())),
            options,
        )
    }
}
// This is a driver product, captured while the checked declaration table is live.
// It is cached with the check, so native builds never resolve/type-check again.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EntryAbi {
    recipe: Recipe,
    exit_offset: Option<u64>,
    parameters: Vec<EntryParameter>,
    cleanup: Option<Module>,
}
#[derive(Clone, Serialize, Deserialize)]
struct EntryParameter {
    ty: String,
    name: String,
}
pub(crate) type EntryBinding = (DefPath, Result<EntryAbi, String>);
pub(crate) fn bind_entry(
    p: &Program<'_>,
    entry: &DefPath,
    product: Option<&DefinitionProduct>,
) -> EntryBinding {
    use hirsel_core_front::TyKind;
    let binding = (|| {
        let fd = p.function(entry).ok_or("entry declaration unavailable")?;
        let TyKind::Adt(result, args) = p.types.kind(fd.result) else {
            return Err("PROG-3 requires Result[(), E]");
        };
        let result = &p.decls[result.index()];
        if result.path != DefPath::from_full("core@1/core/Result").unwrap()
            || args.len() != 2
            || !matches!(p.types.kind(args[0]), TyKind::Unit)
        {
            return Err("PROG-3 requires Result[(), E]");
        }
        let product = product.ok_or("entry has no checked body product")?;
        let function = product
            .check
            .function
            .ok_or("entry body is not fully lowered")?;
        let module = &product.ir;
        let signature = &module.signatures[module.functions[function.index()].signature.index()];
        let dest = signature
            .dest
            .ok_or("entry result has no emitted destination")?;
        let recipes = hirsel_core_layout::compute(&module.types).expect("producer layouts");
        let recipe = recipes[dest.index()].clone();
        let exit_offset = match p.types.kind(args[1]) {
            TyKind::Adt(d, a)
                if a.is_empty()
                    && p.decls[d.index()].path
                        == DefPath::from_full("core@1/core/Exit").unwrap() =>
            {
                let hirsel_core_layout::Body::Variant(cases) = &module.types[dest.index()].body
                else {
                    unreachable!("checked Result has variant shape")
                };
                let hirsel_core_layout::FieldTy::Inline(exit) = cases[1].fields[0].ty else {
                    unreachable!("checked Exit is an inline result payload")
                };
                let Layout::Fixed(result) = &recipe.layout else {
                    unreachable!("entry result is fixed")
                };
                let Layout::Fixed(exit) = &recipes[exit.index()].layout else {
                    unreachable!("Exit is fixed")
                };
                Some(result.offsets[1][0] + exit.offsets[0][0])
            }
            _ => None,
        };
        let parameters = fd
            .params
            .iter()
            .map(|param| {
                let TyKind::Adt(d, _) = p.types.kind(param.ty) else {
                    return Err("entry parameter has no provider handle type");
                };
                Ok(EntryParameter {
                    ty: p.decls[d.index()].path.to_string(),
                    name: p.index.symbols.text(param.name).to_owned(),
                })
            })
            .collect::<Result<Vec<_>, &str>>()?;
        let cleanup = (!recipe.plain).then(|| result_cleanup(module, dest, entry));
        Ok(EntryAbi {
            recipe,
            exit_offset,
            parameters,
            cleanup,
        })
    })()
    .map_err(str::to_owned);
    (entry.clone(), binding)
}

/// Runtime CAP-4 kind numbers. Args is always supplied by the process constructor.
pub fn grant_kind(name: &str) -> Option<u8> {
    [
        "Dir", "Net", "Clock", "Random", "Stdout", "Stderr", "Stdin", "Env", "Process", "Tasks",
        "Args",
    ]
    .iter()
    .position(|kind| *kind == name)
    .map(|kind| kind as u8)
}

fn entry_source(
    symbol: &str,
    recipe: &Recipe,
    exit_offset: Option<u64>,
    parameters: &[EntryParameter],
    cleanup: Option<String>,
    options: &NativeOptions,
) -> Result<String, NativeStop> {
    let Layout::Fixed(layout) = &recipe.layout else {
        return Err(NativeStop::Entry("Result needs fixed layout".into()));
    };
    let tag = layout.tag.expect("checked Result is a variant");
    let case = c_case(&tag, "word");
    let error = if let Some(offset) = exit_offset {
        format!("out->kind=2; out->exit=result[{}];", offset)
    } else {
        "out->kind=1;".into()
    };
    let mut ordinals = [0usize; 11];
    let mut requirements = Vec::new();
    for parameter in parameters {
        let kind = parameter
            .ty
            .strip_prefix("core@1/core/")
            .and_then(grant_kind)
            .ok_or_else(|| {
                NativeStop::Entry(format!("unsupported provider parameter {}", parameter.ty))
            })?;
        requirements.push(format!("{{{kind},{}}}", ordinals[kind as usize]));
        ordinals[kind as usize] += 1;
    }
    let requirements_len = requirements.len();
    let requirements = if requirements.is_empty() {
        "{0,0}".into()
    } else {
        requirements.join(",")
    };
    let directories_len = options.directories.len();
    let directories = if options.directories.is_empty() {
        "{0,0}".into()
    } else {
        options
            .directories
            .iter()
            .map(|path| c_view(path))
            .collect::<Vec<_>>()
            .join(",")
    };
    let parameter_names = if parameters.is_empty() {
        "{0,0}".into()
    } else {
        parameters
            .iter()
            .map(|p| c_view(&p.name))
            .collect::<Vec<_>>()
            .join(",")
    };
    let parameter_types = ", uint64_t".repeat(parameters.len());
    let grants = (0..parameters.len())
        .map(|i| format!(",cfg->grants[{i}]"))
        .collect::<String>();
    let cleanup_declaration = cleanup
        .as_ref()
        .map(|name| format!("extern uint8_t {name}(HirselCoreContext *, void *);"))
        .unwrap_or_default();
    let cleanup_call = cleanup
        .as_ref()
        .map(|name| format!("if({name}(ctx,result)) return 1;"))
        .unwrap_or_default();
    let limit = options.root_limit;
    let grant_mask = options.grant_mask;
    let observation_label = if options.logical_charge_report.is_some() {
        r#"__attribute__((used)) static const char hirsel_observation_build[] = "Hirsel observation build: timings are not performance samples";"#
    } else {
        ""
    };
    let charge_report = options.logical_charge_report.as_ref().map_or_else(String::new, |path| {
        use std::os::unix::ffi::OsStrExt;
        let literal = path.as_os_str().as_bytes().iter().map(|b| format!("\\{b:03o}")).collect::<String>();
        format!(r#"FILE *report=fopen("{literal}","w");
    if(!report) {{ perror("logical charge report"); return 2; }}
    fprintf(report,"{{\"observation_mode\":true,\"peak_root_reserved_bytes\":%llu}}\n",(unsigned long long)hirsel_core_peak_charge());
    if(fclose(report)) {{ perror("logical charge report"); return 2; }}"#)
    });
    Ok(format!(
        r#"#include "hirsel_core_rt_host.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
{observation_label}
extern uint8_t {symbol}(HirselCoreContext *, void *{parameter_types});
{cleanup_declaration}
static uint8_t entry(HirselCoreContext *ctx, const HirselCoreProcessConfig *cfg, HirselCoreMainResult *out) {{
    (void)cfg;
    _Alignas({align}) uint8_t result[{size}];
    uint8_t failed={symbol}(ctx,result{grants});
    if(failed) return failed;
    uint64_t word=0; memcpy(&word,result+{at},{width});
    if(({case})==1) {{ {error} }}
    {cleanup_call}
    return 0;
}}
static const HirselCoreEntryRequirement parameters[] = {{{requirements}}};
static const HirselCoreView parameter_names[] = {{{parameter_names}}};
static const HirselCoreView directories[] = {{{directories}}};
int main(int argc, char **argv) {{
    size_t n=(size_t)(argc-1);
    HirselCoreView *args=calloc(n ? n : 1,sizeof(*args));
    if(!args) {{ fputs("run stop: HostMemory\n",stderr); return 102; }}
    for(size_t i=0;i<n;i++) {{ args[i].data=(const uint8_t *)argv[i+1]; args[i].len=strlen(argv[i+1]); }}
    HirselCoreOsConfig input={{0}};
    input.root_limit=UINT64_C({limit}); input.args=args; input.args_len=n;
    input.directories=directories; input.directories_len={directories_len};
    input.grant_mask=UINT64_C({grant_mask}); input.stdout_fd=1; input.stderr_fd=2;
    input.parameters=parameters; input.parameters_len={requirements_len};
    uint8_t status=hirsel_core_os_run(&input,parameter_names,entry);
    free(args);
    {charge_report}
    return status;
}}
"#,
        align = layout.align,
        size = layout.size.max(1),
        at = tag.offset,
        width = tag.width,
    ))
}
/// The C expression for the case a variant's tag word `word` holds (read_case).
fn c_case(tag: &hirsel_core_layout::Tag, word: &str) -> String {
    struct C(Vec<String>);
    impl hirsel_core_layout::CaseOps for C {
        type V = usize;
        fn constant(&mut self, n: u64) -> usize {
            self.push(format!("UINT64_C({n})"))
        }
        fn add(&mut self, a: usize, b: usize) -> usize {
            self.push(format!("({}+{})", self.0[a], self.0[b]))
        }
        fn sub(&mut self, a: usize, b: usize) -> usize {
            self.push(format!("({}-{})", self.0[a], self.0[b]))
        }
        fn and(&mut self, a: usize, b: usize) -> usize {
            self.push(format!("({}&{})", self.0[a], self.0[b]))
        }
        fn below(&mut self, a: usize, b: usize) -> usize {
            self.push(format!("(uint64_t)({}<{})", self.0[a], self.0[b]))
        }
        fn select(&mut self, c: usize, yes: usize, no: usize) -> usize {
            self.push(format!("({}?{}:{})", self.0[c], self.0[yes], self.0[no]))
        }
    }
    impl C {
        fn push(&mut self, e: String) -> usize {
            self.0.push(e);
            self.0.len() - 1
        }
    }
    let mut c = C(vec![word.to_string()]);
    let v = hirsel_core_layout::read_case(tag, &mut c, 0);
    c.0.swap_remove(v)
}
fn c_view(text: &str) -> String {
    // Fixed-width byte escapes preserve UTF-8 and never form C source tokens.
    let literal = text
        .as_bytes()
        .iter()
        .map(|b| format!("\\{b:03o}"))
        .collect::<String>();
    format!("{{(const uint8_t *)\"{literal}\",{}}}", text.len())
}
fn command(command: &mut Command) -> Result<Vec<u8>, NativeStop> {
    let result = command
        .output()
        .map_err(|e| NativeStop::Link(format!("launch {command:?}: {e}")))?;
    if !result.status.success() {
        return Err(NativeStop::Link(format!(
            "{command:?}: {}\n{}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    Ok(result.stdout)
}
fn link(
    source: &str,
    objects: &[ObjectProduct],
    options: &NativeOptions,
) -> Result<BuildProduct, NativeStop> {
    let runtime = read_input(&options.runtime)?;
    let headers = [
        "hirsel_core_rt.h",
        "hirsel_core_rt_host.h",
        "hirsel_core_rt_tasks.h",
    ]
    .map(|n| read_input(&options.include.join(n)))
    .into_iter()
    .collect::<io::Result<Vec<_>>>()?;
    let cc = command(Command::new(&options.cc).arg("--version"))?;
    let identities: Vec<_> = objects.iter().map(|o| o.identity).collect();
    let link_key = key(
        "hirsel.core-executable.v1",
        &(
            compiler_identity()?,
            source,
            &identities,
            &options.target,
            options.backend,
            options.optimize,
            ContentHash::of_parts(
                "hirsel.core-link-inputs.v1",
                headers
                    .iter()
                    .map(Vec::as_slice)
                    .chain([runtime.as_slice(), cc.as_slice()]),
            ),
        ),
    )?;
    let store = Store::new(&options.store)?;
    let (identity, bytes, linked) = if let Some((identity, bytes)) = cached(&store, link_key)? {
        (identity, bytes, false)
    } else {
        let temp = tempfile::tempdir_in(&options.store)?;
        let entry = temp.path().join("entry.c");
        std::fs::write(&entry, source)?;
        let output = temp.path().join("program");
        let mut cc = Command::new(&options.cc);
        cc.args([
            "-std=c11",
            "-fstack-clash-protection",
            "-no-pie",
            if options.optimize { "-O3" } else { "-O0" },
            "-I",
        ])
        .arg(&options.include)
        .arg(&entry);
        for (i, object) in objects.iter().enumerate() {
            let path = temp.path().join(format!("{i}.o"));
            std::fs::write(&path, &object.bytes)?;
            cc.arg(path);
        }
        cc.arg(&options.runtime)
            .args([
                "-lgcc_s",
                "-lpthread",
                "-ldl",
                "-lm",
                "-lrt",
                "-lutil",
                "-o",
            ])
            .arg(&output);
        command(&mut cc)?;
        let bytes = std::fs::read(output)?;
        (publish(&store, link_key, &bytes)?, bytes, true)
    };
    if let Some(parent) = options
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    // Copy from integrity-checked bytes even on a hit; a damaged materialized file
    // is never trusted as the executable cache product.
    let parent = options
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Close the original writable descriptor before publishing. Keeping the
    // NamedTempFile open across parallel child launches can leave a forked
    // child holding a writable descriptor until exec (ETXTBSY at our exec).
    let temp = tempfile::NamedTempFile::new_in(parent)?.into_temp_path();
    std::fs::write(&temp, &bytes)?;
    std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))?;
    temp.persist(&options.output).map_err(|e| e.error)?;
    Ok(BuildProduct {
        executable: options.output.clone(),
        identity,
        compiled: objects.iter().filter(|o| o.compiled).count(),
        linked,
        observation_mode: options.logical_charge_report.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_glue_has_one_object_definition() {
        fn unit(name: &str) -> Module {
            hirsel_core_ir::text::parse(&format!(
                r#"core v1
sig g0 = () -> (u64)
inst n0 = app@1/main/{name} [] []
inst n1 = core@1/map/$key_equal ["u64"] []
fn f0 = n0 g0 {{
b0:
 v0: u64 = call f1 g0 ()
 return v0
}}
fn f1 = n1 g0 {{
b0:
 v0: u64 = const 17
 return v0
}}
"#
            ))
            .unwrap()
        }
        let a = unit("a");
        let b = unit("b");
        let mut emitted = HashSet::new();
        assert!(matches!(define_once(&a, &mut emitted), Cow::Borrowed(_)));
        let selected = define_once(&b, &mut emitted);
        assert!(selected.functions[0].body.is_some());
        assert!(selected.functions[1].body.is_none());
        assert!(
            b.functions[1].body.is_some(),
            "immutable source product remains complete"
        );
    }
    fn source_request(source: &str, dir: &Path) -> CheckRequest {
        let mut request = crate::tests::request(source, dir);
        let core = dir.join("core.hirsel");
        std::fs::write(&core, "pub type Result[T, E] = Ok(T) | Error(E)\npub type Exit: key { status: u8 }\npub opaque type Args\npub opaque type Stdout\npub opaque type Dir\npub opaque type Net\npub opaque type Clock\npub opaque type Random\npub opaque type Stderr\npub opaque type Stdin\npub opaque type Env\npub opaque type Process\npub opaque type Tasks\n").unwrap();
        request.inputs.packages[0]
            .dependencies
            .push(package("core"));
        request.inputs.packages.push(PackageManifest {
            package: package("core"),
            sources: vec![hirsel_core_resolve::ModuleSource {
                file: core,
                modules: vec!["core".into()],
                signatures: true,
            }],
            dependencies: vec![],
        });
        request.entry = Some(DefPath::from_full("app@1/main/main").unwrap());
        request
    }
    include!("core_ops_native.rs");
    include!("list_native.rs");
    include!("numeric_native.rs");
    include!("instance_native.rs");
    include!("callable_native.rs");
    include!("finalizer_native.rs");
    include!("startup_native.rs");
    include!("test_rules_native.rs");
    include!("test_run_native.rs");
    include!("clone_native.rs");

    fn run_clean(binary: &Path, exit: i32) {
        let result = Command::new(binary).output().unwrap();
        assert_eq!(result.status.code(), Some(exit));
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
    }
    fn native_options(backend: Backend, dir: &Path) -> NativeOptions {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        NativeOptions {
            backend,
            optimize: false,
            target: TARGET.into(),
            runtime: root
                .join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").expect("native runtime archive")),
            include: root.join("crates/hirsel-core-rt/include"),
            cc: PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/clang"),
            store: dir.join("native"),
            output: dir.join("program"),
            root_limit: 1_048_576,
            instance_limit: 64,
            directories: vec![],
            grant_mask: 0,
            logical_charge_report: None,
        }
    }
    fn exercise(backend: Backend) {
        let dir = tempfile::tempdir().unwrap();
        let mut options = native_options(backend, dir.path());
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let literal = "pub fn main() -> Result[(), Exit] { Error(Exit(status: 7u8)) }\n";
        let mut driver = Driver::default();
        let product = driver
            .check(source_request(literal, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(
            product.accepted(),
            "{}",
            serde_json::to_string(&(
                product.machine_diagnostics().unwrap(),
                product.machine_incomplete().unwrap()
            ))
            .unwrap()
        );
        let built = product.build(&entry, &options).unwrap();
        assert_eq!(built.compiled, 1);
        assert!(built.linked);
        run_clean(&built.executable, 7);
        let cached = Driver::default()
            .check(source_request(literal, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(cached.rechecked.is_empty());
        let warm = cached.build(&entry, &options).unwrap();
        assert_eq!(warm.compiled, 0);
        assert!(!warm.linked);
        std::fs::write(&warm.executable, b"damaged executable").unwrap();
        let restored = cached.build(&entry, &options).unwrap();
        assert_eq!(restored.compiled, 0);
        assert!(!restored.linked);
        run_clean(&restored.executable, 7);

        let source = "pub fn main() -> Result[(), Exit] { Error(Exit(status: answer())) }\nfn answer() -> u8 { add(3u8, 4u8) }\nfn add(a: u8, b: u8) -> u8 { a + b }\nfn unused[T](take x: T) -> T { x }\ntype Broken = Missing\n";
        let product = driver
            .check(source_request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(
            !product.accepted(),
            "unreached body/declaration errors still refuse package check"
        );
        let built = product.build(&entry, &options).unwrap();
        assert_eq!(built.compiled, 3);
        run_clean(&built.executable, 7);
        let warm = product.build(&entry, &options).unwrap();
        assert_eq!(warm.compiled, 0);
        assert!(!warm.linked);
        let edited = source.replace("a + b", "a + b + 1u8");
        let product = driver
            .check(source_request(&edited, dir.path()), &mut Meter::new(false))
            .unwrap();
        let changed = product.build(&entry, &options).unwrap();
        assert_eq!(changed.compiled, 1);
        assert!(changed.linked);
        run_clean(&changed.executable, 8);
        let objects = product.emit_reachable(&entry, &options).unwrap();
        let callee = objects
            .iter()
            .find(|o| o.instance.definition.0.name == "add")
            .unwrap();
        let store = Store::new(&options.store).unwrap();
        std::fs::write(store.path(callee.identity), b"bad object").unwrap();
        let repaired = product.emit_reachable(&entry, &options).unwrap();
        assert_eq!(repaired.iter().filter(|o| o.compiled).count(), 1);

        let overflowing = source.replace("add(3u8, 4u8)", "add(255u8, 1u8)");
        let product = driver
            .check(
                source_request(&overflowing, dir.path()),
                &mut Meter::new(false),
            )
            .unwrap();
        let built = product.build(&entry, &options).unwrap();
        let result = Command::new(built.executable).output().unwrap();
        assert_eq!(result.status.code(), Some(101));
        assert!(result.stdout.is_empty());
        assert!(
            String::from_utf8(result.stderr)
                .unwrap()
                .starts_with("Overflow at ")
        );

        let source =
            "pub fn main(a: Args, out: Stdout) -> Result[(), Exit] { Error(Exit(status: 7u8)) }\n";
        let product = driver
            .check(source_request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        let refused = product.build(&entry, &options).unwrap();
        assert_not_started(
            &Command::new(refused.executable).output().unwrap(),
            "not started: grant `out` is not in the run's configuration\n",
        );
        options.grant_mask = 1 << grant_kind("Stdout").unwrap();
        let granted = product.build(&entry, &options).unwrap();
        run_clean(&granted.executable, 7);
        options.grant_mask |= 1 << grant_kind("Stderr").unwrap();
        let changed_grants = product.build(&entry, &options).unwrap();
        assert_eq!(changed_grants.compiled, 0);
        assert!(
            changed_grants.linked,
            "grant configuration is part of the executable key"
        );
        run_clean(&changed_grants.executable, 7);
        let source = "pub fn main(out: Stdout, d: Dir, a: Args, other: Dir, net: Net, clock: Clock, random: Random, err: Stderr, input: Stdin, env: Env, process: Process, tasks: Tasks) -> Result[(), Exit] { Error(Exit(status: 7u8)) }\n";
        let product = driver
            .check(source_request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        options.grant_mask = (1 << 10) - 2;
        let escaped = dir.path().join("directory \"é");
        std::fs::create_dir(&escaped).unwrap();
        options.directories = vec![
            dir.path().display().to_string(),
            escaped.display().to_string(),
        ];
        let built = product.build(&entry, &options).unwrap();
        run_clean(&built.executable, 7);
        // Two Dir parameters require two explicit directories, in ordinal order.
        options.directories.pop();
        let refused = product.build(&entry, &options).unwrap();
        assert_not_started(
            &Command::new(refused.executable).output().unwrap(),
            "not started: grant `other` is not in the run's configuration\n",
        );
        // A declared record crosses result and view-argument ABIs between units.
        // Its u64 field has padding before it, so reading it at byte 1 is wrong.
        options.optimize = matches!(backend, Backend::Llvm);
        let record = "type Pair { a: u8, b: u64 }\nfn make() -> Pair { Pair(a: 7u8, b: 300u64) }\nfn read(p: Pair) -> u8 { if p.b == 300u64 { p.a } else { 1u8 } }\npub fn main() -> Result[(), Exit] { let p = make(); Error(Exit(status: read(p))) }\n";
        let product = driver
            .check(source_request(record, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(
            product.accepted(),
            "{}",
            serde_json::to_string(&(
                product.machine_diagnostics().unwrap(),
                product.machine_incomplete().unwrap()
            ))
            .unwrap()
        );
        let built = product.build(&entry, &options).unwrap();
        // LLVM release emits one reached unit so its bodies can inline across calls.
        assert_eq!(built.compiled, if options.optimize { 1 } else { 3 });
        run_clean(&built.executable, 7);
        let edited = record.replace("b: 300u64", "b: 301u64");
        let product = driver
            .check(source_request(&edited, dir.path()), &mut Meter::new(false))
            .unwrap();
        let built = product.build(&entry, &options).unwrap();
        assert_eq!(built.compiled, 1);
        run_clean(&built.executable, 1);
        let source = "pub fn main() -> Result[(), Exit] { Ok(()) }\n";
        let product = driver
            .check(source_request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        let built = product.build(&entry, &options).unwrap();
        run_clean(&built.executable, 0);
    }
    fn capability_entry(backend: Backend, kind: &str, expected_kind: u8) {
        if expected_kind == 0 {
            crate::openat2_required::require();
        }
        let dir = tempfile::tempdir().unwrap();
        let mut options = native_options(backend, dir.path());
        let source = format!("pub fn main(cap: {kind}) -> Result[(), Exit] {{ Ok(()) }}\n");
        let product = Driver::default()
            .check(source_request(&source, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(
            product.accepted(),
            "{kind}: {}",
            serde_json::to_string(&(
                product.machine_diagnostics().unwrap(),
                product.machine_incomplete().unwrap()
            ))
            .unwrap()
        );
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        if expected_kind != 10 {
            let refused = product.build(&entry, &options).unwrap();
            assert_not_started(
                &Command::new(refused.executable).output().unwrap(),
                "not started: grant `cap` is not in the run's configuration\n",
            );
        }
        // Numbers come from hirsel_core_rt_host.h, independently of grant_kind.
        match expected_kind {
            10 => {}
            0 => options.directories.push(dir.path().display().to_string()),
            _ => options.grant_mask = 1 << expected_kind,
        }
        let built = product.build(&entry, &options).unwrap();
        run_clean(&built.executable, 0);
    }
    macro_rules! capability_entries {
        ($(($kind:ident, $expected_kind:literal, $cranelift:ident, $llvm:ident)),+ $(,)?) => {$(
            #[test]
            #[ignore = "explicit native integration target"]
            fn $cranelift() { capability_entry(Backend::Cranelift, stringify!($kind), $expected_kind); }
            #[test]
            #[ignore = "explicit native integration target"]
            fn $llvm() { capability_entry(Backend::Llvm, stringify!($kind), $expected_kind); }
        )+};
    }
    capability_entries!(
        (Args, 10, args_cranelift_entry, args_llvm_entry),
        (Dir, 0, dir_cranelift_entry, dir_llvm_entry),
        (Net, 1, net_cranelift_entry, net_llvm_entry),
        (Clock, 2, clock_cranelift_entry, clock_llvm_entry),
        (Random, 3, random_cranelift_entry, random_llvm_entry),
        (Stdout, 4, stdout_cranelift_entry, stdout_llvm_entry),
        (Stderr, 5, stderr_cranelift_entry, stderr_llvm_entry),
        (Stdin, 6, stdin_cranelift_entry, stdin_llvm_entry),
        (Env, 7, env_cranelift_entry, env_llvm_entry),
        (Process, 8, process_cranelift_entry, process_llvm_entry),
        (Tasks, 9, tasks_cranelift_entry, tasks_llvm_entry),
    );
    fn product_entry(backend: Backend, optimize: bool) {
        let dir = tempfile::tempdir().unwrap();
        let mut options = native_options(backend, dir.path());
        options.optimize = optimize;
        let source =
            "pub fn main() -> Result[(), Exit] { Error(Exit(status: identity(answer()))) }\n";
        let mut request = source_request(source, dir.path());
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("core.hirsel"))
            .unwrap()
            .write_all(b"pub fn answer() -> u8 { 7u8 }\npub fn identity[T](take x: T) -> T { x }\npub fn unused_bad() -> u64 { true }\n")
            .unwrap();
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let checked = Driver::default()
            .check(request.clone(), &mut Meter::new(false))
            .unwrap();
        let native = checked.build(&entry, &options).unwrap();
        run_clean(&native.executable, 7);
        request.store = dir.path().join("image-request");
        let cache = dir.path().join("products");
        let core = request
            .inputs
            .packages
            .iter()
            .find(|p| p.package == package("core"))
            .unwrap()
            .clone();
        drop(
            PackageProduct::sources(
                CorpusManifest {
                    core: package("core"),
                    packages: vec![core],
                },
                &cache,
                &mut Meter::new(false),
            )
            .unwrap(),
        );
        let installed = Driver::default()
            .check_reachable_with_product(
                request,
                &dir.path().join("products"),
                &mut Meter::new(false),
            )
            .unwrap();
        let native = installed.build(&entry, &options).unwrap();
        run_clean(&native.executable, 7);
        assert_eq!(installed.rechecked.len(), 1);
        assert_eq!(installed.image.as_ref().unwrap().decoded_bodies(), 2);
        // A second application's private native store still reuses core's objects.
        let mut other_options = native_options(backend, dir.path());
        other_options.optimize = optimize;
        other_options.store = dir.path().join("another-app-native");
        let objects = installed.emit_reachable(&entry, &other_options).unwrap();
        let core = objects
            .iter()
            .filter(|o| o.instance.definition.package_is("core", "1"))
            .collect::<Vec<_>>();
        assert_eq!(core.len(), 2);
        assert!(core.iter().all(|o| !o.compiled));
        if optimize {
            let answer = core
                .iter()
                .find(|o| o.instance.definition.to_string().ends_with("/answer"))
                .unwrap();
            let name = format!("h{}", selection(answer.instance.digest()));
            let own = objects
                .iter()
                .find(|o| o.instance.definition == entry)
                .unwrap();
            assert!(
                !own.bytes.windows(name.len()).any(|w| w == name.as_bytes()),
                "the program must inline the published answer body, not reference its object"
            );
        }
        assert_eq!(objects.iter().filter(|o| o.compiled).count(), 1);
        // A cached negative body is refused only when this entry reaches it.
        let mut request = source_request(
            "pub fn main()->Result[(),Exit] { let _=unused_bad(); Ok(()) }",
            dir.path(),
        );
        // source_request refreshes its small core fixture; restore the same package.
        std::fs::OpenOptions::new().append(true).open(dir.path().join("core.hirsel")).unwrap().write_all(b"pub fn answer() -> u8 { 7u8 }\npub fn identity[T](take x: T) -> T { x }\npub fn unused_bad() -> u64 { true }\n").unwrap();
        request.store = dir.path().join("reached-negative");
        let refused = Driver::default()
            .check_reachable_with_product(request, &cache, &mut Meter::new(false))
            .unwrap();
        assert!(
            matches!(refused.build(&entry,&options),Err(NativeStop::Check(message)) if message.contains("unused_bad"))
        );
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn published_core_entry_cranelift() {
        product_entry(Backend::Cranelift, false);
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn published_core_entry_llvm() {
        product_entry(Backend::Llvm, false);
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn release_inlines_published_core_and_reuses_its_objects() {
        product_entry(Backend::Llvm, true);
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn prog3_prog6_cranelift_source_entries_link_run_and_reuse() {
        exercise(Backend::Cranelift)
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn prog3_prog6_llvm_source_entries_link_run_and_reuse() {
        exercise(Backend::Llvm)
    }
}
