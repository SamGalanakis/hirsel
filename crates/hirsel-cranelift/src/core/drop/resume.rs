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
    pub(super) fn count(&self) -> u64 {
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
    // FORM v1c: a row with a finalizer, or a resource, is one step, cleaned whole.
    if d.finalizer.is_some() || tables.input.types[t.index()].repr == Repr::Resource {
        return Ok(Plan::Leaf(FieldTy::Inline(t)));
    }
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
        FieldTy::Seq(_) if hirsel_core_layout::storage_free(&tables.input.types, t) => Plan::Empty,
        FieldTy::Box(_) | FieldTy::Rc(_) | FieldTy::Arc(_) | FieldTy::Seq(_) | FieldTy::Map(..) => {
            Plan::Leaf(t.clone())
        }
        FieldTy::Scalar(_)
        | FieldTy::Stable(_)
        | FieldTy::Fn
        | FieldTy::Borrow
        | FieldTy::View(_)
        | FieldTy::Locator(_) => Plan::Empty,
        FieldTy::Array(e, n) => Plan::Array(
            *n,
            field_size(&tables.input.types, tables.layouts, e).0,
            Box::new(plan_field(tables, e)?),
        ),
        FieldTy::Handle(hirsel_core_layout::Handle::Capability) => Plan::Empty,
        // Cleaned whole by ordinary glue in one step that never yields.
        FieldTy::Handle(_) | FieldTy::OwnFn => Plan::Leaf(t.clone()),
        FieldTy::Text
        | FieldTy::FixedSpan(_)
        | FieldTy::SeqSpan(_)
        | FieldTy::IndexedSpan(_)
        | FieldTy::Link(_) => Plan::Empty,
    })
}
impl Glue<'_, '_, '_> {
    /// The next suspension point of fixed glue, or 0 in buffer glue.
    fn next_point(&mut self) -> Value {
        let k = self.point.as_mut().map_or(0, |n| {
            *n += 1;
            *n
        });
        self.icon(k)
    }
    /// Hand one descendant to the drain: written inline into the worklist
    /// (abi_layout WORKLIST_*), read by the drain when the recipe returns.
    fn stage(&mut self, list: Value, node: Value, descriptor: Value, point: Value) {
        let flags = MemFlagsData::trusted();
        self.b
            .ins()
            .store(flags, node, list, memory_layout::WORKLIST_NODE as i32);
        self.b.ins().store(
            flags,
            descriptor,
            list,
            memory_layout::WORKLIST_DESCRIPTOR as i32,
        );
        self.b
            .ins()
            .store(flags, point, list, memory_layout::WORKLIST_POINT as i32);
    }
    fn enqueue_buffer(
        &mut self,
        field: &FieldTy,
        carrier: Value,
        node: Value,
        logical: u64,
        list: Value,
    ) -> Result<()> {
        let done = self.b.create_block();
        let present = self.b.create_block();
        let cap = self.load(carrier, 16);
        let allocated = self.b.ins().icmp_imm_s(IntCC::NotEqual, cap, 0);
        self.b.ins().brif(allocated, present, &[], done, &[]);
        self.b.switch_to_block(present);
        let bytes = self.b.ins().imul_imm_s(cap, logical as i64);
        let charge = self.round8(bytes);
        if hirsel_core_layout::terminal(&self.tables.input.types, self.tables.layouts, field) {
            fast::free(
                &mut self.b,
                &mut self.calls,
                self.object,
                self.ctx,
                node,
                charge,
            )?;
        } else {
            let function = self
                .glues
                .buffers
                .iter()
                .find(|(t, _)| t == field)
                .unwrap()
                .1;
            let reference = self.calls.reference(self.object, &mut self.b, function);
            let recipe = self.b.ins().func_addr(types::I64, reference);
            let point = self.next_point();
            self.call(
                "hirsel_core_release_buffer_enqueue",
                &[P, P, U, P, P, U],
                &[],
                &[list, node, charge, carrier, recipe, point],
            )?;
            let no = self.b.ins().iconst(types::I8, 0);
            self.b.ins().return_(&[no]);
            self.b.switch_to_block(done);
            return Ok(());
        }
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        Ok(())
    }
    pub(super) fn resume_buffer(
        &mut self,
        field: &FieldTy,
        carrier: Value,
        slot: Value,
        list: Value,
    ) -> Result<()> {
        let base = self.load(carrier, 0);
        let len = self.load(carrier, 8);
        let cursor = self.load(slot, 0);
        let rank = self.icon(0);
        let (plan, stride, map) = match field {
            FieldTy::Seq(e) => (
                plan_field(self.tables, e)?,
                field_size(&self.tables.input.types, self.tables.layouts, e).0,
                false,
            ),
            FieldTy::Map(k, v) => {
                let entry = hirsel_core_layout::map_entry(
                    &self.tables.input.types,
                    self.tables.layouts,
                    k,
                    v,
                );
                (
                    Plan::Fields(vec![
                        (entry.key, plan_field(self.tables, k)?),
                        (entry.value, plan_field(self.tables, v)?),
                    ]),
                    entry.stride,
                    true,
                )
            }
            _ => unreachable!(),
        };
        if plan.count() != 0 {
            self.resume_array(&plan, [base, rank, cursor, slot, list, len], stride, map)?;
        }
        Ok(())
    }
    fn resume_array(
        &mut self,
        p: &Plan,
        traversal: [Value; 6],
        stride: u64,
        map: bool,
    ) -> Result<()> {
        let [base, rank, cursor, slot, list, len] = traversal;
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
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
        self.b.ins().brif(more, body, &[], done, &[]);
        self.b.switch_to_block(body);
        let off = self.b.ins().imul_imm_s(i, stride as i64);
        let at = self.b.ins().iadd(base, off);
        let off = self.b.ins().imul_imm_s(i, p.count() as i64);
        let r = self.b.ins().iadd(rank, off);
        if map {
            let live = self.b.create_block();
            let skip = self.b.create_block();
            let state = self.load(at, 8);
            self.b.ins().brif(state, live, &[], skip, &[]);
            self.b.switch_to_block(live);
            self.resume(p, at, r, cursor, slot, list)?;
            self.b.ins().jump(skip, &[]);
            self.b.switch_to_block(skip);
        } else {
            self.resume(p, at, r, cursor, slot, list)?;
        }
        let next = self.b.ins().iadd_imm_s(i, 1);
        self.b.ins().jump(head, &[next.into()]);
        self.b.switch_to_block(done);
        Ok(())
    }
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
                let mut yields = true;
                match field {
                    // Its ordinary glue: the finalizer, then the fields, or the
                    // resource's release. Its status joins the worklist's, and it
                    // hands nothing over.
                    FieldTy::Inline(t) => {
                        self.drop_type(base, *t);
                        yields = false;
                    }
                    // A runtime holder begins its cleanup and hands it over
                    // (CONC-8, CONC-9, CONC-13); a Latest only at its last handle.
                    FieldTy::Handle(
                        h @ (hirsel_core_layout::Handle::Receiver
                        | hirsel_core_layout::Handle::Task
                        | hirsel_core_layout::Handle::Latest),
                    ) => {
                        let point = self.next_point();
                        let staged = if *h == hirsel_core_layout::Handle::Latest {
                            self.call(
                                "hirsel_core_latest_release",
                                &[P, P, U],
                                &[B],
                                &[node, list, point],
                            )?[0]
                        } else {
                            let name = if *h == hirsel_core_layout::Handle::Task {
                                "hirsel_core_task_release"
                            } else {
                                "hirsel_core_receiver_release"
                            };
                            self.call(name, &[P, P, P, U], &[B], &[self.ctx, node, list, point])?[0]
                        };
                        let handed = self.b.create_block();
                        self.b.ins().brif(staged, handed, &[], skip, &[]);
                        self.b.switch_to_block(handed);
                    }
                    // A handle or an owned closure is released whole in place.
                    FieldTy::Handle(_) | FieldTy::OwnFn => {
                        self.field(base, field)?;
                        yields = false;
                    }
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
                        if hirsel_core_layout::terminal(
                            &self.tables.input.types,
                            self.tables.layouts,
                            field,
                        ) {
                            fast::free(
                                &mut self.b,
                                &mut self.calls,
                                self.object,
                                self.ctx,
                                node,
                                charge,
                            )?;
                            yields = false;
                        } else {
                            let descriptor = self.descriptor(*t, false);
                            let point = self.next_point();
                            self.stage(list, node, descriptor, point);
                        }
                    }
                    FieldTy::Rc(t) | FieldTy::Arc(t) => {
                        let atomic = matches!(field, FieldTy::Arc(_));
                        if hirsel_core_layout::terminal(
                            &self.tables.input.types,
                            self.tables.layouts,
                            field,
                        ) {
                            self.release(node, *t, atomic)?;
                            yields = false;
                        } else {
                            let last = fast::decrement(&mut self.b, node, atomic);
                            if atomic {
                                fast::arc_event(
                                    &mut self.b,
                                    &mut self.calls,
                                    self.object,
                                    self.ctx,
                                    "hirsel_core_arc_released",
                                    &[B],
                                    &[last],
                                )?;
                            }
                            let last_block = self.b.create_block();
                            self.b.set_cold_block(last_block);
                            self.b.ins().brif(last, last_block, &[], skip, &[]);
                            self.b.switch_to_block(last_block);
                            let descriptor = self.descriptor(*t, true);
                            let point = self.next_point();
                            self.stage(list, node, descriptor, point);
                        }
                    }
                    FieldTy::Seq(e) => {
                        let logical =
                            field_size(&self.tables.input.types, self.tables.layouts, e).2;
                        self.enqueue_buffer(field, base, node, logical, list)?;
                        yields = false;
                    }
                    FieldTy::Map(k, v) => {
                        let logical = field_size(&self.tables.input.types, self.tables.layouts, k)
                            .2
                            + field_size(&self.tables.input.types, self.tables.layouts, v).2
                            + 16;
                        self.enqueue_buffer(field, base, node, logical, list)?;
                        yields = false;
                    }
                    _ => unreachable!(),
                }
                if yields {
                    let no = self.b.ins().iconst(types::I8, 0);
                    self.b.ins().return_(&[no]);
                } else {
                    self.b.ins().jump(skip, &[]);
                }
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
                let v =
                    super::super::case::case_value(&mut self.b, tag, base, MemFlagsData::trusted());
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
                let len = self.icon(*n);
                self.resume_array(p, [base, rank, cursor, slot, list, len], *stride, false)?;
            }
            Plan::Array(..) => {}
        }
        Ok(())
    }
}
