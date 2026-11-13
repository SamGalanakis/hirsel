//! Diagnostic runner: profile instructions and export semantic results for comparison.
use hirsel_core_driver::{CheckRequest, Driver, LibraryInput, Meter};
use hirsel_core_front::cost;
use hirsel_intern::instructions::InstructionCounter;
use serde_json::json;
use std::path::{Path, PathBuf};
hirsel_guard::install!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        return Err("usage: check_cost <inputs.json> <new-store> <output.json> [--profile] [--instances <entry>]".into());
    }
    if args.iter().any(|a| a == "--profile") {
        let counter = InstructionCounter::open()?;
        cost::start(move || counter.read())?;
    }
    let request = CheckRequest {
        inputs: serde_json::from_slice(&std::fs::read(&args[0])?)?,
        store: PathBuf::from(&args[1]),
        library: LibraryInput::Sources,
        entry: None,
    };
    let mut meter = Meter::new(true);
    let product = Driver::default().check(request, &mut meter)?;
    let instances = if let Some(at) = args.iter().position(|a| a == "--instances") {
        let entry =
            hirsel_core_ids::DefPath::from_full(args.get(at + 1).ok_or("missing instance entry")?)?;
        Some(product.instances(
            [hirsel_core_ir::InstanceKey {
                definition: entry,
                types: vec![],
                dictionaries: vec![],
            }],
            65_536,
        ))
    } else {
        None
    };
    let profile = cost::finish();
    if let Some(ref error) = profile.counter_refusal {
        return Err(error.clone().into());
    }
    let definitions: Vec<_> = product.definitions.values().map(|d| {
        let definition = product.index.definitions().find(|(id,_)| product.index.definition_path(*id) == d.definition).unwrap().1;
        let file = product.files.iter().find(|f| f.module == definition.site.module).unwrap();
        let item = file.syntax.item(definition.site.item as usize);
        let mut instance_refs = Vec::new();
        let mut instance_bodies = Vec::new();
        for function in &d.ir.functions {
            let key = &d.ir.instances[function.instance.index()];
            instance_refs.push(format!("{key:?}"));
            if function.body.is_some() { instance_bodies.push(format!("{key:?}")); }
        }
        let mut syntax_kinds = std::collections::BTreeMap::<String, u64>::new();
        for node in &item.tree.nodes[item.range.nodes.start as usize..item.range.nodes.end as usize] {
            *syntax_kinds.entry(format!("{:?}", node.kind)).or_default() += 1;
        }
        json!({
            "definition": d.definition.to_string(),
            "source": item.source_text(),
            "syntax_kinds":syntax_kinds,
            "complete": d.check.function.is_some() && d.check.gaps.is_empty() && d.check.refusals.is_empty(),
            "refusals": d.check.refusals,
            "gaps": d.check.gaps.iter().map(|g| json!({"what": g.what, "site":g.site})).collect::<Vec<_>>(),
            "nodes": d.check.counts.nodes,
            "calls": d.check.counts.calls,
            "places": d.check.counts.places,
            "types": d.check.counts.types,
            "ir_bytes": d.check.counts.bytes,
            "publication_payload_bytes": bincode::serialized_size(&(&d.ir, &d.check.template)).unwrap(),
            "template": d.check.template.is_some(),
            "key": {
                "body":d.key.body, "lookups":d.key.lookups,
                "signatures":d.key.signatures.iter().map(|(path,hash)| json!([path.to_string(),hash])).collect::<Vec<_>>(),
            },
            "instance_references": instance_refs,
            "instance_bodies": instance_bodies,
            "ir": hirsel_core_ir::text::print(&d.ir),
        })
    }).collect();
    std::fs::write(
        Path::new(&args[2]),
        serde_json::to_vec_pretty(&json!({
            "profile":profile, "metrics":meter.result, "definitions":definitions,
            "instances":instances.as_ref().map(|result| match result {
                Ok(keys) => json!({"keys":keys}),
                Err(stop) => json!({"stop":format!("{stop:?}")}),
            }),
            "diagnostics":product.machine_diagnostics()?, "incomplete":product.machine_incomplete()?,
        }))?,
    )?;
    Ok(())
}
