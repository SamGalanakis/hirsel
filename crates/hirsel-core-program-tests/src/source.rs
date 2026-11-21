//! Source fixtures retain the same reached units for native composition and evaluation.
use crate::{Checked, package};
use hirsel_core_ids::DefPath;
use hirsel_core_ir::{FuncId, Identity, Module};
use hirsel_core_layout::Target;
use std::{collections::HashMap, fmt::Write as _, ops::Deref, process::Command};

pub struct SourceProgram {
    pub modules: Vec<Module>,
    joined: Module,
    /// By joined item, its first site word, bound to the checked sources as a
    /// native link binds them (`SourceMap::item_sites`); 0 for an item no
    /// failure anchor names (`source::anchored_items`).
    pub sites: Vec<u64>,
    _checked: Checked,
}
impl Deref for SourceProgram {
    type Target = Module;
    fn deref(&self) -> &Module {
        &self.joined
    }
}
impl SourceProgram {
    pub fn lower(source: &str, entry: Option<&str>) -> (Self, HashMap<String, FuncId>) {
        Self::lower_with_core(source, entry, &crate::repo().join("core/core"))
    }
    pub fn lower_with_core(
        source: &str,
        entry: Option<&str>,
        core: &std::path::Path,
    ) -> (Self, HashMap<String, FuncId>) {
        let entry = entry.map(|name| DefPath::from_full(&format!("app@1/main/{name}")).unwrap());
        let checked = Checked::source_with_core(source, entry, core);
        let product = &checked.product;
        let mut expected = source
            .lines()
            .filter_map(|l| l.trim().strip_prefix("// expect: refuse "))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut actual = product
            .diagnostics
            .iter()
            .filter(|r| product.index.module_path(r.primary.module).package == package("app"))
            .map(|r| {
                let path = product.index.module_path(r.primary.module);
                let file = checked
                    .corpus
                    .sources
                    .iter()
                    .find(|s| s.module == path)
                    .unwrap();
                let parsed = checked.corpus.parse();
                let at = checked
                    .corpus
                    .sources
                    .iter()
                    .position(|s| s.module == path)
                    .unwrap();
                let syntax = &parsed.files[at].syntax;
                let offset =
                    syntax.items[r.primary.item as usize].file_offset + r.primary.span.start;
                let before = &file.text[..offset as usize];
                format!(
                    "{} at {}:{}",
                    r.rule,
                    before.matches('\n').count() + 1,
                    before.rsplit('\n').next().unwrap().chars().count() + 1
                )
            })
            .collect::<Vec<_>>();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected, "source refusal rules and sites");
        let app_gaps = product
            .incomplete
            .iter()
            .filter(|r| product.index.module_path(r.primary.module).package == package("app"))
            .collect::<Vec<_>>();
        assert!(app_gaps.is_empty(), "{app_gaps:?}");
        assert!(product.resources.is_empty(), "{:?}", product.resources);
        let roots = product
            .definitions
            .values()
            .filter(|d| {
                d.definition.package_is("app", "1")
                    && d.check.function.is_some()
                    && d.check.refusals.is_empty()
                    && d.incomplete.is_empty()
                    && d.check
                        .template
                        .as_ref()
                        .is_none_or(|t| t.parameters.is_empty())
            })
            .map(|d| Identity::plain(d.definition.clone()))
            .collect::<Vec<_>>();
        Self::checked(checked, roots)
    }
    #[cfg(test)]
    pub(crate) fn from_inputs(
        index: &hirsel_core_resolve::NameIndex,
        modules: &[hirsel_core_front::ModuleInput],
        wanted: impl Fn(&DefPath) -> bool,
    ) -> (Self, HashMap<String, FuncId>) {
        let checked = crate::checked_inputs(index, modules);
        let product = &checked.product;
        assert!(product.diagnostics.is_empty(), "{:?}", product.diagnostics);
        assert!(product.resources.is_empty(), "{:?}", product.resources);
        let roots = product
            .definitions
            .values()
            .filter(|d| {
                wanted(&d.definition)
                    && d.check.function.is_some()
                    && d.check
                        .template
                        .as_ref()
                        .is_none_or(|t| t.parameters.is_empty())
            })
            .map(|d| {
                assert!(
                    d.incomplete.is_empty(),
                    "{}: {:?}",
                    d.definition,
                    d.incomplete
                );
                Identity::plain(d.definition.clone())
            })
            .collect::<Vec<_>>();
        Self::checked(checked, roots)
    }
    fn checked(checked: Checked, roots: Vec<Identity>) -> (Self, HashMap<String, FuncId>) {
        let units = checked.product.units(roots.clone(), 65_536).unwrap();
        let reached = units.modules().unwrap();
        let joined = hirsel_core_materialize::join(
            &reached,
            roots.first().expect("source fixture has a concrete root"),
        )
        .unwrap()
        .0;
        hirsel_core_ir::wf::module(&joined).unwrap();
        let names = joined
            .functions
            .iter()
            .enumerate()
            .filter_map(|(i, f)| {
                let key = &joined.instances[f.instance.index()];
                (roots.iter().any(|r| r.definition == key.definition)
                    && key.arguments.is_empty()
                    && f.body.is_some())
                .then(|| (key.definition.0.name.clone(), FuncId(i as u32)))
            })
            .collect();
        let modules = reached.iter().map(|u| u.module.clone()).collect();
        (
            Self {
                modules,
                sites: sites(&checked, &joined),
                joined,
                _checked: checked,
            },
            names,
        )
    }
    pub fn cranelift_object(
        &self,
        target: &str,
        speed: bool,
        debug: bool,
        observe: bool,
    ) -> Vec<u8> {
        let objects = self
            .modules
            .iter()
            .enumerate()
            .map(|(i, m)| {
                hirsel_cranelift::core::emit_object_for_test_observed(
                    m,
                    &hirsel_core_layout::compute(&m.types).unwrap(),
                    &hirsel_cranelift::core::Options {
                        target,
                        optimize: speed,
                        debug_lines: debug,
                    },
                    hirsel_core_ids::ContentHash::of_bytes(&i.to_le_bytes()),
                    observe,
                )
                .unwrap()
                .bytes
            })
            .collect::<Vec<_>>();
        combine(&objects, Target::from_triple(target).unwrap())
    }
    pub fn llvm_texts(&self, target: Target, observe: bool, debug: bool) -> Vec<String> {
        self.modules
            .iter()
            .map(|module| {
                hirsel_llvm::core::emit_ir_for_test(module, target, observe, debug, None).unwrap()
            })
            .collect()
    }
    pub fn site_bindings(&self) -> String {
        let mut bindings = String::new();
        for (item, &site) in self.items.iter().zip(&self.sites) {
            if site == 0 {
                continue;
            }
            writeln!(
                bindings,
                "__asm__(\".globl {0}\\n.hidden {0}\\n.set {0}, {site}\\n\");",
                hirsel_core_ir::source::site_symbol(item),
            )
            .unwrap();
        }
        bindings
    }
    pub fn llvm_eager_object(
        &self,
        target: Target,
        optimization: hirsel_llvm::core::Optimization,
        debug: bool,
    ) -> Vec<u8> {
        let objects = self
            .modules
            .iter()
            .map(|m| {
                hirsel_llvm::core::emit_object_for_test(m, target, optimization, false, debug)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        combine(&objects, target)
    }

    pub fn llvm_object(
        &self,
        target: Target,
        speed: bool,
        debug: bool,
        composed: bool,
        observe: bool,
    ) -> Vec<u8> {
        self.llvm_object_exported(target, speed, debug, composed, observe, None)
    }
    pub fn llvm_program(
        &self,
        target: Target,
        debug: bool,
        observe: bool,
        exports: &[FuncId],
    ) -> Vec<u8> {
        self.llvm_object_exported(target, true, debug, true, observe, Some(exports))
    }
    pub fn llvm_program_with_counter_range_folds(
        &self,
        target: Target,
        exports: &[FuncId],
    ) -> (Vec<u8>, hirsel_llvm_lazy::CounterRangeFolds) {
        self.llvm_composed(target, false, false, Some(exports), |units, exports| {
            hirsel_llvm_lazy::emit_program_with_counter_range_folds_for_test(units, exports, target)
        })
        .unwrap()
    }
    fn llvm_object_exported(
        &self,
        target: Target,
        speed: bool,
        debug: bool,
        composed: bool,
        observe: bool,
        exports: Option<&[FuncId]>,
    ) -> Vec<u8> {
        if composed {
            self.llvm_composed(target, debug, observe, exports, |units, exports| {
                hirsel_llvm_lazy::emit_program_for_target(units, exports, target)
            })
            .unwrap()
        } else {
            let objects = self
                .llvm_texts(target, observe, debug)
                .iter()
                .map(|s| hirsel_llvm_lazy::compile_ir_for_target(s, speed, target).unwrap())
                .collect::<Vec<_>>();
            combine(&objects, target)
        }
    }

    fn llvm_composed<T>(
        &self,
        target: Target,
        debug: bool,
        observe: bool,
        exports: Option<&[FuncId]>,
        emit: impl FnOnce(&[hirsel_llvm_lazy::Unit<'_>], &[String]) -> T,
    ) -> T {
        let refs = self.modules.iter().collect::<Vec<_>>();
        let facts = hirsel_core_ir::roles::Facts::program(&refs, &[]);
        let texts = self
            .modules
            .iter()
            .zip(&facts)
            .map(|(module, facts)| {
                hirsel_llvm::core::emit_ir_for_test(module, target, observe, debug, Some(facts))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let units = texts
            .iter()
            .map(|s| hirsel_llvm_lazy::Unit::Text(s))
            .collect::<Vec<_>>();
        let exports = exports.map_or_else(
            || {
                self.joined
                    .functions
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| f.body.is_some())
                    .map(|(i, _)| FuncId(i as u32))
                    .collect()
            },
            <[FuncId]>::to_vec,
        );
        let exports = exports
            .into_iter()
            .map(|f| hirsel_llvm::core::symbol(&self.joined, f))
            .collect::<Vec<_>>();
        emit(&units, &exports)
    }
}
fn combine(objects: &[Vec<u8>], target: Target) -> Vec<u8> {
    let scratch = tempfile::tempdir().unwrap();
    let compiler = match target {
        Target::X86_64Linux => Some(
            std::path::PathBuf::from(
                std::env::var_os("LLVM_SYS_231_PREFIX").expect("pinned LLVM prefix"),
            )
            .join("bin/clang")
            .into_os_string(),
        ),
        Target::Aarch64Linux => std::env::var_os("HIRSEL_AARCH64_CC"),
    }
    .expect("declared native compiler");
    let mut link = Command::new(compiler);
    link.args(["-r", "-nostdlib"]);
    for (i, bytes) in objects.iter().enumerate() {
        let path = scratch.path().join(format!("unit-{i}.o"));
        std::fs::write(&path, bytes).unwrap();
        link.arg(path);
    }
    let output = scratch.path().join("program.o");
    let result = link.arg("-o").arg(&output).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    std::fs::read(output).unwrap()
}

fn sites(checked: &Checked, joined: &Module) -> Vec<u64> {
    let anchored = hirsel_core_ir::source::anchored_items(joined);
    checked
        .product
        .native_sources(&anchored)
        .unwrap()
        .item_sites(&joined.items)
        .unwrap()
}
