//! Native products compose the checked body and the two engines directly.
use super::*;
use hirsel_core_ir::InstanceKey;
use hirsel_core_layout::{Layout, Recipe};
use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

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
impl CheckProduct {
    /// Each checked body is already an emitter module, with signature-only imports.
    /// A callee implementation edit therefore recompiles its object alone.
    pub fn emit_reachable(
        &self,
        entry: &DefPath,
        options: &NativeOptions,
    ) -> Result<Vec<ObjectProduct>, NativeStop> {
        let reached = self.reachable(entry, options.instance_limit)?;
        self.emit_instances(reached.require()?, options)
    }
    fn emit_instances(
        &self,
        reached: &[InstanceKey],
        options: &NativeOptions,
    ) -> Result<Vec<ObjectProduct>, NativeStop> {
        if options.target != TARGET {
            return Err(NativeStop::Entry(format!(
                "native engines currently target {TARGET}"
            )));
        }
        let runtime = ContentHash::of_parts(
            "hirsel.core-runtime.v1",
            [&std::fs::read(&options.runtime)?[..]],
        );
        let store = Store::new(&options.store)?;
        let mut objects = Vec::new();
        for instance in reached {
            let body = self.demand_body(&instance.definition)?;
            let object_key = key(
                "hirsel.core-object.v1",
                &(
                    compiler_identity()?,
                    &options.target,
                    options.backend,
                    options.optimize,
                    runtime,
                    &instance,
                    &body.key,
                    &body.ir,
                ),
            )?;
            let (identity, bytes, compiled) = if let Some((hash, bytes)) =
                cached(&store, object_key)?
            {
                (hash, bytes, false)
            } else {
                // The front currently publishes scalar bodies only. Memory emission
                // needs its indexed recipes, not a second layout computation here.
                if !body.ir.types.is_empty() {
                    return Err(NativeStop::Emit {
                        definition: instance.definition.clone(),
                        message: "checker has not published emitter-indexed layout recipes".into(),
                    });
                }
                let bytes = match options.backend {
                    Backend::Cranelift => hirsel_cranelift::core::emit_object(
                        &body.ir,
                        &hirsel_cranelift::core::Options {
                            target: &options.target,
                            optimize: options.optimize,
                        },
                    )
                    .map(|o| o.bytes)
                    .map_err(|e| e.to_string()),
                    Backend::Llvm => hirsel_llvm::core::emit_object(
                        &body.ir,
                        if options.optimize {
                            hirsel_llvm::core::Optimization::Speed
                        } else {
                            hirsel_llvm::core::Optimization::None
                        },
                    )
                    .map_err(|e| e.to_string()),
                }
                .map_err(|message| NativeStop::Emit {
                    definition: instance.definition.clone(),
                    message,
                })?;
                (publish(&store, object_key, &bytes)?, bytes, true)
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
        let source = self.process_entry(entry, options.root_limit)?;
        let objects = self.emit_instances(instances, options)?;
        link(&source, &objects, options)
    }
    fn process_entry(&self, entry: &DefPath, limit: u64) -> Result<String, NativeStop> {
        let Some((path, binding)) = &self.native_entry else {
            return Err(NativeStop::Entry("no checked entry selected".into()));
        };
        if path != entry {
            return Err(NativeStop::Entry(
                "build entry differs from checked entry".into(),
            ));
        }
        let EntryAbi {
            recipe,
            exit_offset,
            parameters,
        } = binding.as_ref().map_err(|e| NativeStop::Entry(e.clone()))?;
        let instance = InstanceKey {
            definition: entry.clone(),
            types: vec![],
            dictionaries: vec![],
        };
        if !parameters.is_empty() {
            return Err(NativeStop::Entry(
                "provider grant creation and close C exports are not available yet".into(),
            ));
        }
        entry_source(
            &hirsel_cranelift::core::symbol(&instance),
            recipe,
            *exit_offset,
            parameters.len(),
            limit,
        )
    }
}
// This is a driver product, captured while the checked declaration table is live.
// It is cached with the check, so native builds never resolve/type-check again.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EntryAbi {
    recipe: Recipe,
    exit_offset: Option<u64>,
    parameters: Vec<String>,
}
pub(crate) type EntryBinding = (DefPath, Result<EntryAbi, String>);
pub(crate) fn bind_entry(p: &Program<'_>, entry: &DefPath) -> EntryBinding {
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
        let recipe = result
            .summary
            .recipe
            .clone()
            .ok_or("checker has not published the instantiated Result destination/layout")?;
        let exit_offset = match p.types.kind(args[1]) {
            TyKind::Adt(d, a)
                if a.is_empty()
                    && p.decls[d.index()].path
                        == DefPath::from_full("core@1/core/Exit").unwrap() =>
            {
                let Some(Recipe {
                    layout: Layout::Fixed(layout),
                    ..
                }) = &p.decls[d.index()].summary.recipe
                else {
                    return Err("checker has not published Exit layout");
                };
                Some(layout.offsets[0][0])
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
                Ok(p.decls[d.index()].path.to_string())
            })
            .collect::<Result<Vec<_>, &str>>()?;
        Ok(EntryAbi {
            recipe,
            exit_offset,
            parameters,
        })
    })()
    .map_err(str::to_owned);
    (entry.clone(), binding)
}

fn entry_source(
    symbol: &str,
    recipe: &Recipe,
    exit_offset: Option<u64>,
    parameters: usize,
    limit: u64,
) -> Result<String, NativeStop> {
    let Layout::Fixed(layout) = &recipe.layout else {
        return Err(NativeStop::Entry("Result needs fixed layout".into()));
    };
    if !recipe.plain {
        return Err(NativeStop::Entry(
            "owned entry error cleanup needs emitted drop glue".into(),
        ));
    }
    let tag = layout.tag.expect("checked Result is a variant");
    let error = if let Some(offset) = exit_offset {
        format!(
            "out->kind=2; out->exit=result[{}];",
            layout.offsets[1][0] + offset
        )
    } else {
        "out->kind=1;".into()
    };
    let parameter_types = ", uint64_t".repeat(parameters);
    let grants = (0..parameters)
        .map(|i| format!(",cfg->grants[{i}]"))
        .collect::<String>();
    Ok(format!(
        r#"#include "hirsel_core_process.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
extern uint8_t {symbol}(HirselCoreContext *, void *{parameter_types});
static uint8_t entry(HirselCoreContext *ctx, const HirselCoreProcessConfig *cfg, HirselCoreMainResult *out) {{
    (void)cfg;
    _Alignas({align}) uint8_t result[{size}];
    uint8_t failed={symbol}(ctx,result{grants});
    if(failed) return failed;
    uint32_t tag=0; memcpy(&tag,result+{at},{width});
    if(tag==1) {{ {error} }}
    return 0;
}}
static void diagnostic(void *state, const uint8_t *p, size_t n) {{ (void)state; fwrite(p,1,n,stderr); }}
int main(int argc, char **argv) {{
    size_t n=(size_t)(argc-1);
    HirselCoreView *args=calloc(n ? n : 1,sizeof(*args));
    if(!args) {{ fputs("host argument allocation failed\n",stderr); return 102; }}
    for(size_t i=0;i<n;i++) {{ args[i].data=(const uint8_t *)argv[i+1]; args[i].len=strlen(argv[i+1]); }}
    HirselCoreProcessConfig cfg={{0}}; cfg.root_limit=UINT64_C({limit}); cfg.args=args; cfg.args_len=n; cfg.diagnostic=diagnostic;
    uint8_t status=hirsel_core_run(&cfg,entry); free(args); return status;
}}
"#,
        align = layout.align,
        size = layout.size.max(1),
        at = tag.offset,
        width = tag.width
    ))
}
fn command(command: &mut Command) -> Result<Vec<u8>, NativeStop> {
    let result = command.output()?;
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
    let runtime = std::fs::read(&options.runtime)?;
    let headers = ["hirsel_core_process.h", "hirsel_core_rt.h"]
        .map(|n| std::fs::read(options.include.join(n)))
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
    let temp = tempfile::NamedTempFile::new_in(parent)?;
    std::fs::write(temp.path(), &bytes)?;
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755))?;
    temp.persist(&options.output).map_err(|e| e.error)?;
    Ok(BuildProduct {
        executable: options.output.clone(),
        identity,
        compiled: objects.iter().filter(|o| o.compiled).count(),
        linked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_layout::{Fixed, Tag};
    fn exercise(backend: Backend) {
        let dir = tempfile::tempdir().unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let options = NativeOptions {
            backend,
            optimize: false,
            target: TARGET.into(),
            runtime: root.join(
                std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").expect("native target runtime archive"),
            ),
            include: root.join("crates/hirsel-core-rt/include"),
            cc: PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/clang"),
            store: dir.path().join("native"),
            output: dir.path().join("program"),
            root_limit: 1_048_576,
            instance_limit: 64,
        };
        let source = "fn answer() -> u8 { add(3u8, 4u8) }\nfn add(a: u8, b: u8) -> u8 { a + b }\nfn unused[T](take x: T) -> T { x }\ntype Broken = Missing\n";
        let entry = DefPath::from_full("app@1/main/answer").unwrap();
        let mut driver = Driver::default();
        let product = driver
            .check(
                crate::tests::request(source, dir.path()),
                &mut Meter::new(false),
            )
            .unwrap();
        assert!(
            !product.accepted(),
            "unreached library errors keep package check red"
        );
        assert!(product.reachable(&entry, 64).unwrap().accepted());
        let objects = product.emit_reachable(&entry, &options).unwrap();
        assert_eq!(objects.iter().filter(|o| o.compiled).count(), 2);
        let warm = product.emit_reachable(&entry, &options).unwrap();
        assert!(warm.iter().all(|o| !o.compiled));
        // A hand-written C bridge supplies the Result carrier the checker cannot
        // emit yet. It is a native fixture only, never a product entry alternative.
        let recipe = Recipe {
            layout: Layout::Fixed(Fixed {
                size: 2,
                align: 1,
                tag: Some(Tag {
                    offset: 0,
                    width: 1,
                }),
                offsets: vec![vec![], vec![1]],
            }),
            logical_size: Some(9),
            plain: true,
            drop: None,
        };
        let symbol = hirsel_cranelift::core::symbol(&InstanceKey {
            definition: entry.clone(),
            types: vec![],
            dictionaries: vec![],
        });
        let mut c = entry_source("bridge", &recipe, Some(0), 2, options.root_limit).unwrap();
        c.push_str(&format!("\ntypedef struct {{ uint8_t value, failed; }} R;\nextern R {symbol}(void *);\nuint8_t bridge(HirselCoreContext *ctx, void *dest, uint64_t a, uint64_t b) {{ R r={symbol}(ctx); ((uint8_t *)dest)[0]=1; ((uint8_t *)dest)[1]=r.value+(a!=21 || b!=34); return r.failed; }}\n"));
        // Provider handles are plain u64 operands in declaration order, FORM #44.
        c = c.replace(
            "cfg.root_limit=",
            "uint64_t grants[]={21,34}; cfg.grants=grants; cfg.grants_len=2; cfg.root_limit=",
        );
        let built = link(&c, &objects, &options).unwrap();
        assert!(built.linked);
        let result = Command::new(&built.executable).output().unwrap();
        assert_eq!(result.status.code(), Some(7));
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
        std::fs::write(&built.executable, b"damaged executable").unwrap();
        let rebuilt = link(&c, &warm, &options).unwrap();
        assert!(!rebuilt.linked);
        assert_eq!(
            Command::new(&rebuilt.executable).status().unwrap().code(),
            Some(7)
        );
        let edited = source.replace("a + b", "a - b");
        let product = driver
            .check(
                crate::tests::request(&edited, dir.path()),
                &mut Meter::new(false),
            )
            .unwrap();
        let changed = product.emit_reachable(&entry, &options).unwrap();
        assert_eq!(changed.iter().filter(|o| o.compiled).count(), 1);
        // Corrupt a stored object. Envelope length/digest rejection must cause
        // precisely that object to compile, rather than link the bad bytes.
        let store = Store::new(&options.store).unwrap();
        let object = &changed[0];
        std::fs::write(store.path(object.identity), b"bad object").unwrap();
        let repaired = product.emit_reachable(&entry, &options).unwrap();
        assert_eq!(repaired.iter().filter(|o| o.compiled).count(), 1);
        let overflowing = source.replace("add(3u8, 4u8)", "add(255u8, 1u8)");
        let product = driver
            .check(
                crate::tests::request(&overflowing, dir.path()),
                &mut Meter::new(false),
            )
            .unwrap();
        let failing = product.emit_reachable(&entry, &options).unwrap();
        let built = link(&c, &failing, &options).unwrap();
        let result = Command::new(built.executable).output().unwrap();
        assert_eq!(result.status.code(), Some(101));
        assert!(result.stdout.is_empty());
        assert!(
            String::from_utf8(result.stderr)
                .unwrap()
                .starts_with("Overflow at ")
        );
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn cranelift_objects_link_run_and_reuse() {
        exercise(Backend::Cranelift)
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn llvm_objects_link_run_and_reuse() {
        exercise(Backend::Llvm)
    }
}
