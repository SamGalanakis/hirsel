//! A data witness captures one concrete construction, including native field
//! offsets and niche tags. No function from an executable codec is consulted.
use super::*;
use crate::FieldDecl;
use hirsel_data::guest::{Construction, Field, Node, Plan};

impl Cx<'_, '_, '_> {
    pub(super) fn data_witness(
        &mut self,
        root: TyId,
        alias: NodeId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let TyKind::Adt(decl, _) = self.t.kind(root) else {
            self.refuse(
                "LANG-12",
                n,
                "a data witness requires a concrete record type",
            );
            return None;
        };
        let path = &self.p.decls[decl.index()].path;
        if self.token_kind(alias) != Some(TokenKind::String) {
            self.refuse(
                "LANG-12",
                alias,
                "a data witness requires a literal module alias",
            );
            return None;
        }
        let alias = String::from_utf8(hirsel_core_syntax::quoted_bytes(
            self.token_text(alias),
            false,
        )?)
        .ok()?;
        let schema = match self.p.data_descriptor(path, &alias) {
            Ok(schema) => schema,
            Err(error) => {
                self.refuse(
                    "LANG-12",
                    n,
                    format!("cannot build a data witness for {path}: {error}"),
                );
                return None;
            }
        };
        let mut decoder = self.p.decls[match self.t.kind(result) {
            TyKind::Adt(id, _) => id.index(),
            _ => return None,
        }]
        .path
        .clone();
        decoder.0.name = "decode".into();
        let output = self.p.fns[self.p.function_id(&decoder)?].result;
        let output = subst(&mut self.t, output, &[root]);
        let mut nodes = Vec::new();
        let result_row = self.data_node(output, &mut nodes, &mut HashMap::default(), n)?;
        let bytes = Plan {
            nodes,
            result: result_row,
        }
        .witness(&schema);
        let dest = self.runtime_type(result, n)?;
        let dest = self.new_slot(PlaceTy::Declared(dest), n);
        let string = self.p.core_decl("String")?;
        let string = self.t.mk(TyKind::Adt(string, vec![]));
        let value = self.static_string(&bytes, string, true, n)?;
        let rep = self.rep_at(string, n)?;
        let field = self.runtime_field(dest, &[Step::Field(0)], string, n)?;
        self.install(field, rep, Some(value), n);
        let ptr = self.pointer(dest, n);
        self.temp(result, Some(ptr));
        Some(ptr)
    }

    fn data_node(
        &mut self,
        ty: TyId,
        nodes: &mut Vec<Node>,
        seen: &mut HashMap<TyId, usize>,
        n: NodeId,
    ) -> Option<usize> {
        if let Some(&row) = seen.get(&ty) {
            return Some(row);
        }
        let row = self.runtime_type(ty, n)?;
        let recipe = self.e.recipe(self.p, row, self.e.module.types.len())?;
        let hirsel_core_layout::Layout::Fixed(layout) = &recipe.layout else {
            self.refuse(
                "LANG-12",
                n,
                "a data witness requires an owned construction layout",
            );
            return None;
        };
        let layout = layout.clone();
        let idx = nodes.len();
        seen.insert(ty, idx);
        nodes.push(Node {
            size: layout.size,
            align: layout.align,
            logical: recipe.logical_size?,
            body: Construction::Unit,
        });
        let body = match self.t.kind(ty).clone() {
            TyKind::Unit => Construction::Unit,
            TyKind::Scalar(s) => Construction::Scalar(hirsel_data::Scalar::parse(s.name())?),
            TyKind::Tuple(items) => {
                let mut fields = vec![];
                for (i, item) in items.into_iter().enumerate() {
                    fields.push(Field {
                        offset: layout.offsets[0][i],
                        name: i.to_string(),
                        node: self.data_node(item, nodes, seen, n)?,
                    });
                }
                Construction::Record(fields)
            }
            TyKind::Adt(d, args) => {
                let decl = &self.p.decls[d.index()];
                match decl.role {
                    Some(Role::String) => Construction::Text,
                    Some(Role::List) => {
                        let item = self.data_node(args[0], nodes, seen, n)?;
                        let edge = self.e.field_ty(self.p, &mut self.t, ty)?;
                        let graph = !hirsel_core_layout::terminal(
                            &self.e.module.types,
                            &self.e.recipes,
                            &edge,
                        );
                        Construction::List { item, graph }
                    }
                    Some(Role::Map) => {
                        let key = self.e.field_ty(self.p, &mut self.t, args[0])?;
                        let value = self.e.field_ty(self.p, &mut self.t, args[1])?;
                        let item = self.data_node(args[1], nodes, seen, n)?;
                        let entry = hirsel_core_layout::map_entry(
                            &self.e.module.types,
                            &self.e.recipes,
                            &key,
                            &value,
                        );
                        Construction::Map {
                            item,
                            stride: entry.stride,
                            align: entry.align,
                            meta: entry.meta,
                            key: entry.key,
                            value: entry.value,
                        }
                    }
                    None => {
                        let mut fields = |row: usize, decls: &[FieldDecl]| -> Option<Vec<Field>> {
                            decls
                                .iter()
                                .enumerate()
                                .map(|(i, f)| {
                                    let part = subst(&mut self.t, f.ty, &args);
                                    Some(Field {
                                        offset: layout.offsets[row][i],
                                        name: f.name.clone(),
                                        node: self.data_node(part, nodes, seen, n)?,
                                    })
                                })
                                .collect()
                        };
                        match &decl.body {
                            DeclBody::Record(fs) => Construction::Record(fields(0, fs)?),
                            DeclBody::Variant(cases) => {
                                let cases = cases
                                    .iter()
                                    .enumerate()
                                    .map(|(i, c)| Some((c.name.clone(), fields(i, &c.fields)?)))
                                    .collect::<Option<_>>()?;
                                Construction::Sum {
                                    tag: layout.tag,
                                    cases,
                                }
                            }
                            DeclBody::Alias(_) => return None,
                        }
                    }
                    _ => return None,
                }
            }
            _ => return None,
        };
        nodes[idx].body = body;
        Some(idx)
    }

    pub(super) fn data_decode(
        &mut self,
        types: [TyId; 3],
        values: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let [w, input, limit_ty] = types;
        let witness = self.runtime_type(w, n)?;
        let bytes = self.runtime_type(input, n)?;
        let output = self.runtime_type(result, n)?;
        let view = |cx: &mut Self, val: Option<ValueId>, row| {
            Some(Arg::Address(cx.address(
                AddressBase::Value(val?),
                &[],
                PlaceTy::Declared(row),
                n,
            )))
        };
        let mut args = vec![
            view(self, values[0], witness)?,
            view(self, values[1], bytes)?,
        ];
        let limit_row = self.runtime_type(limit_ty, n)?;
        let limits = self.address(
            AddressBase::Value(values[2]?),
            &[],
            PlaceTy::Declared(limit_row),
            n,
        );
        let u = Ty::Scalar(Scalar::U64);
        for i in 0..7 {
            let at = self.address(
                AddressBase::Address(limits),
                &[Step::Field(i)],
                PlaceTy::Value(u),
                n,
            );
            args.push(Arg::Value(self.read_bits(at, u, n)));
        }
        let dest = self.new_slot(PlaceTy::Declared(output), n);
        args.push(Arg::Address(dest));
        self.runtime_row(
            hirsel_core_ir::RtOp::DataDecode {
                witness,
                bytes,
                output,
            },
            &args,
            n,
        )?;
        let ptr = self.pointer(dest, n);
        self.temp(result, Some(ptr));
        Some(ptr)
    }
}
