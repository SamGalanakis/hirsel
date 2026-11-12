//! Stable identity of an entire signature and its referenced type rows.
use crate::{Signature, TypeInterner};
use hirsel_syntax::ContentHash;

/// Encodes signature fields and reachable types with local name and type numbering.
pub fn signature_identity(t: &TypeInterner, signature: &Signature) -> ContentHash {
    let mut definitions = hirsel_intern::persist::Numbering::default();
    let mut symbols = hirsel_intern::persist::Numbering::default();
    let payload = hirsel_syntax::with_def_numbering(&mut definitions, || {
        hirsel_intern::persist::with_symbol_numbering(&mut symbols, || encoded_view(t, signature))
    });
    let paths: Vec<_> = definitions.ids().iter().map(|d| d.path()).collect();
    let spellings: Vec<_> = symbols.ids().iter().map(|s| s.as_str()).collect();
    let names = bincode::serialize(&(paths, spellings)).expect("signature names serialize");
    ContentHash::of_parts(
        "hirsel.validate.view.v3",
        [names.as_slice(), payload.as_slice()],
    )
}

fn encoded_view(t: &TypeInterner, signature: &Signature) -> Vec<u8> {
    let (encoded, roots) =
        crate::with_table_collection(|| bincode::serialize_into(std::io::sink(), signature));
    encoded.expect("signature fields serialize in a type collection scope");
    let mut positions = std::collections::HashMap::new();
    let mut order = Vec::new();
    let mut pending = roots;
    pending.reverse();
    while let Some(id) = pending.pop() {
        let next = order.len() as u32;
        let std::collections::hash_map::Entry::Vacant(entry) = positions.entry(id) else {
            continue;
        };
        entry.insert(next);
        order.push(id);
        if let Some(row) = t.get(id) {
            let (encoded, edges) =
                crate::with_table_collection(|| bincode::serialize_into(std::io::sink(), row));
            encoded.expect("type fields serialize in a type collection scope");
            pending.extend(edges.into_iter().rev());
        }
    }
    let rows: Vec<_> = order.iter().map(|id| t.get(*id)).collect();
    crate::with_type_positions(positions, || bincode::serialize(&(signature, rows)))
        .expect("all referenced type IDs have positions")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Primitive, SignatureKind, SignatureMetadata, Type, TypeKind};
    use hirsel_syntax::{DefId, DefPath};
    fn id(name: &str) -> DefId {
        DefId::intern(&DefPath::core(name))
    }
    #[test]
    fn view_digests_ignore_interner_numbers_but_preserve_literal_type_id_text() {
        let signature = |definition, ty| Signature {
            definition,
            metadata: SignatureMetadata::default(),
            kind: SignatureKind::Const {
                ty,
                generics: vec![].into(),
            },
            fingerprint: ContentHash::default(),
        };
        let first = TypeInterner::new(None);
        let second = TypeInterner::new(None);
        second.intern(Type::with_kind(TypeKind::Primitive(Primitive::String)));
        let left = first.intern(Type::with_kind(TypeKind::Primitive(Primitive::U64)));
        let right = second.intern(Type::with_kind(TypeKind::Primitive(Primitive::U64)));
        assert_ne!(left, right);
        assert_eq!(
            signature_identity(&first, &signature(id("Data"), left)),
            signature_identity(&second, &signature(id("Data"), right)),
        );
        let mut definitions = hirsel_intern::persist::Numbering::default();
        definitions.number(id("Unrelated"));
        let mut symbols = hirsel_intern::persist::Numbering::default();
        symbols.number("unrelated".into());
        let row = signature(id("Data"), left);
        let expected = signature_identity(&first, &row);
        assert_eq!(
            expected,
            hirsel_syntax::with_def_numbering(&mut definitions, || {
                hirsel_intern::persist::with_symbol_numbering(&mut symbols, || {
                    signature_identity(&first, &row)
                })
            }),
            "ambient product numbering cannot change signature identity",
        );
        assert_eq!(definitions.ids().len(), 1);
        assert_eq!(symbols.ids().len(), 1);
        let literal = format!("{left:?}");
        assert_ne!(
            signature_identity(&first, &signature(id(&literal), left)),
            signature_identity(&first, &signature(id("TypeId(#0)"), left)),
            "a declaration name is text, never a type-table reference",
        );
    }
}
