use super::*;
use hirsel_intern::jobs::with_compiler_stack;

fn prove(source: &str) -> (usize, BTreeMap<String, Vec<ParamId>>) {
    let mut adapters = crate::compiler_adapters();
    adapters.modes = None;
    let mut engine = QueryEngine::new(default_environment(), adapters);
    let file = engine
        .sources
        .add("nonretention-work.hir", source.into())
        .unwrap();
    let parsed = engine.parse_file(file, &default_context()).unwrap();
    let bodies: Vec<_> = parsed
        .value
        .items
        .iter()
        .map(|syntax| {
            let path = &syntax.item.definition;
            let signature = engine.item_signature(path).unwrap().value;
            let body = Arc::new(engine.check_definition(path).unwrap().value);
            (body, signature)
        })
        .collect();
    let mut headers: BTreeMap<_, _> = bodies
        .iter()
        .map(|(_, signature)| {
            let mut header = (**signature).clone();
            header.metadata.inferred_unique.nonretaining.clear();
            (header.definition, Arc::new(header))
        })
        .collect();
    PROOFS.with(|count| count.set(0));
    infer(&engine.types().clone(), &bodies, &mut headers, &mut engine).unwrap();
    (
        PROOFS.with(|count| count.get()),
        headers
            .into_iter()
            .map(|(id, header)| {
                (
                    id.path().0.name.clone(),
                    header.metadata.inferred_unique.nonretaining.clone(),
                )
            })
            .collect(),
    )
}

#[test]
fn observer_chain_proves_each_parameter_once_in_any_declaration_order() {
    with_compiler_stack(|| {
        let n = 20;
        let functions: Vec<_> = (0..n)
            .map(|i| {
                let expression = if i + 1 == n {
                    "xs[0]".into()
                } else {
                    format!("observer{}(xs)", i + 1)
                };
                format!("fn observer{i}(xs: Bytes) -> u8 {{ {expression} }}\n")
            })
            .collect();
        let expected: BTreeMap<_, _> = (0..n)
            .map(|i| (format!("observer{i}"), vec![ParamId(0)]))
            .collect();
        for order in [
            (0..n).collect::<Vec<_>>(),
            (0..n).map(|i| i * 7 % n).collect(),
        ] {
            let source: String = order.into_iter().map(|i| functions[i].as_str()).collect();
            let (proofs, bits) = prove(&source);
            assert_eq!(bits, expected);
            assert_eq!(proofs, n, "a chain needs one proof per borrowed parameter");
        }
    });
}

#[test]
fn cycles_and_indirect_calls_cannot_bootstrap_observer_bits() {
    with_compiler_stack(|| {
        let (_, bits) = prove(
            "fn left(xs: Bytes) -> u8 { right(xs) }\nfn right(xs: Bytes) -> u8 { left(xs) }\nfn recursive(xs: Bytes) -> u8 { recursive(xs) }\nfn caller(xs: Bytes) -> u8 { left(xs) }\nfn indirect(xs: Bytes, f: fn(Bytes) -> u8) -> u8 { f(xs) }\nfn leaf(xs: Bytes) -> u8 { xs[0] }\n",
        );
        for name in ["left", "right", "recursive", "caller", "indirect"] {
            assert_eq!(bits[name], [], "{name}");
        }
        assert_eq!(bits["leaf"], [ParamId(0)]);
    });
}
