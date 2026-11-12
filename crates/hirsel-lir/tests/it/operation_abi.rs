//! Hand-built calls, independent of RC/import. Every mutation is otherwise valid
//! and validates when the operation-declaration ABI check is removed.
use super::support::{self, ENTRY, Lir};
use hirsel_core_contract::{Manifest, OperationId, Passing};
use hirsel_layout::layout::{OwnerRepresentation, Owners};
use hirsel_lir::abi_types::FnAbiId;
use hirsel_lir::ir::*;

use hirsel_layout::operation::OperationRegistry;
use hirsel_lir::layout_types::*;
use hirsel_lir::operation::{NativeType, OperationArgument as A, OperationDeclarations};
use hirsel_lir::stage::{Stage, Value};
use hirsel_lir::validate::{Inputs, ViolationKind as K};
use hirsel_syntax::{ContentHash, DefId, DefPath, ItemPath, PackageId, PassingMode};
use hirsel_tir::RepresentationRecord;
use hirsel_types::*;
use std::sync::Arc;

fn path(name: &str) -> DefPath {
    let mut parts: Vec<_> = name.split('.').map(str::to_owned).collect();
    DefPath(ItemPath {
        package: PackageId {
            name: "core".into(),
            version: "1".into(),
        },
        name: parts.pop().unwrap(),
        modules: parts,
    })
}

fn ty(store: &TypeInterner, kind: TypeKind) -> TypeId {
    store.intern(Type {
        kind,
        ..Type::unit()
    })
}

fn function(
    parameters: Vec<Parameter>,
    result: TypeId,
    generics: Vec<GenericParameter>,
) -> FunctionType {
    FunctionType {
        state_promises: Default::default(),
        parameters: parameters.into(),
        result,
        generics: generics.into(),
        error: None,
        allocation: Allocation::NoAlloc,
        authority: AuthoritySummary::Unknown,
        uses: Uses::Unrestricted,
        contracts: Default::default(),
        incomplete: false,
        foreign: None,
    }
}

pub(super) fn signature(name: &str, types: &TypeInterner, f: FunctionType) -> SignatureRef {
    let mut s = Signature {
        metadata: Default::default(),
        definition: DefId::intern(&path(name)),
        kind: SignatureKind::Function(f),
        fingerprint: Default::default(),
    };
    s.fingerprint = encoding::fingerprint(types, &s, &[]).unwrap();
    Arc::new(s)
}

fn parameter(ty: TypeId, passing: PassingMode) -> Parameter {
    Parameter {
        name: None,
        label: None,
        ty,
        passing,
    }
}

pub(super) fn scalar_signature(
    name: &str,
    params: &[(PassingMode, Primitive)],
    result: Option<Primitive>,
) -> SignatureRef {
    let store = support::session();
    let parameters = params
        .iter()
        .map(|(passing, p)| {
            let t = ty(&store, TypeKind::Primitive(*p));
            parameter(t, *passing)
        })
        .collect();
    let result = match result {
        Some(p) => ty(&store, TypeKind::Primitive(p)),
        None => store.intern(Type::unit()),
    };
    signature(name, &store, function(parameters, result, vec![]))
}

fn nominal(owners: &mut Owners, name: &str, generics: Vec<GenericParameter>, shape: TypeId) {
    owners.nominals.insert(
        DefId::intern(&path(name)),
        OwnerRepresentation::Defined(RepresentationRecord {
            definition: path(name),
            representation_revision: ContentHash::of_bytes(name.as_bytes()),
            generics,
            private_shape: shape,
            layout_promises: Layout::default(),
            resource: false,
            finalizer: None,
            unsafe_ffi_package: false,
        }),
    );
}

pub(super) fn ready_signature(result: bool) -> (SignatureRef, Owners) {
    if !result {
        return (scalar_signature("fixture", &[], None), Owners::default());
    }
    let mut owners = Owners::default();
    let store = support::session();
    let shape = ty(&store, TypeKind::Primitive(Primitive::I64));
    nominal(&mut owners, "ResultCell", vec![], shape);
    let store = support::session();
    let result = ty(
        &store,
        TypeKind::Named {
            definition: DefId::intern(&path("ResultCell")),
            arguments: Default::default(),
        },
    );
    (
        signature("fixture", &store, function(vec![], result, vec![])),
        owners,
    )
}
