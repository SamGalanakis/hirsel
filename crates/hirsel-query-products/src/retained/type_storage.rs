use hirsel_syntax::{ContentHash, DefPath};
use hirsel_types::*;
use std::sync::Arc;

fn function(width: usize, ty: TypeId, path: &DefPath) -> FunctionType {
    FunctionType {
        state_promises: vec![
            StatePromise {
                selector: RelationPlace {
                    parameter: ParameterRef::bound(ParamId(0)),
                    fields: (0..width).map(|i| FieldId(i as u32)).collect(),
                },
                advance_only: false,
                preserve_captures: false,
                preserve_unique: false,
                unretained: vec![ParamId(0); width].into(),
            };
            width
        ]
        .into(),
        generics: (0..width)
            .map(|i| GenericParameter::Type {
                name: format!("parameter{i}"),
                bounds: vec![Bound::Copyable; width].into(),
            })
            .collect::<Vec<_>>()
            .into(),
        parameters: (0..width)
            .map(|i| Parameter {
                name: Some(format!("name{i}")),
                label: Some(format!("label{i}")),
                passing: hirsel_syntax::PassingMode::Borrow,
                ty,
            })
            .collect::<Vec<_>>()
            .into(),
        result: ty,
        error: None,
        allocation: Allocation::NoAlloc,
        authority: AuthoritySummary::Known(
            vec![AuthorityTerm::Capability(path.clone()); width].into(),
        ),
        uses: Uses::Only(AuthoritySummary::Known(
            vec![AuthorityTerm::Capability(path.clone()); width].into(),
        )),
        contracts: vec![
            Contract {
                kind: hirsel_syntax::ContractKind::Requires,
                ordinal: 0,
                code_hash: ContentHash::default()
            };
            width
        ]
        .into(),
        incomplete: false,
        foreign: Some(ForeignContract {
            abi: "C".repeat(width),
            blocking: false,
            aliasing_contract: "retained contract".repeat(width),
        }),
    }
}
