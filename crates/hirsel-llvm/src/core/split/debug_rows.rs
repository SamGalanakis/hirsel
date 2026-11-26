//! A release part's own rows of its program's debug data.
use hirsel_core_ir::source::{CodeSection, NativeDebug, ScopeKind};
use std::collections::HashMap;

/// The debug rows of one code section, as if its part had been generated
/// alone: the section is the part's only one, scopes are its own trees then
/// the abstract scopes they name, and items are numbered by first use. A
/// physical function scope carries its abstract origin's name and
/// declaration, so it does not depend on whether another definition inlined
/// the same function.
pub(super) fn restrict(
    debug: &NativeDebug,
    id: u32,
    symbol: String,
) -> Result<NativeDebug, String> {
    let mut out = NativeDebug {
        sections: vec![CodeSection {
            symbol,
            size: debug.sections[id as usize].size,
        }],
        ..NativeDebug::default()
    };
    let mut items: HashMap<u32, u32> = HashMap::new();
    let mut item = |location: Option<hirsel_core_ir::source::NativeLocation>,
                    out: &mut NativeDebug| {
        location.map(|mut l| {
            l.item = *items.entry(l.item).or_insert_with(|| {
                out.items.push(debug.items[l.item as usize].clone());
                out.items.len() as u32 - 1
            });
            l
        })
    };
    for sequence in debug.sequences.iter().filter(|s| s.range.section == id) {
        let mut sequence = sequence.clone();
        sequence.range.section = 0;
        for row in &mut sequence.rows {
            row.source = item(row.source, &mut out);
        }
        out.sequences.push(sequence);
    }
    // Own scopes in preorder, each after its parent; then named origins.
    let mut order: Vec<u32> = Vec::new();
    let mut kept: HashMap<u32, u32> = HashMap::new();
    fn visit(
        scopes: &[hirsel_core_ir::source::DebugScope],
        s: u32,
        order: &mut Vec<u32>,
        kept: &mut HashMap<u32, u32>,
    ) {
        if kept.contains_key(&s) {
            return;
        }
        if let Some(parent) = scopes[s as usize].parent {
            visit(scopes, parent, order, kept);
        }
        kept.insert(s, order.len() as u32);
        order.push(s);
    }
    for (s, scope) in debug.scopes.iter().enumerate() {
        if scope.ranges.iter().any(|r| r.section == id) {
            visit(&debug.scopes, s as u32, &mut order, &mut kept);
        }
    }
    let own = order.len();
    let folded = |s: u32| {
        let scope = &debug.scopes[s as usize];
        matches!(scope.kind, ScopeKind::Function { .. })
            && !scope.ranges.is_empty()
            && scope.origin.is_some()
    };
    let mut at = 0;
    while at < order.len() {
        let s = order[at];
        if let Some(origin) = debug.scopes[s as usize].origin
            && !(at < own && folded(s))
        {
            visit(&debug.scopes, origin, &mut order, &mut kept);
        }
        at += 1;
    }
    for (at, &s) in order.iter().enumerate() {
        let mut scope = debug.scopes[s as usize].clone();
        if at < own && folded(s) {
            let origin = &debug.scopes[scope.origin.take().unwrap() as usize];
            if scope.name.is_empty() {
                scope.name = origin.name.clone();
            }
            if scope.declaration.is_none() {
                scope.declaration = origin.declaration;
            }
            if let (ScopeKind::Function { linkage }, ScopeKind::Function { linkage: from }) =
                (&mut scope.kind, &origin.kind)
                && linkage.is_empty()
            {
                *linkage = from.clone();
            }
        }
        scope.parent = scope.parent.map(|p| kept[&p]);
        scope.origin = scope.origin.map(|o| kept[&o]);
        scope.ranges.retain(|r| r.section == id);
        for range in &mut scope.ranges {
            range.section = 0;
        }
        scope.declaration = item(scope.declaration, &mut out);
        scope.call = item(scope.call, &mut out);
        out.scopes.push(scope);
    }
    Ok(out)
}
