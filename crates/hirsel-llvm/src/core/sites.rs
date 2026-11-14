//! Item-relative causes and the caller location inherited through core/generated code.
use super::*;

impl FunctionEmitter<'_> {
    pub(super) fn call_site_word(&mut self, callee: Callee) -> Value {
        if let Callee::Direct(f) = callee {
            let function = &self.module.functions[f.index()];
            if !hirsel_core_ir::source::inherits_site(
                &self.module.instances[function.instance.index()],
            ) {
                return Value::Poison;
            }
        }
        self.site_word()
    }

    pub(super) fn site_word(&mut self) -> Value {
        let Some(word) = self.fixed_site_word() else {
            return if self.function.is_none_or(|f| {
                let function = &self.module.functions[f.index()];
                hirsel_core_ir::source::inherits_site(
                    &self.module.instances[function.instance.index()],
                )
            }) {
                self.caller_site.clone()
            } else {
                int(0)
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
            self.items.global(Global {
                name: pool.clone().into(),
                linkage: Linkage::Private,
                constant: true,
                externally_initialized: true,
                ty: Type::I64,
                init: Some(word),
                align: 8,
                absolute_symbol: false,
            });
            self.load(&Type::I64, &Value::global(pool), 8)
        } else {
            word
        }
    }

    fn absolute_symbol(&mut self, name: String) {
        self.items.global(Global {
            name: name.into(),
            linkage: Linkage::External,
            constant: true,
            externally_initialized: false,
            ty: Type::I8,
            init: None,
            align: 0,
            absolute_symbol: true,
        });
    }

    pub(super) fn fixed_site_word(&mut self) -> Option<Value> {
        let anchor = self
            .anchor
            .filter(|a| !self.module.items[a.item.index()].package_is("core", "1"))?;
        if !self.relocatable_sites {
            return Some(int(
                ((u64::from(anchor.item.0) + 1) << 32) | u64::from(anchor.start)
            ));
        }
        let symbol = hirsel_core_ir::source::site_symbol(&self.module.items[anchor.item.index()]);
        Some(match self.abi.target {
            Target::X86_64Linux => {
                // A nonzero IR addend lets X86ISel fold this into a PC-relative
                // LEA, even with !absolute_symbol. Resolve the addend in asm;
                // this temporary name disappears and ELF references only the
                // canonical symbol with its original addend.
                let alias = format!(".L{symbol}_{}", anchor.start);
                self.items
                    .asm(format!(".set {alias}, {symbol}+{}", anchor.start));
                self.absolute_symbol(alias.clone());
                Value::PtrToInt(alias.into())
            }
            Target::Aarch64Linux => {
                self.absolute_symbol(symbol.clone());
                Value::Add(
                    Box::new(Value::PtrToInt(symbol.into())),
                    i64::from(anchor.start),
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

/// Synthetic DWARF is only an intermediate machine-to-anchor map. Its file is
/// the item's identity and its line is the item-relative byte offset plus one.
/// The final link writer resolves these rows against the current SourceMap.
pub(super) fn debug_plan(
    module: &Module,
    failures: &failure::Failures,
    next: &mut usize,
    sink: &mut dyn Sink,
) -> Vec<Option<DebugFunction>> {
    let mut node = |value: Metadata| {
        let id = *next;
        *next += 1;
        sink.metadata(id, &value);
        id
    };
    let mut files = std::collections::BTreeMap::new();
    for body in module.functions.iter().filter_map(|f| f.body.as_ref()) {
        for anchor in body
            .inst_anchors
            .iter()
            .flatten()
            .chain(body.blocks.iter().filter_map(|b| b.anchor.as_ref()))
        {
            files.entry(anchor.item.0).or_insert_with(|| {
                node(Metadata::File {
                    filename: module.items[anchor.item.index()].to_string(),
                })
            });
        }
    }
    let Some(&first_file) = files.values().next() else {
        return module.functions.iter().map(|_| None).collect();
    };
    let unit = node(Metadata::CompileUnit { file: first_file });
    let flag = |key: &str, value| Metadata::Tuple {
        distinct: false,
        items: vec![
            MdItem::I32(2),
            MdItem::String(key.into()),
            MdItem::I32(value),
        ],
    };
    let dwarf_flag = node(flag("Dwarf Version", 4));
    let debug_flag = node(flag("Debug Info Version", 3));
    let types = node(Metadata::Tuple {
        distinct: false,
        items: vec![],
    });
    let signature = node(Metadata::SubroutineType { types });
    let plan = module
        .functions
        .iter()
        .enumerate()
        .map(|(i, function)| {
            let body = function.body.as_ref()?;
            let first = body
                .inst_anchors
                .iter()
                .flatten()
                .next()
                .copied()
                .or_else(|| body.blocks.iter().find_map(|b| b.anchor))?;
            let file = files[&first.item.0];
            let base = symbol(module, FuncId(i as u32));
            let name = module.instances[function.instance.index()]
                .definition
                .to_string();
            let subprogram = |linkage: String| Metadata::Subprogram {
                name: name.clone(),
                linkage,
                file,
                ty: signature,
                unit,
            };
            let linkage = if failures.may_fail[i] {
                base.clone()
            } else {
                format!("{base}.infallible")
            };
            let scope = node(subprogram(linkage));
            let fallback = node(Metadata::Location {
                line: 0,
                column: 0,
                scope,
            });
            let mut scopes = std::collections::BTreeMap::from([(first.item.0, scope)]);
            let mut locations = std::collections::BTreeMap::new();
            for anchor in body
                .inst_anchors
                .iter()
                .flatten()
                .chain(body.blocks.iter().filter_map(|b| b.anchor.as_ref()))
            {
                let scope = *scopes.entry(anchor.item.0).or_insert_with(|| {
                    node(Metadata::LexicalBlockFile {
                        scope,
                        file: files[&anchor.item.0],
                    })
                });
                locations
                    .entry((anchor.item.0, anchor.start))
                    .or_insert_with(|| {
                        node(Metadata::Location {
                            line: u64::from(anchor.start) + 1,
                            column: 1,
                            scope,
                        })
                    });
            }
            let wrapper = if failures.may_fail[i] {
                None
            } else {
                let wrapper = node(subprogram(base));
                let location = node(Metadata::Location {
                    line: 0,
                    column: 0,
                    scope: wrapper,
                });
                Some((wrapper, location))
            };
            Some(DebugFunction {
                scope,
                wrapper,
                fallback,
                locations,
            })
        })
        .collect();
    sink.named_metadata("llvm.dbg.cu", &[unit]);
    sink.named_metadata("llvm.module.flags", &[dwarf_flag, debug_flag]);
    plan
}
