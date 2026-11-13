use super::*;

// A cursor ranks mechanical pointer fields. Inline arrays reserve a rank range
// per element; variant cases share a range. No heap depth enters generated code.
pub(super) enum Plan {
    Empty,
    Leaf(FieldTy),
    Fields(Vec<(u64, Plan)>),
    Variant(hirsel_core_layout::Tag, Vec<Plan>),
    Array(u64, u64, Box<Plan>),
}
impl Plan {
    fn count(&self) -> u64 {
        match self {
            Self::Empty => 0,
            Self::Leaf(_) => 1,
            Self::Fields(fs) => fs.iter().map(|(_, p)| p.count()).sum(),
            Self::Variant(_, cs) => cs.iter().map(Self::count).max().unwrap_or(0),
            Self::Array(n, _, p) => n * p.count(),
        }
    }
}
pub(super) fn plan_type(tables: &Tables<'_>, t: TypeRef) -> Result<Plan> {
    let Some(d) = &tables.layouts[t.index()].drop else {
        return Ok(Plan::Empty);
    };
    let Layout::Fixed(f) = &tables.layouts[t.index()].layout else {
        return Ok(Plan::Empty);
    };
    let rows = d
        .fields
        .iter()
        .map(|fs| {
            fs.iter()
                .map(|f| Ok((f.offset, plan_field(tables, &f.ty)?)))
                .collect::<Result<Vec<_>>>()
                .map(Plan::Fields)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(if let Some(tag) = f.tag {
        Plan::Variant(tag, rows)
    } else {
        rows.into_iter().next().unwrap()
    })
}
fn plan_field(tables: &Tables<'_>, t: &FieldTy) -> Result<Plan> {
    Ok(match t {
        FieldTy::Inline(t) => plan_type(tables, *t)?,
        FieldTy::Box(_) | FieldTy::Rc(_) | FieldTy::Arc(_) => Plan::Leaf(t.clone()),
        FieldTy::Array(e, n) => Plan::Array(
            *n,
            field_size(&tables.input.types, tables.layouts, e).0,
            Box::new(plan_field(tables, e)?),
        ),
        other => return Err(unsupported(format!("resumable counted field {other:?}"))),
    })
}
impl Glue<'_, '_, '_> {
    pub(super) fn resume(
        &mut self,
        plan: &Plan,
        base: Value,
        rank: Value,
        cursor: Value,
        slot: Value,
        list: Value,
    ) -> Result<()> {
        match plan {
            Plan::Empty => {}
            Plan::Leaf(field) => {
                let skip = self.b.create_block();
                let run = self.b.create_block();
                let past = self.b.ins().icmp(IntCC::UnsignedGreaterThan, cursor, rank);
                self.b.ins().brif(past, skip, &[], run, &[]);
                self.b.switch_to_block(run);
                let next = self.b.ins().iadd_imm_s(rank, 1);
                self.b.ins().store(MemFlagsData::trusted(), next, slot, 0);
                let node = self.load(base, 0);
                match field {
                    FieldTy::Box(t) => {
                        let charge = self.icon(
                            charge::boxed(
                                field_size(
                                    &self.tables.input.types,
                                    self.tables.layouts,
                                    &FieldTy::Inline(*t),
                                )
                                .2,
                            )
                            .unwrap(),
                        );
                        let recipe = self.callback(*t);
                        self.call(
                            "hirsel_core_release_own_enqueue",
                            &[P, P, U, P],
                            &[],
                            &[list, node, charge, recipe],
                        )?;
                    }
                    FieldTy::Rc(t) | FieldTy::Arc(t) => {
                        let atomic = self
                            .b
                            .ins()
                            .iconst(types::I8, i64::from(matches!(field, FieldTy::Arc(_))));
                        let charge = self.icon(
                            charge::counted(self.tables.layouts[t.index()].logical_size.unwrap())
                                .unwrap(),
                        );
                        let recipe = self.callback(*t);
                        self.call(
                            "hirsel_core_release_enqueue",
                            &[P, P, B, U, P],
                            &[],
                            &[list, node, atomic, charge, recipe],
                        )?;
                    }
                    _ => unreachable!(),
                }
                let no = self.b.ins().iconst(types::I8, 0);
                self.b.ins().return_(&[no]);
                self.b.switch_to_block(skip);
            }
            Plan::Fields(fields) => {
                let mut off = 0;
                for (offset, p) in fields {
                    let at = self.at(base, *offset);
                    let r = self.at(rank, off);
                    self.resume(p, at, r, cursor, slot, list)?;
                    off += p.count();
                }
            }
            Plan::Variant(tag, cases) => {
                let native = match tag.width {
                    1 => types::I8,
                    2 => types::I16,
                    _ => types::I32,
                };
                let v = self
                    .b
                    .ins()
                    .load(native, MemFlagsData::trusted(), base, tag.offset as i32);
                let done = self.b.create_block();
                for (i, p) in cases.iter().enumerate() {
                    let yes = self.b.create_block();
                    let no = self.b.create_block();
                    let is = self.b.ins().icmp_imm_s(IntCC::Equal, v, i as i64);
                    self.b.ins().brif(is, yes, &[], no, &[]);
                    self.b.switch_to_block(yes);
                    self.resume(p, base, rank, cursor, slot, list)?;
                    self.b.ins().jump(done, &[]);
                    self.b.switch_to_block(no);
                }
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
            }
            Plan::Array(n, stride, p) if p.count() != 0 => {
                let delta = self.b.ins().isub(cursor, rank);
                let past = self.b.ins().icmp(IntCC::UnsignedGreaterThan, cursor, rank);
                let zero = self.icon(0);
                let delta = self.b.ins().select(past, delta, zero);
                let count = self.icon(p.count());
                let first = self.b.ins().udiv(delta, count);
                let head = self.b.create_block();
                let body = self.b.create_block();
                let done = self.b.create_block();
                self.b.append_block_param(head, types::I64);
                self.b.ins().jump(head, &[first.into()]);
                self.b.switch_to_block(head);
                let i = self.b.block_params(head)[0];
                let more = self
                    .b
                    .ins()
                    .icmp_imm_s(IntCC::UnsignedLessThan, i, *n as i64);
                self.b.ins().brif(more, body, &[], done, &[]);
                self.b.switch_to_block(body);
                let off = self.b.ins().imul_imm_s(i, *stride as i64);
                let at = self.b.ins().iadd(base, off);
                let off = self.b.ins().imul_imm_s(i, p.count() as i64);
                let r = self.b.ins().iadd(rank, off);
                self.resume(p, at, r, cursor, slot, list)?;
                let next = self.b.ins().iadd_imm_s(i, 1);
                self.b.ins().jump(head, &[next.into()]);
                self.b.switch_to_block(done);
            }
            Plan::Array(..) => {}
        }
        Ok(())
    }
}
