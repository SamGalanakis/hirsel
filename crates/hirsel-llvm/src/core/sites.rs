//! Item-relative causes and the caller location inherited through core/generated code.
use super::*;

impl<S: sink::Sink> FunctionEmitter<'_, S> {
    pub(super) fn call_site_word(&mut self, callee: Callee) -> String {
        if let Callee::Direct(f) = callee {
            let function = &self.module.functions[f.index()];
            if !hirsel_core_ir::source::inherits_site(
                &self.module.instances[function.instance.index()],
            ) {
                return "poison".into();
            }
        }
        self.site_word()
    }

    pub(super) fn site_word(&mut self) -> String {
        let Some(word) = self.fixed_site_word() else {
            return if self.function.is_none_or(|f| {
                let function = &self.module.functions[f.index()];
                hirsel_core_ir::source::inherits_site(
                    &self.module.instances[function.instance.index()],
                )
            }) {
                self.caller_site.into()
            } else {
                "0".into()
            };
        };
        if self.relocatable_sites && self.abi.target == Target::Aarch64Linux {
            // AArch64 selects a GOT reference for an external pointer even
            // with !absolute_symbol. Code addresses only this object's pool.
            // externally_initialized keeps O3 from folding back to the GOT.
            let anchor = self.anchor.unwrap();
            let symbol =
                hirsel_core_ir::source::site_symbol(&self.module.items[anchor.item.index()]);
            let pool = format!("{symbol}.{}.word", anchor.start);
            self.intrinsics.insert(format!(
                "@{pool} = private externally_initialized constant i64 {word}, align 8"
            ));
            self.calc(format!("load i64, ptr @{pool}, align 8"))
        } else {
            word
        }
    }

    pub(super) fn fixed_site_word(&mut self) -> Option<String> {
        let anchor = self
            .anchor
            .filter(|a| !self.module.items[a.item.index()].package_is("core", "1"))?;
        if !self.relocatable_sites {
            return Some(
                (((u64::from(anchor.item.0) + 1) << 32) | u64::from(anchor.start)).to_string(),
            );
        }
        let symbol = hirsel_core_ir::source::site_symbol(&self.module.items[anchor.item.index()]);
        Some(match self.abi.target {
            Target::X86_64Linux => {
                // A nonzero IR addend lets X86ISel fold this into a PC-relative
                // LEA, even with !absolute_symbol. Resolve the addend in asm;
                // this temporary name disappears and ELF references only the
                // canonical symbol with its original addend.
                let alias = format!(".L{symbol}_{}", anchor.start);
                self.intrinsics.insert(format!(
                    "module asm \".set {alias}, {symbol}+{}\"",
                    anchor.start
                ));
                self.intrinsics.insert(format!(
                    "@{alias} = external constant i8, !absolute_symbol !{{i64 -1, i64 -1}}"
                ));
                format!("ptrtoint (ptr @{alias} to i64)")
            }
            Target::Aarch64Linux => {
                self.intrinsics.insert(format!(
                    "@{symbol} = external constant i8, !absolute_symbol !{{i64 -1, i64 -1}}"
                ));
                format!(
                    "add (i64 ptrtoint (ptr @{symbol} to i64), i64 {})",
                    anchor.start
                )
            }
        })
    }
}

pub(super) struct DebugFunction {
    pub scope: usize,
    pub wrapper: Option<(usize, usize)>,
    fallback: usize,
    locations: std::collections::BTreeMap<(u32, u32), usize>,
}
impl DebugFunction {
    pub fn location(&self, anchor: Option<Anchor>) -> usize {
        anchor
            .and_then(|a| self.locations.get(&(a.item.0, a.start)).copied())
            .unwrap_or(self.fallback)
    }
}

fn escaped(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte == b'"' || byte == b'\\' || !(32..127).contains(&byte) {
            write!(out, "\\{byte:02X}").unwrap();
        } else {
            out.push(byte as char);
        }
    }
    out
}

/// Synthetic DWARF is only an intermediate machine-to-anchor map. Its file is
/// the item's identity and its line is the item-relative byte offset plus one.
/// The final link writer resolves these rows against the current SourceMap.
pub(super) fn debug_plan(
    module: &Module,
    failures: &failure::Failures,
    next: &mut usize,
    declarations: &mut BTreeSet<String>,
) -> Vec<Option<DebugFunction>> {
    fn node(next: &mut usize, declarations: &mut BTreeSet<String>, value: String) -> usize {
        let id = *next;
        *next += 1;
        declarations.insert(format!("!{id} = {value}"));
        id
    }
    let mut files = std::collections::BTreeMap::new();
    for body in module.functions.iter().filter_map(|f| f.body.as_ref()) {
        for anchor in body
            .inst_anchors
            .iter()
            .flatten()
            .chain(body.blocks.iter().filter_map(|b| b.anchor.as_ref()))
        {
            files.entry(anchor.item.0).or_insert_with(|| {
                node(
                    next,
                    declarations,
                    format!(
                        "!DIFile(filename: \"{}\", directory: \"\")",
                        escaped(&module.items[anchor.item.index()].to_string())
                    ),
                )
            });
        }
    }
    let Some(&first_file) = files.values().next() else {
        return module.functions.iter().map(|_| None).collect();
    };
    let unit = node(
        next,
        declarations,
        format!(
            "distinct !DICompileUnit(language: DW_LANG_Rust, file: !{first_file}, producer: \"Hirsel\", isOptimized: false, runtimeVersion: 0, emissionKind: FullDebug)"
        ),
    );
    let dwarf_flag = node(
        next,
        declarations,
        "!{i32 2, !\"Dwarf Version\", i32 4}".into(),
    );
    let debug_flag = node(
        next,
        declarations,
        "!{i32 2, !\"Debug Info Version\", i32 3}".into(),
    );
    declarations.insert(format!("!llvm.dbg.cu = !{{!{unit}}}"));
    declarations.insert(format!(
        "!llvm.module.flags = !{{!{dwarf_flag}, !{debug_flag}}}"
    ));
    let types = node(next, declarations, "!{}".into());
    let signature = node(
        next,
        declarations,
        format!("!DISubroutineType(types: !{types})"),
    );
    module.functions.iter().enumerate().map(|(i, function)| {
        let body = function.body.as_ref()?;
        let first = body.inst_anchors.iter().flatten().next().copied()
            .or_else(|| body.blocks.iter().find_map(|b| b.anchor))?;
        let file = files[&first.item.0];
        let linkage = format!("{}{}", symbol(module, FuncId(i as u32)), if failures.may_fail[i] { "" } else { ".infallible" });
        let name = &module.instances[function.instance.index()].definition;
        let scope = node(next, declarations, format!("distinct !DISubprogram(name: \"{}\", linkageName: \"{linkage}\", scope: !{file}, file: !{file}, line: 1, type: !{signature}, scopeLine: 1, spFlags: DISPFlagDefinition, unit: !{unit})", escaped(&name.to_string())));
        let fallback = node(next, declarations, format!("!DILocation(line: 0, scope: !{scope})"));
        let mut scopes = std::collections::BTreeMap::from([(first.item.0, scope)]);
        let mut locations = std::collections::BTreeMap::new();
        for anchor in body.inst_anchors.iter().flatten().chain(body.blocks.iter().filter_map(|b| b.anchor.as_ref())) {
            let scope = *scopes.entry(anchor.item.0).or_insert_with(|| node(next, declarations, format!("!DILexicalBlockFile(scope: !{scope}, file: !{}, discriminator: 0)", files[&anchor.item.0])));
            locations.entry((anchor.item.0, anchor.start)).or_insert_with(|| node(next, declarations, format!("!DILocation(line: {}, column: 1, scope: !{scope})", u64::from(anchor.start) + 1)));
        }
        let wrapper = if failures.may_fail[i] { None } else {
            let linkage = symbol(module, FuncId(i as u32));
            let wrapper = node(next, declarations, format!("distinct !DISubprogram(name: \"{}\", linkageName: \"{linkage}\", scope: !{file}, file: !{file}, line: 1, type: !{signature}, scopeLine: 1, spFlags: DISPFlagDefinition, unit: !{unit})", escaped(&name.to_string())));
            let location = node(next, declarations, format!("!DILocation(line: 0, scope: !{wrapper})"));
            Some((wrapper, location))
        };
        Some(DebugFunction { scope, wrapper, fallback, locations })
    }).collect()
}
