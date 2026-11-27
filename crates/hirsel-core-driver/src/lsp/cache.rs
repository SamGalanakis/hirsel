use super::completion::Callable;
use crate::{ContentHash, ModuleInput, Resolution};
use hirsel_core_ids::DefPath;
use hirsel_core_resolve::SourceId;
use hirsel_core_syntax::{Derive, Item, ParseMode, ParseOutput, Span, Symbols};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::Arc;

pub(super) type FieldTypes = BTreeMap<(DefPath, String), (Arc<str>, Option<DefPath>)>;

type EditorType = (Span, String, Option<DefPath>);

const PARSE_BUDGET: usize = 32 << 20;
const TYPE_BUDGET: usize = 16 << 20;

#[derive(Default)]
pub(crate) struct Cache {
    parsed: HashMap<ContentHash, Parsed>,
    parse_bytes: usize,
    bodies: BTreeMap<(DefPath, ContentHash), Arc<[EditorType]>>,
    body_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Stats {
    pub(super) parse_hits: usize,
    pub(super) parse_misses: usize,
    pub(super) body_hits: usize,
    pub(super) body_checks: usize,
    pub(super) retained_bytes: usize,
}

struct Parsed {
    output: ParseOutput,
    added: Vec<String>,
    after: ContentHash,
}

#[derive(Default)]
pub(crate) struct Check {
    stats: Stats,
    pub(super) resolutions: Vec<Vec<Arc<Resolution>>>,
    pub(super) types: BTreeMap<(SourceId, u32), Arc<[EditorType]>>,
    pub(super) callables: BTreeMap<DefPath, Callable>,
    pub(super) declared_types: BTreeMap<DefPath, Arc<str>>,
    pub(super) field_types: FieldTypes,
}

impl Check {
    pub(crate) fn declarations(&mut self, program: &hirsel_core_front::Program<'_>) {
        use hirsel_core_front::{ERROR, TyKind, canonical};
        let shown = |ty| Arc::<str>::from(canonical(&program.decls, &program.types, ty));
        let nominal = |ty| match program.types.kind(ty) {
            TyKind::Adt(decl, _) => Some(program.decls[decl.index()].path.clone()),
            _ => None,
        };
        for function in program.fns.iter().filter(|f| f.callable != ERROR) {
            self.callables.insert(
                function.path.clone(),
                Callable {
                    parameters: function
                        .params
                        .iter()
                        .map(|param| {
                            (
                                program.index.symbols.text(param.name).to_owned(),
                                nominal(param.ty),
                            )
                        })
                        .collect(),
                    result: nominal(function.result),
                },
            );
            self.declared_types
                .insert(function.path.clone(), shown(function.callable));
        }
        for constant in program.constants.iter().filter(|c| c.ty != ERROR) {
            self.declared_types
                .insert(constant.path.clone(), shown(constant.ty));
        }
        for decl in program.decls.iter() {
            for field in decl.fields(None).iter().filter(|f| f.ty != ERROR) {
                let nominal = nominal(field.ty);
                let name = field.name.clone();
                self.field_types
                    .insert((decl.path.clone(), name), (shown(field.ty), nominal));
            }
        }
    }

    pub(crate) fn resolutions(&mut self, resolutions: Vec<Vec<Arc<Resolution>>>) {
        self.resolutions = resolutions;
    }

    pub(crate) fn body(
        &mut self,
        cache: &Cache,
        path: &DefPath,
        key: ContentHash,
        module: &ModuleInput,
        ordinal: u32,
        item: Item<'_>,
    ) -> bool {
        let Some(rows) = cache.bodies.get(&(path.clone(), key)) else {
            return false;
        };
        // The compiler body key includes these exact significant bytes. An
        // item can move between revisions, so rebind through its current
        // occurrence and ensure every retained byte range fits that item.
        let length = item.range.end - item.file_offset;
        if rows.iter().any(|row| row.0.end > length) {
            return false;
        }
        self.types
            .insert((module.source, ordinal), Arc::clone(rows));
        self.stats.body_hits += 1;
        true
    }

    pub(crate) fn checked(
        &mut self,
        cache: &mut Cache,
        path: &DefPath,
        key: ContentHash,
        module: &ModuleInput,
        ordinal: u32,
        rows: Vec<EditorType>,
        reusable: bool,
    ) {
        self.stats.body_checks += 1;
        let bytes = rows.len() * std::mem::size_of::<EditorType>()
            + rows
                .iter()
                .map(|row| row.1.capacity() + row.2.as_ref().map_or(0, path_bytes))
                .sum::<usize>()
            + std::mem::size_of::<(DefPath, ContentHash, Arc<[EditorType]>)>()
            + path_bytes(path);
        let rows: Arc<[_]> = rows.into();
        if reusable && bytes <= TYPE_BUDGET {
            if cache.body_bytes + bytes > TYPE_BUDGET {
                cache.bodies.clear();
                cache.body_bytes = 0;
            }
            cache.bodies.insert((path.clone(), key), Arc::clone(&rows));
            cache.body_bytes += bytes;
        }
        self.types.insert((module.source, ordinal), rows);
    }

    pub(super) fn stats(&self, cache: &Cache) -> Stats {
        Stats {
            retained_bytes: cache.body_bytes + cache.parse_bytes,
            ..self.stats
        }
    }

    pub(crate) fn parse(
        &mut self,
        cache: &mut Cache,
        prefix: &mut ContentHash,
        symbols: &mut Symbols,
        hash: ContentHash,
        bytes: &[u8],
        mode: ParseMode,
        derive: Option<&Derive>,
    ) -> io::Result<(ParseOutput, bool)> {
        let style = derive.map(|d| (&d.codec, &d.path));
        let key = crate::native::key("hirsel.editor-parse.v1", &(*prefix, hash, mode, style))?;
        if let Some(parsed) = cache.parsed.get(&key) {
            self.stats.parse_hits += 1;
            for name in &parsed.added {
                symbols.intern(name);
            }
            *prefix = parsed.after;
            return Ok((parsed.output.clone(), false));
        }
        self.stats.parse_misses += 1;
        let start = symbols.len();
        let output = hirsel_core_resolve::parse_module(bytes, mode, symbols, derive);
        let added = crate::resident::symbol_suffix(symbols, start);
        if !added.is_empty() {
            *prefix = crate::native::key("hirsel.editor-symbols.v1", &(*prefix, &added))?;
        }
        let syntax = &output.syntax;
        let size = std::mem::size_of::<Parsed>()
            + syntax.source.len()
            + syntax.items.len() * std::mem::size_of::<hirsel_core_syntax::ItemRange>()
            + syntax.tree.tokens.len() * std::mem::size_of::<hirsel_core_syntax::Token>()
            + syntax.tree.nodes.len() * std::mem::size_of::<hirsel_core_syntax::Node>()
            + syntax.tree.edges.len() * std::mem::size_of::<hirsel_core_syntax::NodeId>()
            + syntax.tree.canonical.len()
            + syntax.tree.canonical_offsets.len() * std::mem::size_of::<u32>()
            + added.len() * std::mem::size_of::<String>()
            + added.iter().map(String::capacity).sum::<usize>();
        // Refusals may own unbounded diagnostic text. Keep only clean parses
        // within the retention budget. A miss always does the same work.
        if output.diagnostics.is_empty()
            && output.resource_refusal.is_none()
            && size <= PARSE_BUDGET
        {
            if cache.parse_bytes + size > PARSE_BUDGET {
                cache.parsed.clear();
                cache.parse_bytes = 0;
            }
            cache.parsed.insert(
                key,
                Parsed {
                    output: output.clone(),
                    added,
                    after: *prefix,
                },
            );
            cache.parse_bytes += size;
        }
        Ok((output, true))
    }
}

fn path_bytes(path: &DefPath) -> usize {
    path.0.modules.len() * std::mem::size_of::<String>()
        + path.0.modules.iter().map(String::capacity).sum::<usize>()
        + path.0.name.capacity()
        + path.0.package.name.capacity()
        + path.0.package.version.capacity()
}
