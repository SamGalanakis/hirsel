//! Source-independent machine rows, encoded as standard DWARF for the one
//! shared driver reader. A DWARF file is a DefPath; its line is byte-start+1.
use super::*;
use crate::native_dwarf::{Function, Resolved};

fn position(input: &c::Module, anchor: Anchor) -> Resolved {
    Resolved {
        path: input.items[anchor.item.index()].to_string(),
        line: u64::from(anchor.start) + 1,
        column: 1,
    }
}
pub(super) fn function(
    input: &c::Module,
    function: &c::Function,
    code: &cranelift_codegen::CompiledCode,
) -> Option<Function> {
    let body = function.body.as_ref()?;
    let instance = &input.instances[function.instance.index()];
    let anchor = body
        .inst_anchors
        .iter()
        .copied()
        .flatten()
        .chain(body.blocks.iter().filter_map(|b| b.anchor))
        .next()?;
    let declaration = position(
        input,
        Anchor {
            start: 0,
            end: 0,
            ..anchor
        },
    );
    let rows = code
        .buffer
        .get_srclocs_sorted()
        .iter()
        .filter_map(|range| {
            let index = range.loc.bits().checked_sub(1)? as usize;
            let anchor = if index < body.insts.len() {
                body.inst_anchors.get(index).copied().flatten()
            } else {
                body.blocks.get(index - body.insts.len())?.anchor
            }?;
            Some((
                u64::from(range.start),
                u64::from(range.end),
                position(input, anchor),
            ))
        })
        .collect();
    Some(Function {
        symbol: symbol(instance),
        name: instance.definition.to_string(),
        size: code.code_buffer().len() as u64,
        declaration,
        rows,
    })
}
