//! Item-relative causes and the caller location inherited through core/generated code.
use super::*;

impl FunctionEmitter<'_> {
    pub(super) fn call_site_word(&mut self, callee: Callee) -> Value {
        if let Callee::Direct(f) = callee
            && !self.module.passes_caller_site(f)
        {
            return Value::Poison;
        }
        self.site_word()
    }

    /// Record the site owner (`*const CauseData`, or null for host/immortal) into
    /// `context.site_owner` before a call or raise that supplies a site ordinal.
    pub(super) fn store_site_owner(&mut self, own: bool) {
        if !self.module.track_pack_owners {
            return;
        }
        let owner = if own {
            self.items.global(Global {
                name: "hirsel_core_cause_owner".into(),
                linkage: Linkage::External,
                constant: true,
                externally_initialized: false,
                ty: Type::I64,
                init: None,
                align: 8,
                absolute_symbol: false,
                section: None,
            });
            self.load(&Type::I64, &Value::global("hirsel_core_cause_owner"), 8)
        } else if self.function.is_none_or(|f| {
            let function = &self.module.functions[f.index()];
            hirsel_core_ir::source::inherits_site(&self.module.instances[function.instance.index()])
        }) {
            self.caller_site_owner.clone()
        } else {
            int(0)
        };
        let slot = self.offset(&CONTEXT, memory_layout::CONTEXT_SITE_OWNER as u64);
        self.store(&Type::I64, &owner, &slot, 8);
    }

    /// Capture the caller's site owner at entry to a function or glue that inherits
    /// its caller's site (`PACK-9.7`).
    pub(super) fn capture_caller_site_owner(&mut self) {
        if !self.module.track_pack_owners {
            return;
        }
        let slot = self.offset(&CONTEXT, memory_layout::CONTEXT_SITE_OWNER as u64);
        self.caller_site_owner = self.load(&Type::I64, &slot, 8);
    }

    /// Recipes clean under the site and site owner their release was called with.
    pub(super) fn release_site(&mut self) {
        let list = Value::Local(Local::WORKLIST);
        let at = self.offset(&list, memory_layout::WORKLIST_SITE as u64);
        self.caller_site = self.load(&Type::I64, &at, 8);
        if self.module.track_pack_owners {
            let owner_at = self.offset(&list, memory_layout::WORKLIST_SITE_OWNER as u64);
            self.caller_site_owner = self.load(&Type::I64, &owner_at, 8);
        }
    }

    pub(super) fn raise(&mut self, kind: u8) {
        let fixed_site = self.fixed_site_word();
        let fixed = fixed_site.is_some();
        if fixed {
            self.store_site_owner(true);
        }
        let site = fixed_site.unwrap_or_else(|| self.site_word());
        let fixed = fixed || site == int(0);
        let shape = Type::bytes(memory_layout::CAUSE_BYTES as u64);
        let argument = abi::composite_argument(self.abi.target, memory_layout::CAUSE_BYTES);
        // Built-in causes have fixed bytes. SysV byval supplies a caller copy;
        // AAPCS64 needs an explicit private copy before passing its pointer.
        // Neither convention lets the callee mutate the shared constant.
        let relocated = fixed && self.relocatable_sites && site != int(0);
        let suffix = if relocated {
            if let Some(sites) = &self.items.release_sites {
                let anchor = self.anchor.unwrap();
                sites.names[&(anchor.item, anchor.start)].clone()
            } else {
                let anchor = self.anchor.unwrap();
                format!("{}_{}", anchor.item.0, anchor.start)
            }
        } else {
            match site {
                Value::Int(n) => n.to_string(),
                _ => "0".into(),
            }
        };
        let name = format!("hirsel_cause_{kind}_{suffix}");
        let prefix = Type::bytes(memory_layout::CAUSE_KIND as u64);
        let word = memory_layout::cause_kind_word(kind) as i64;
        // A relocated word in a cause needs the same materialization as a
        // standalone site word.
        let initialized = relocated && self.keep_site_in_data();
        self.items.global(Global {
            name: name.clone().into(),
            linkage: Linkage::Private,
            constant: true,
            externally_initialized: initialized,
            ty: Type::Struct(vec![prefix.clone(), Type::I64, Type::I64], true),
            init: Some(Value::Struct {
                packed: true,
                fields: vec![
                    (prefix, Value::Zero),
                    (Type::I64, int(word)),
                    (Type::I64, if fixed { site.clone() } else { int(0) }),
                ],
            }),
            align: 8,
            absolute_symbol: false,
            section: None,
        });
        let pointer = if argument.byval.is_none() || !fixed {
            let pointer = self.temp(shape);
            let words = Type::words(memory_layout::CAUSE_BYTES as u64 / 8);
            let value = self.load(&words, &Value::global(name), 8);
            self.store(&words, &value, &pointer, 8);
            if !fixed {
                let at = self.offset(&pointer, memory_layout::CAUSE_SITE as i128);
                self.store(&Type::I64, &site, &at, 8);
            }
            pointer
        } else {
            Value::global(name)
        };
        self.call_declared(
            "hirsel_core_raise",
            Type::Void,
            &[ptr(CONTEXT), (argument, pointer)],
            FnAttrs::default(),
            &CallAttrs::default(),
        );
    }

    pub(super) fn site_word(&mut self) -> Value {
        let Some(word) = self.fixed_site_word() else {
            self.store_site_owner(false);
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
        self.store_site_owner(true);
        if self.keep_site_in_data() {
            // AArch64 selects a GOT reference even with !absolute_symbol.
            // X86 can fold item site words into PC-relative LEAs.
            // externally_initialized keeps LLVM from folding the load.
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
                section: None,
            });
            self.load(&Type::I64, &Value::global(pool), 8)
        } else {
            word
        }
    }

    pub(super) fn keep_site_in_data(&self) -> bool {
        self.relocatable_sites
            && match self.abi.target.arch() {
                hirsel_core_layout::Arch::Aarch64 => true,
                hirsel_core_layout::Arch::X86_64 => self.items.release_sites.is_none(),
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
            section: None,
        });
    }

    pub(super) fn fixed_site_word(&mut self) -> Option<Value> {
        let anchor = self
            .anchor
            .filter(|a| !self.module.items[a.item.index()].package_is("core", "1"))?;
        if !self.relocatable_sites {
            let first = self.fixed_sites.get(anchor.item.index()).copied();
            return Some(int(first
                .filter(|&first| first != 0)
                .map_or(0, |first| first + u64::from(anchor.start))));
        }
        if let Some(sites) = &mut self.items.release_sites {
            let name = sites
                .names
                .entry((anchor.item, anchor.start))
                .or_insert_with(|| {
                    let item = self.module.items[anchor.item.index()].clone();
                    let owner = hirsel_core_ir::source::release_site_owner(&item);
                    let next = sites.next.entry(owner).or_default();
                    let ordinal = *next;
                    *next += 1;
                    let name = hirsel_core_ir::source::release_site_symbol(
                        &item,
                        ordinal,
                        sites.partition,
                    );
                    sites.bindings.push(hirsel_core_ir::source::ReleaseSite {
                        item,
                        start: anchor.start,
                        ordinal,
                        partition: sites.partition,
                    });
                    name
                })
                .clone();
            self.absolute_symbol(name.clone());
            return Some(self.linked_site(name));
        }
        // Module-assembly aliases would leave undefined site symbols behind
        // even if LLVM emits no relocation against them.
        let symbol = hirsel_core_ir::source::site_symbol(&self.module.items[anchor.item.index()]);
        self.absolute_symbol(symbol.clone());
        Some(Value::Add(
            Box::new(self.linked_site(symbol)),
            i64::from(anchor.start),
        ))
    }

    /// The linked value of the absolute site symbol `name`. A Mach-O unit
    /// measures it from the sites origin
    /// ([`hirsel_core_ir::source::SITES_ORIGIN_SYMBOL`]).
    fn linked_site(&mut self, name: String) -> Value {
        match self.abi.target.object_format() {
            hirsel_core_layout::ObjectFormat::Elf => Value::PtrToInt(name.into()),
            hirsel_core_layout::ObjectFormat::MachO => {
                let origin = hirsel_core_ir::source::SITES_ORIGIN_SYMBOL;
                self.absolute_symbol(origin.into());
                Value::Difference(name.into(), origin.into())
            }
        }
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

#[derive(Default)]
pub(super) struct DebugPlan {
    base: Vec<Option<DebugFunction>>,
    versions: std::collections::BTreeMap<(usize, u8), DebugFunction>,
}
impl DebugPlan {
    pub(super) fn get(&self, index: usize, level: Option<Level>) -> Option<&DebugFunction> {
        match level {
            Some(level) => self.versions.get(&(index, level.number)),
            None => self.base.get(index).and_then(Option::as_ref),
        }
    }
}

/// Synthetic DWARF is only an intermediate machine-to-anchor map. Its file is
/// the item's identity and its line is the item-relative byte offset plus one.
/// The final link writer resolves these rows against the current SourceMap.
pub(super) fn debug_plan(
    module: &Module,
    function_symbols: &[String],
    versioned: Option<&[bool]>,
    step_dispatch: &[Option<FuncId>],
    failures: &failure::Failures,
    call_layouts: &hirsel_core_ir::native_layout::CallLayouts,
    release: bool,
    partition: Option<u32>,
    members: Option<&[bool]>,
    next: &mut usize,
    sink: &mut dyn Sink,
) -> DebugPlan {
    let mut node = |value: Metadata| {
        let id = *next;
        *next += 1;
        sink.metadata(id, &value);
        id
    };
    let mut files = std::collections::BTreeMap::new();
    let mut owners = std::collections::BTreeMap::new();
    for (_, body) in super::bodies(module, members) {
        for anchor in body
            .inst_anchors
            .iter()
            .flatten()
            .chain(body.blocks.iter().filter_map(|b| b.anchor.as_ref()))
        {
            files.entry(anchor.item.0).or_insert_with(|| {
                let item = &module.items[anchor.item.index()];
                if release {
                    let owner = hirsel_core_ir::source::release_debug_owner(item, partition);
                    *owners.entry(owner.clone()).or_insert_with(|| {
                        node(Metadata::File {
                            filename: owner.to_string(),
                        })
                    })
                } else {
                    node(Metadata::File {
                        filename: item.to_string(),
                    })
                }
            });
        }
    }
    if files.is_empty() && module.functions.iter().all(|f| f.body.is_none()) {
        return DebugPlan::default();
    }
    // Generated adapters have no source positions of their own. Give them
    // an artificial scope so LLVM can describe source code inlined into them.
    let generated_file = node(Metadata::File {
        filename: "<generated>".into(),
    });
    let file = if release {
        generated_file
    } else {
        files.values().next().copied().unwrap_or(generated_file)
    };
    let unit = node(Metadata::CompileUnit { file });
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
    let mut make = |i: usize, base: &str| {
        if members.is_some_and(|members| !members[i]) {
            return None;
        }
        let function = &module.functions[i];
        let body = function.body.as_ref()?;
        let first = body
            .inst_anchors
            .iter()
            .flatten()
            .next()
            .copied()
            .or_else(|| body.blocks.iter().find_map(|b| b.anchor));
        let file = first.map_or(generated_file, |anchor| files[&anchor.item.0]);
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
        let body_base = if step_dispatch[i].is_some() {
            super::step_free::checked(base)
        } else {
            base.to_owned()
        };
        let has_status = failures.may_fail[i];
        let ok = call_layouts
            .ok_result(&module.signatures[function.signature.index()])
            .is_some();
        let linkage = format!("{body_base}{}", super::abi::private_suffix(has_status, ok));
        let scope = node(subprogram(linkage));
        let fallback = node(Metadata::Location {
            line: 0,
            column: 0,
            scope,
        });
        let mut scopes = std::collections::BTreeMap::from([(file, scope)]);
        let mut locations = std::collections::BTreeMap::new();
        for anchor in body
            .inst_anchors
            .iter()
            .flatten()
            .chain(body.blocks.iter().filter_map(|b| b.anchor.as_ref()))
        {
            let file = files[&anchor.item.0];
            let scope = *scopes
                .entry(file)
                .or_insert_with(|| node(Metadata::LexicalBlockFile { scope, file }));
            locations
                .entry((anchor.item.0, anchor.start))
                .or_insert_with(|| {
                    node(if release {
                        Metadata::ReleaseLocation {
                            item: anchor.item.0,
                            start: anchor.start,
                            scope,
                        }
                    } else {
                        Metadata::Location {
                            line: u64::from(anchor.start) + 1,
                            column: 1,
                            scope,
                        }
                    })
                });
        }
        let wrapper = if has_status && !ok {
            None
        } else {
            let wrapper = node(subprogram(base.to_owned()));
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
    };
    let mut plan = DebugPlan::default();
    for (i, name) in function_symbols.iter().enumerate() {
        if versioned.is_some_and(|rows| rows[i]) {
            plan.base.push(None);
            for &level in X86_64_LEVELS {
                if let Some(debug) = make(i, &versions::symbol(name, level)) {
                    plan.versions.insert((i, level.number), debug);
                }
            }
        } else {
            plan.base.push(make(i, name));
        }
    }
    sink.named_metadata("llvm.dbg.cu", &[unit]);
    sink.named_metadata("llvm.module.flags", &[dwarf_flag, debug_flag]);
    plan
}

/// Debug positions are numbered on first use, independently in each
/// partition. The native and printed sinks resolve the same deferred nodes.
#[cfg(hirsel_lazy_core)]
pub(crate) struct ReleaseDebug {
    items: std::rc::Rc<[hirsel_core_ids::DefPath]>,
    partition: Option<u32>,
    pending: std::collections::HashMap<usize, (u32, u32, usize)>,
    ordinals: std::collections::HashMap<(u32, u32), u32>,
    next: std::collections::HashMap<hirsel_core_ids::DefPath, u32>,
    sites: Vec<source::ReleaseSite>,
}

#[cfg(hirsel_lazy_core)]
impl ReleaseDebug {
    pub(super) fn new(
        items: std::rc::Rc<[hirsel_core_ids::DefPath]>,
        partition: Option<u32>,
    ) -> Self {
        Self {
            items,
            partition,
            pending: Default::default(),
            ordinals: Default::default(),
            next: Default::default(),
            sites: Vec::new(),
        }
    }

    pub(super) fn defer(&mut self, id: usize, item: u32, start: u32, scope: usize) {
        self.pending.insert(id, (item, start, scope));
    }

    pub(super) fn location(&mut self, id: usize) -> Option<Metadata> {
        let (item, start, scope) = self.pending.remove(&id)?;
        let ordinal = *self.ordinals.entry((item, start)).or_insert_with(|| {
            let item = &self.items[item as usize];
            let next = self
                .next
                .entry(source::release_site_owner(item))
                .or_default();
            let ordinal = *next;
            *next += 1;
            self.sites.push(source::ReleaseSite {
                item: item.clone(),
                start,
                ordinal,
                partition: self.partition,
            });
            ordinal
        });
        Some(Metadata::Location {
            line: u64::from(ordinal) + 1,
            column: 1,
            scope,
        })
    }

    pub(super) fn sites(self) -> Vec<source::ReleaseSite> {
        self.sites
    }
}
