//! Checked ordered-key shapes and their codec makers.
use super::codec_glue::{CodecOp, Format, Part, Rows};
use super::*;
use crate::TypeDecl;

pub(crate) fn is_key_of(path: &DefPath) -> bool {
    path.package_is("std", "1") && path.0.modules == ["codec"] && path.0.name == "key_of"
}

pub(super) fn is_desc(path: &DefPath) -> bool {
    path.package_is("std", "1") && path.0.modules == ["codec"] && path.0.name == "Desc"
}

pub(super) fn is_desc_row(name: &str) -> bool {
    name.starts_with("std@1/codec/Desc[")
}

const BUILTINS: &[(&str, &str)] = &[
    ("()", "key_unit"),
    ("bool", "key_bool"),
    ("u8", "key_u8"),
    ("u16", "key_u16"),
    ("u32", "key_u32"),
    ("u64", "key_u64"),
    ("i8", "key_i8"),
    ("i16", "key_i16"),
    ("i32", "key_i32"),
    ("i64", "key_i64"),
    ("core@1/core/String", "key_string"),
    ("core@1/core/List[u8]", "key_bytes"),
];

pub(crate) const MAKERS: [&str; BUILTINS.len()] = {
    let mut names = [""; BUILTINS.len()];
    let mut i = 0;
    while i < names.len() {
        names[i] = BUILTINS[i].1;
        i += 1;
    }
    names
};

fn builtin(name: &str) -> Option<&'static str> {
    BUILTINS
        .iter()
        .find_map(|&(row, maker)| (row == name).then_some(maker))
}

pub(super) fn available(module: &Module, ty: TypeId) -> bool {
    let row = &module.types[ty.index()];
    row.key_codec.is_some() || builtin(&row.name).is_some()
}

pub(super) fn maker(
    module: &mut Module,
    key_of: &DefPath,
    ty: TypeId,
    signature: SigId,
) -> Result<FuncId, String> {
    let row = &module.types[ty.index()];
    let Some(maker) = builtin(&row.name) else {
        return row
            .key_codec
            .map(|f| FuncId(f.0))
            .ok_or_else(|| format!("LANG-12: {} has no ordered key codec", row.name));
    };
    let mut definition = key_of.clone();
    definition.0.name = maker.into();
    Ok(templates::codec_maker_row(module, definition, signature))
}

fn parts(
    decls: &crate::Rows<TypeDecl>,
    t: &mut impl TyRead,
    ty: TyId,
) -> Option<(Format, Vec<TyId>)> {
    match t.kind(ty).clone() {
        TyKind::Tuple(parts) => Some((Format::Key, parts)),
        TyKind::Adt(d, args) => {
            let decl = &decls[d.index()];
            if is_desc(&decl.path) {
                return Some((Format::Desc, args));
            }
            if decl.role.is_some()
                || decl.declared & crate::decls::KEY == 0
                || decl
                    .codec()
                    .is_some_and(|c| c.defaulted_fields || !c.removed.is_empty())
            {
                return None;
            }
            let DeclBody::Record(fields) = &decl.body else {
                return None;
            };
            let parts = fields.iter().map(|f| subst(t, f.ty, &args)).collect();
            Some((Format::Key, parts))
        }
        _ => None,
    }
}

pub(super) fn admitted(decls: &crate::Rows<TypeDecl>, t: &mut impl TyRead, ty: TyId) -> bool {
    match t.kind(ty) {
        TyKind::Param(_) | TyKind::Unit => true,
        TyKind::Scalar(s) => builtin(s.name()).is_some(),
        TyKind::Adt(d, _) if decls[d.index()].role == Some(crate::decls::Role::String) => true,
        TyKind::Adt(d, args) if decls[d.index()].role == Some(crate::decls::Role::List) => {
            args.len() == 1
                && matches!(
                    t.kind(args[0]),
                    TyKind::Param(_) | TyKind::Scalar(Scalar::U8)
                )
        }
        _ => parts(decls, t, ty)
            .is_some_and(|(_, parts)| parts.into_iter().all(|part| admitted(decls, t, part))),
    }
}

pub(crate) fn composite(decls: &crate::Rows<TypeDecl>, t: &mut impl TyRead, ty: TyId) -> bool {
    parts(decls, t, ty)
        .is_some_and(|(_, parts)| parts.into_iter().all(|part| admitted(decls, t, part)))
}

impl Emitter {
    pub(super) fn ordered_codec_rows(
        &mut self,
        p: &Program,
        t: &mut impl TyRead,
        ty: TyId,
        row: TypeId,
    ) -> Option<(DefPath, Rows)> {
        let (format, components) = parts(&p.decls, t, ty)?;
        if !components.iter().all(|&part| admitted(&p.decls, t, part)) {
            return None;
        }
        let protocol = p.ordered_codec_protocol()?;
        let key = subst(t, protocol.codec, &[ty]);
        let decoded = subst(t, protocol.decoded, &[ty]);
        let mut parts = Vec::with_capacity(components.len());
        for component in components {
            let key = subst(t, protocol.codec, &[component]);
            let decoded = subst(t, protocol.decoded, &[component]);
            parts.push(Part {
                ty: self.type_row(p, t, component)?,
                codec: self.type_row(p, t, key)?,
                decoded: self.type_row(p, t, decoded)?,
            });
        }
        Some((
            protocol.of.clone(),
            Rows {
                format,
                tuple: row,
                codec: self.type_row(p, t, key)?,
                decoded: self.type_row(p, t, decoded)?,
                written: self.type_row(p, t, protocol.written)?,
                bytes: self.type_row(p, t, protocol.bytes)?,
                schema: self.type_row(p, t, protocol.schema)?,
                parts,
            },
        ))
    }

    pub(super) fn ordered_codec(
        &mut self,
        p: &Program,
        t: &mut impl TyRead,
        ty: TyId,
        row: TypeId,
    ) {
        let Some((of, rows)) = self.ordered_codec_rows(p, t, ty, row) else {
            return;
        };
        let maker = codec_glue::row(&mut self.module, &of, &rows, CodecOp::Of);
        std::sync::Arc::make_mut(&mut self.module.types[row.index()]).key_codec =
            Some(hirsel_core_layout::FuncRef(maker.0));
    }
}
