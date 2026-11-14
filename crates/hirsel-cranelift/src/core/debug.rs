//! Item-relative machine locations beside the untouched linker input.
use super::*;
use c::source::{
    CodeRange, CodeSection, DebugScope, LineRow, LineSequence, NativeDebug, NativeLocation,
    ScopeKind,
};
use cranelift_object::object::{
    self,
    write::{Symbol, SymbolSection},
};
use hirsel_core_ids::ContentHash;

pub(super) struct FunctionRows {
    function: cranelift_module::FuncId,
    linkage: String,
    name: String,
    declaration: Option<NativeLocation>,
    size: u64,
    rows: Vec<LineRow>,
}

fn row(rows: &mut Vec<LineRow>, offset: u64, source: Option<NativeLocation>) {
    if let Some(last) = rows.last_mut() {
        if last.offset == offset {
            last.source = source;
            if rows.len() > 1 && rows[rows.len() - 2].source == source {
                rows.pop();
            }
            return;
        }
        if last.source == source {
            return;
        }
    }
    rows.push(LineRow { offset, source });
}

pub(super) fn function(
    input: &c::Module,
    function: &c::Function,
    id: cranelift_module::FuncId,
    code: &cranelift_codegen::CompiledCode,
) -> FunctionRows {
    let body = function.body.as_ref().unwrap();
    let instance = &input.instances[function.instance.index()];
    let item = input
        .items
        .iter()
        .position(|item| *item == instance.definition)
        .map(|i| i as u32)
        .or_else(|| {
            body.inst_anchors
                .iter()
                .copied()
                .flatten()
                .chain(body.blocks.iter().filter_map(|b| b.anchor))
                .next()
                .map(|anchor| anchor.item.0)
        });
    let size = code.code_buffer().len() as u64;
    let mut rows = Vec::new();
    row(&mut rows, 0, None);
    for range in code.buffer.get_srclocs_sorted() {
        let source = range
            .loc
            .bits()
            .checked_sub(1)
            .and_then(|index| {
                let index = index as usize;
                if index < body.insts.len() {
                    body.inst_anchors.get(index).copied().flatten()
                } else {
                    body.blocks
                        .get(index - body.insts.len())
                        .and_then(|b| b.anchor)
                }
            })
            .map(|anchor| NativeLocation {
                item: anchor.item.0,
                start: anchor.start,
            });
        row(&mut rows, u64::from(range.start), source);
        row(&mut rows, u64::from(range.end), None);
    }
    if rows.last().is_some_and(|r| r.offset == size) {
        rows.pop();
    }
    FunctionRows {
        function: id,
        linkage: symbol(instance),
        name: instance.definition.to_string(),
        declaration: item.map(|item| NativeLocation { item, start: 0 }),
        size,
        rows,
    }
}

pub(super) fn finish(
    input: &c::Module,
    product: &mut cranelift_object::ObjectProduct,
    functions: Vec<FunctionRows>,
    object_key: ContentHash,
) -> NativeDebug {
    let mut debug = NativeDebug {
        items: input.items.clone(),
        ..NativeDebug::default()
    };
    let mut sections = std::collections::HashMap::new();
    for mut function in functions {
        let symbol = product
            .object
            .symbol(product.function_symbol(function.function));
        let section = symbol.section.id().unwrap();
        let begin = symbol.value;
        let section_index = *sections.entry(section).or_insert_with(|| {
            let index = debug.sections.len() as u32;
            let name = c::source::debug_symbol(object_key, index);
            let size = product.object.section(section).data().len() as u64;
            product.object.add_symbol(Symbol {
                name: name.as_bytes().to_vec(),
                value: 0,
                size: 0,
                kind: object::SymbolKind::Text,
                scope: object::SymbolScope::Linkage,
                weak: false,
                section: SymbolSection::Section(section),
                flags: object::SymbolFlags::Elf {
                    st_info: object::elf::SymbolInfo(object::elf::STB_GLOBAL.0 << 4),
                    st_other: object::elf::SymbolOther(object::elf::STV_HIDDEN.0),
                },
            });
            debug.sections.push(CodeSection { symbol: name, size });
            index
        });
        let range = CodeRange {
            section: section_index,
            begin,
            end: begin + function.size,
        };
        for row in &mut function.rows {
            row.offset += begin;
        }
        debug.sequences.push(LineSequence {
            range,
            rows: function.rows,
        });
        debug.scopes.push(DebugScope {
            parent: None,
            origin: None,
            kind: ScopeKind::Function {
                linkage: function.linkage,
            },
            name: function.name,
            declaration: function.declaration,
            call: None,
            ranges: vec![range],
        });
    }
    debug
}
