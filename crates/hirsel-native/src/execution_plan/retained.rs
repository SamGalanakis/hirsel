//! Allocation bounds use the existing native metadata visitor and Rust BTree
//! node bound, not encoded lengths. Shared observation products keep admission.
use crate::native_metadata::native_type;
use hirsel_lir::{ahead::*, continuation::*, layout_types::*, products::Binding};
use hirsel_query_products::retained::MetadataWeight as W;

fn symbol(w: &mut W, s: &hirsel_lir::ir::Symbol) {
    w.path(&s.definition);
}
fn size(w: &mut W, terms: &Vec<SizeTerm>) {
    w.vec(terms, |w, term| match term {
        SizeTerm::At(terms) => size(w, terms),
        SizeTerm::ByTag { variants, .. } => w.vec(variants, |w, (_, terms)| size(w, terms)),
        _ => (),
    });
}
fn scan(w: &mut W, rows: &Vec<ScanStep>) {
    w.vec(rows, |w, row| match row {
        ScanStep::ActiveVariant { variants, .. } => w.vec(variants, |w, (_, rows)| scan(w, rows)),
        ScanStep::Columns(columns) => w.flat_vec(&columns.columns),
        _ => (),
    });
}
fn drop_steps(w: &mut W, rows: &Vec<DropStep>) {
    w.vec(rows, |w, row| match row {
        DropStep::ActiveVariant { variants, .. } => {
            w.vec(variants, |w, (_, rows)| drop_steps(w, rows))
        }
        DropStep::ColumnsReverse(columns) => w.flat_vec(&columns.columns),
        _ => (),
    });
}
pub fn layout(w: &mut W, row: &LayoutRecord) {
    match &row.kind {
        LayoutKind::Environment { movable } => w.flat_vec(movable),
        LayoutKind::Opaque { owner } => w.path(owner),
        LayoutKind::HostCapability { kind } => w.add(kind.capacity()),
        LayoutKind::PayloadReceipt { root } => native_type(w, root),
        _ => (),
    }
    if let SizeSpec::Dynamic(terms) = &row.size {
        size(w, terms);
    }
    w.flat_vec(&row.fields);
    if let Some(tag) = &row.tag {
        w.vec(&tag.variants, |w, v| w.flat_vec(&v.fields));
    }
    scan(w, &row.scan);
    drop_steps(w, &row.drop);
    if let Some(finalizer) = &row.finalizer {
        w.path(&finalizer.definition);
    }
    w.flat_vec(&row.storage);
    w.flat_vec(&row.abi.scalars);
}
pub fn binding(w: &mut W, value: &Binding) {
    // Each canonical index row names a distinct row in the bound table.
    w.tree::<hirsel_syntax::ContentHash, LayoutId>(value.table.layouts.len());
    w.tree::<hirsel_syntax::ContentHash, hirsel_lir::abi_types::FnAbiId>(value.table.fn_abis.len());
    table(w, &value.table);
}
pub fn table(w: &mut W, value: &LayoutTable) {
    w.vec(&value.layouts, layout);
    w.vec(&value.fn_abis, |w, abi| {
        w.vec(&abi.parameters, |w, p| w.flat_vec(&p.slots));
        if let Some(r) = &abi.result {
            w.flat_vec(&r.slots);
            w.flat_vec(&r.borrowed_parameters);
        }
        w.vec(&abi.nonretaining, |w, p| w.flat_vec(&p.fields));
        w.vec(&abi.nonretaining_captures, |w, p| w.flat_vec(&p.fields));
        w.vec(&abi.scoped_entries, |w, p| w.flat_vec(&p.parameter.fields));
        if let Some(r) = &abi.receipts {
            w.flat_vec(&r.parameters);
            w.flat_vec(&r.captures);
            w.flat_vec(&r.inventory);
        }
    });
}
fn body(w: &mut W, value: &ScalarBody) {
    w.tree::<hirsel_syntax::ContentHash, ()>(value.calls.len());
    w.flat_vec(&value.checkpoint_points);
}
fn graph(w: &mut W, value: &ScalarGraph) {
    w.map(&value.bodies, |w, _, b| body(w, b));
}
fn owners(w: &mut W, value: &LiveOwners) {
    w.flat_vec(&value.slots);
    w.flat_vec(&value.values);
    w.flat_vec(&value.loans);
}
fn spine(w: &mut W, value: &Spine) {
    w.flat_vec(&value.private_instructions);
    w.tree::<hirsel_lir::ir::CallId, ()>(value.scalar_calls.len());
    w.tree::<hirsel_lir::ir::CallId, ()>(value.reached_calls.len());
    w.map(&value.suspended, |w, _, v| owners(w, v));
    w.tree::<usize, ()>(value.barriers.len());
    w.map(&value.deferred_releases, |_, _, _| ());
}
pub fn ahead(w: &mut W, value: &AheadPlan) {
    w.map(&value.bodies, |w, _, b| body(w, b));
    w.map(&value.symbols, |w, _, s| symbol(w, s));
    w.map(&value.spawns, |_, _, _| ());
    w.map(&value.cpu_workers, |_, _, _| ());
    w.map(&value.refusals, |w, _, s| w.add(s.capacity()));
    w.map(&value.roots, |w, _, root| {
        graph(w, &root.graph);
        symbol(w, &root.recipe.source_code);
        if let Some(host) = &root.host {
            graph(w, &host.graph);
            w.map(&host.spines, |w, _, s| spine(w, s));
            w.map(&host.reads, |w, _, r| {
                w.flat_vec(&r.slots);
                w.flat_vec(&r.values);
                w.flat_vec(&r.loans);
            });
        }
        if let Some(cpu) = &root.cpu {
            symbol(w, &cpu.binding.callback);
            spine(w, &cpu.spine);
            if let Some(mapper) = &cpu.mapper {
                graph(w, &mapper.graph);
                w.map(&mapper.spines, |w, _, s| spine(w, s));
                w.map(&mapper.growth, |_, _, _| ());
                w.map(&mapper.regional, |_, _, _| ());
                w.map(&mapper.allocations, |_, _, _| ());
                w.tree::<(hirsel_syntax::ContentHash, usize), ()>(mapper.cleanup.len());
            }
            if let Some(summary) = &cpu.summary {
                scan(w, &summary.owners);
            }
            if let Some(roots) = &cpu.roots {
                w.map(&roots.frames, |w, _, frames| {
                    w.map(frames, |w, _, v| owners(w, v))
                });
                w.map(&roots.parents, |_, _, _| ());
            }
        }
    });
}
pub fn continuations(w: &mut W, value: &ContinuationPlans) {
    w.map(&value.plans, |w, _, plan| {
        if let Representation::Stackful(reason) = &plan.representation {
            w.add(reason.capacity());
        }
        w.flat_vec(&plan.parameters);
        w.flat_vec(&plan.records);
        w.flat_vec(&plan.storage);
        w.vec(&plan.states, |w, state| {
            w.path(&state.boundary.operation);
            if let Some(runtime) = &state.runtime {
                w.add(runtime.helper.capacity());
                w.flat_vec(&runtime.results);
            }
            w.flat_vec(&state.live);
            w.flat_vec(&state.storage);
            w.flat_vec(&state.loans);
            w.flat_vec(&state.cleanup.anchors);
            w.flat_vec(&state.regions);
        });
        w.vec(&plan.cleanups, |w, cleanup| {
            w.flat_vec(&cleanup.live);
            w.flat_vec(&cleanup.storage);
            if let Some(coverage) = &cleanup.finalizers {
                w.vec(&coverage.layouts, |w, (_, row)| layout(w, row));
                w.flat_vec(&coverage.finalizers);
                w.flat_vec(&coverage.synchronous);
            }
        });
        if let Some(isolate) = &plan.isolate {
            symbol(w, &isolate.protocol.identity.target);
            w.flat_vec(&isolate.protocol.cleanup);
            w.flat_vec(&isolate.protocol.ends);
            w.flat_vec(&isolate.protocol.terminals);
            w.flat_vec(&isolate.cleanup.sites);
            w.flat_vec(&isolate.cleanup.finalizers);
            w.flat_vec(&isolate.callback_bodies);
        }
    });
}
