//! Readable detail for storage violations. A `LeakedStorage` names only a slot
//! id; this traces the slot to its type, the stores that fill it, the
//! instruction or call that produced each stored value, and the source
//! correspondence of each, so one refusal names the leaking value.

use std::fmt::Write as _;

use super::{Violation, ViolationKind};
use crate::ir::{
    AddressBase, AddressId, Callee, Inst, LirUnit, Ref, SlotId, SourceAnchorId, Terminator, ValueId,
};
use crate::layout_types::LayoutTable;

/// One line per distinct leaked slot, then one indented line per store into it
/// and per call writing it through a destination. Empty when no violation is a
/// leak.
pub fn describe_leaks<S>(
    unit: &LirUnit<S>,
    layouts: &LayoutTable,
    violations: &[Violation],
) -> String {
    let mut slots: Vec<SlotId> = violations
        .iter()
        .filter_map(|v| match v.kind {
            ViolationKind::LeakedStorage(slot) => Some(slot),
            _ => None,
        })
        .collect();
    slots.sort();
    slots.dedup();
    let mut out = String::new();
    for slot in slots {
        let Some(info) = unit.slots.get(slot.index()) else {
            continue;
        };
        let _ = write!(
            out,
            "\n  slot {} {:?}: {}",
            slot.0,
            info.kind,
            ty(unit, layouts, info.ty)
        );
        let mut any = false;
        for (b, block) in unit.blocks.iter().enumerate() {
            let insts = block.instructions.get(&unit.instructions).unwrap_or(&[]);
            for (i, inst) in insts.iter().enumerate() {
                let value = match *inst {
                    Inst::Store { address, value, .. } | Inst::Replace { address, value, .. }
                        if root(unit, address) == Some(slot) =>
                    {
                        value
                    }
                    _ => continue,
                };
                any = true;
                let _ = write!(
                    out,
                    "\n    filled at b{b}#{i} {} by {}",
                    site(unit, block.anchor),
                    producer(unit, value)
                );
            }
            let terminator = &unit.terminators[b];
            if let Terminator::Call { call, .. } = *terminator {
                let mut writes = false;
                terminator.visit(&mut |r| {
                    if let Ref::Address(a) = r {
                        writes |= root(unit, a) == Some(slot);
                    }
                });
                if writes {
                    any = true;
                    let site_ = &unit.tables.calls[call.index()];
                    let _ = write!(
                        out,
                        "\n    written by call {} at b{b} {}",
                        callee(unit, &site_.callee),
                        site(unit, site_.anchor)
                    );
                }
            }
        }
        if !any {
            let _ = write!(out, "\n    (no store into the slot)");
        }
        // Every other instruction naming the slot: the moves, drops and storage
        // events whose absence on some path is the leak.
        let mut touches = Vec::new();
        for (b, block) in unit.blocks.iter().enumerate() {
            let insts = block.instructions.get(&unit.instructions).unwrap_or(&[]);
            for (i, inst) in insts.iter().enumerate() {
                if matches!(inst, Inst::Store { .. } | Inst::Replace { .. }) {
                    continue;
                }
                let mut named = false;
                inst.visit(&mut |r| match r {
                    Ref::Slot(s) => named |= s == slot,
                    Ref::Address(a) => named |= root(unit, a) == Some(slot),
                    _ => {}
                });
                if named {
                    touches.push(format!("b{b}#{i} {}", detail(inst)));
                }
            }
        }
        if !touches.is_empty() {
            let _ = write!(out, "\n    touched by {}", touches.join(", "));
        }
    }
    out
}

/// The slot an address is rooted at, through nested addresses.
fn root<S>(unit: &LirUnit<S>, mut address: AddressId) -> Option<SlotId> {
    for _ in 0..unit.addresses.len() {
        match unit.addresses.get(address.index())?.base {
            AddressBase::Slot(slot) => return Some(slot),
            AddressBase::Address(next) => address = next,
            AddressBase::Value(_) => return None,
        }
    }
    None
}

fn ty<S>(unit: &LirUnit<S>, layouts: &LayoutTable, id: crate::ir::LirTypeId) -> String {
    let Some(t) = unit.types.get(id.index()) else {
        return format!("type {}", id.0);
    };
    match layouts.layouts.get(t.layout.0 as usize) {
        Some(layout) => format!(
            "{:?}, layout {} {:?} size {:?} drop {:?}",
            t.repr, t.layout.0, layout.kind, layout.size, layout.drop
        ),
        None => format!("{:?}, layout {}", t.repr, t.layout.0),
    }
}

/// The instruction, call result or block parameter defining `value`.
fn producer<S>(unit: &LirUnit<S>, value: ValueId) -> String {
    for (b, block) in unit.blocks.iter().enumerate() {
        let insts = block.instructions.get(&unit.instructions).unwrap_or(&[]);
        for (i, inst) in insts.iter().enumerate() {
            if inst.defs().contains(&value) {
                let from = match *inst {
                    Inst::Read { address, .. } | Inst::Copy { address, .. } => {
                        match root(unit, address) {
                            Some(s) => format!(" of slot {}", s.0),
                            None => " of a value".into(),
                        }
                    }
                    _ => String::new(),
                };
                return format!(
                    "{}{from} at b{b}#{i} {}",
                    detail(inst),
                    site(unit, block.anchor)
                );
            }
        }
        if let Terminator::Call { call, .. } = unit.terminators[b] {
            let call_site = &unit.tables.calls[call.index()];
            if call_site.result == Some(value) {
                return format!(
                    "call {} at b{b} {}",
                    callee(unit, &call_site.callee),
                    site(unit, call_site.anchor)
                );
            }
        }
        let parameters = block.parameters.get(&unit.pools.values).unwrap_or(&[]);
        if parameters.contains(&value) {
            return format!("parameter of b{b} {}", site(unit, block.anchor));
        }
    }
    if unit.parameters.contains(&value) {
        return format!("unit parameter v{}", value.0);
    }
    format!("v{} (no definition)", value.0)
}

fn callee<S>(unit: &LirUnit<S>, callee: &Callee) -> String {
    match callee {
        Callee::Direct(symbol) => unit
            .tables
            .symbols
            .get(symbol.index())
            .map(|s| s.definition.to_string())
            .unwrap_or_else(|| format!("symbol {}", symbol.0)),
        other => format!("{other:?}"),
    }
}

/// The variant name of an instruction, with the read mode of a `Read`.
fn detail(inst: &Inst) -> String {
    match inst {
        Inst::Read { mode, .. } => format!("Read {mode:?}"),
        other => head(other),
    }
}

/// The variant name of an instruction, without its fields.
fn head(inst: &Inst) -> String {
    let text = format!("{inst:?}");
    let end = text.find([' ', '{', '(']).unwrap_or(text.len());
    text[..end].to_string()
}

/// TIR `(body, block, statement)` and the byte range in the definition's source.
fn site<S>(unit: &LirUnit<S>, anchor: SourceAnchorId) -> String {
    match unit.tables.source_anchors.get(anchor.index()) {
        Some(a) if a.site.source != u32::MAX => format!(
            "[tir {}:{}:{} source {} bytes {}..{}]",
            a.body, a.block, a.statement, a.site.source, a.site.start, a.site.end
        ),
        Some(a) => format!("[tir {}:{}:{} unmapped]", a.body, a.block, a.statement),
        None => "[no anchor]".into(),
    }
}
