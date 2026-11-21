//! Core-private uninhabited storage over the shared List admission/carrier.
//! Slots is the nominal single-field record { raw: List[T] }. Its header's
//! length counts live slots; locations of holes remain core's responsibility.
use super::*;
use hirsel_core_ir::{ArithMode, ArithOp};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SlotPush {
    Back,
    Front,
    Heap,
}

#[derive(Clone, Copy)]
pub(super) struct PreparedPush {
    pub(super) slot: AddressId,
    full: ValueId,
    index: ValueId,
    kind: SlotPush,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn prepare_slot_push(
        &mut self,
        kind: SlotPush,
        item: TyId,
        header: ValueId,
        n: NodeId,
    ) -> Option<PreparedPush> {
        let ty = self.nonempty_aggregate(item)?;
        let word = Ty::Scalar(Scalar::U64);
        let len = self.read_sequence_word(header, 8, word, n);
        let cap = self.read_sequence_word(header, 16, word, n);
        let full = self.list_compare(CompareOp::Eq, len, cap, n);
        let one = self.konst(word, 1, n)?;
        let index = if kind == SlotPush::Heap {
            len
        } else {
            let head = self.read_sequence_word(header, 24, word, n);
            let (mask, _) = self.list_arith(ArithOp::Sub, ArithMode::Wrapping, cap, one, n);
            let (pos, _) = match kind {
                SlotPush::Front => self.list_arith(ArithOp::Sub, ArithMode::Wrapping, head, one, n),
                _ => self.list_arith(ArithOp::Add, ArithMode::Wrapping, head, len, n),
            };
            let index = self.value(word);
            self.inst(
                Inst::Binary {
                    result: index,
                    op: BinaryOp::BitAnd,
                    left: pos,
                    right: mask,
                    fail: None,
                },
                n,
            );
            index
        };
        let module = if kind == SlotPush::Heap {
            "heap"
        } else {
            "deque"
        };
        let Some(prepare) = self.p.core_fn(module, "prepare_push") else {
            self.gap(n, "a prepared push requires its slot preparation body");
            return None;
        };
        let slot = self.choose_spare_slot(ty, full, n, |cx| {
            cx.emit_call(prepare, &[item], &[Some(header), Some(index)], UNIT, n);
            Some(cx.slots_at(item, header, index, n)?.0)
        })?;
        Some(PreparedPush {
            slot,
            full,
            index,
            kind,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn finish_slot_push(
        &mut self,
        prepared: PreparedPush,
        f: usize,
        item: TyId,
        header: ValueId,
        value: Option<ValueId>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() || value.and_then(|v| self.pointer_address(v)) != Some(prepared.slot) {
            return self.emit_call(f, &[item], &[Some(header), value], result, n);
        }
        let full = self.new_block(&[]);
        let spare = self.new_block(&[]);
        let join = self.new_result_join();
        self.list_branch(prepared.full, full, spare, n);
        self.start(full);
        let built = self.emit_call(f, &[item], &[Some(header), value], result, n);
        self.goto(join, result, built, n);
        self.start(spare);
        self.consume(value, n);
        let module = if prepared.kind == SlotPush::Heap {
            "heap"
        } else {
            "deque"
        };
        let Some(publish) = self.p.core_fn(module, "finish_prepared_push") else {
            self.gap(n, "a prepared push requires its publication body");
            return None;
        };
        let front = (prepared.kind != SlotPush::Heap)
            .then(|| {
                self.konst(
                    Ty::Scalar(Scalar::Bool),
                    u64::from(prepared.kind == SlotPush::Front),
                    n,
                )
            })
            .flatten();
        let mut args = vec![Some(header), Some(prepared.index)];
        if let Some(front) = front {
            args.push(Some(front));
        }
        self.emit_call(publish, &[item], &args, UNIT, n);
        let built = self.wrap_case(result, 0, UNIT, None, n);
        self.goto(join, result, built, n);
        self.land(join)
    }

    fn publish_slot(
        &mut self,
        header: ValueId,
        address: AddressId,
        ty: PlaceTy,
        n: NodeId,
    ) -> Option<()> {
        let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
        self.inst(
            Inst::Mark {
                address,
                count: one,
                ty,
                live: true,
            },
            n,
        );
        self.slots_count(header, true, one, n)
    }

    pub(super) fn slots_value(
        &mut self,
        name: &str,
        item: TyId,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<Option<ValueId>> {
        match name {
            "slots_needs_drop" => {
                Some(self.key_operation(key_glue::KeyOp::NeedsDrop, item, &[], n))
            }
            // list_with_capacity writes the payload's three carrier words by
            // byte offset. Slots' sole raw field has exactly those same words.
            "slots_new" => Some(self.slots_new(item, vals[0]?, result, n)),
            "slots_empty" => Some(self.empty_list(result, n)),
            "slots_unmark_spare" | "slots_publish_spare" => {
                let header = vals[0]?;
                let (address, ty) = self.slots_at(item, header, vals[1]?, n)?;
                if name == "slots_publish_spare" {
                    self.publish_slot(header, address, ty, n)?;
                } else {
                    let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
                    self.inst(
                        Inst::Unmark {
                            address,
                            count: one,
                            ty,
                            live: false,
                        },
                        n,
                    );
                }
                Some(None)
            }
            "slots_put" => {
                let header = vals[0]?;
                let index = vals[1]?;
                let (at, ty) = self.slots_at(item, header, index, n)?;
                let rep = self.rep_at(item, n)?;
                self.install(at, rep, vals[2], n);
                self.consume(vals[2], n);
                self.publish_slot(header, at, ty, n)?;
                Some(None)
            }
            "slots_take" => {
                let header = vals[0]?;
                let index = vals[1]?;
                let (at, ty) = self.slots_at(item, header, index, n)?;
                let value = self.read_carried(at, item, n);
                let saved = self.slot(item, value, n)?;
                let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
                self.inst(
                    Inst::Mark {
                        address: at,
                        count: one,
                        ty,
                        live: false,
                    },
                    n,
                );
                self.slots_count(header, false, one, n)?;
                let value = self.read_carried(saved, item, n);
                self.temp(result, value);
                Some(value)
            }
            "slots_view" | "slots_list_view" => {
                let (at, _) = self.slots_at(item, vals[0]?, vals[1]?, n)?;
                Some(self.read_carried(at, item, n))
            }
            "slots_move" | "slots_shift" | "slots_list_move" => {
                let source_header = vals[0]?;
                let (target_header, to) = if name == "slots_shift" {
                    (source_header, vals[2]?)
                } else {
                    (vals[2]?, vals[3]?)
                };
                // FORM #90: a move between distinct blocks is a run of `count`
                // slots, one copy and one mark each way; a shift is one slot.
                let count = if name == "slots_shift" {
                    self.konst(Ty::Scalar(Scalar::U64), 1, n)?
                } else {
                    vals[4]?
                };
                let (source, ty) = self.slots_at(item, source_header, vals[1]?, n)?;
                let (target, _) = self.slots_at(item, target_header, to, n)?;
                let stride = self.layout_size(item, false, n)?;
                let (bytes, _) =
                    self.list_arith(ArithOp::Mul, ArithMode::Wrapping, count, stride, n);
                self.inst(
                    Inst::Copy {
                        to: target,
                        from: source,
                        bytes,
                    },
                    n,
                );
                self.inst(
                    Inst::Mark {
                        address: source,
                        count,
                        ty,
                        live: false,
                    },
                    n,
                );
                self.inst(
                    Inst::Mark {
                        address: target,
                        count,
                        ty,
                        live: true,
                    },
                    n,
                );
                // General transfers count both live sets. A shift keeps the
                // count and permits self-moves without two mutable borrows.
                // Sort's aliases leave both headers fixed: Hole tracks its exact live set and restores
                // xs before the zero-length scratch's structural free.
                if name == "slots_move" {
                    self.slots_count(source_header, false, count, n)?;
                    self.slots_count(target_header, true, count, n)?;
                }
                Some(None)
            }
            "slots_finish_list" => {
                self.slots_finish_list(item, vals[0]?, n)?;
                Some(None)
            }
            "slots_forget_ring" => {
                self.slots_forget_ring(item, vals[0]?, vals[1]?, n)?;
                Some(None)
            }
            "slots_free" => {
                self.slots_free(item, vals[0]?, n)?;
                Some(None)
            }
            _ => None,
        }
    }

    // Only the core finalizer's no-drop branch may discard live slots. Marks
    // preserve the evaluator's live-set accounting; native engines erase them.
    // All indices the sort kernel touched are below its original len. On every
    // exit it restores that prefix, so no tail shadow survives this handoff.
    fn slots_finish_list(&mut self, item: TyId, header: ValueId, n: NodeId) -> Option<()> {
        let count = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let (address, ty) = self.slots_at(item, header, zero, n)?;
        self.inst(
            Inst::Unmark {
                address,
                count,
                live: true,
                ty,
            },
            n,
        );
        Some(())
    }

    fn slots_forget_ring(
        &mut self,
        item: TyId,
        header: ValueId,
        head: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let count = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let capacity = self.read_sequence_word(header, 16, Ty::Scalar(Scalar::U64), n);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
        let (mask, _) = self.list_arith(ArithOp::Sub, ArithMode::Wrapping, capacity, one, n);
        let check = self.new_block(&[Ty::Scalar(Scalar::U64)]);
        let body = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.jump(check, &[zero], n);
        self.start(check);
        let index = self.b.blocks[check.index()]
            .params
            .get(&self.b.pools.values)
            .expect("the check block just built holds its parameter")[0];
        let more = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result: more,
                op: CompareOp::Lt,
                left: index,
                right: count,
            },
            n,
        );
        let yes = self.edge(body, &[]);
        let no = self.edge(done, &[]);
        self.end(
            Terminator::Branch {
                condition: more,
                yes,
                no,
            },
            Some(n),
        );
        self.start(body);
        let (at, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, head, index, n);
        let physical = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Binary {
                result: physical,
                op: BinaryOp::BitAnd,
                left: at,
                right: mask,
                fail: None,
            },
            n,
        );
        let (address, ty) = self.slots_at(item, header, physical, n)?;
        self.inst(
            Inst::Mark {
                address,
                count: one,
                live: false,
                ty,
            },
            n,
        );
        let (next, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, index, one, n);
        self.jump(check, &[next], n);
        self.start(done);
        self.sequence_word(header, 8, Ty::Scalar(Scalar::U64), zero, n);
        Some(())
    }

    fn slots_new(
        &mut self,
        item: TyId,
        count: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let value = self.list_with_capacity(item, count, result, n)?;
        // List uses MAX capacity for ZSTs. A ring still needs its requested
        // power-of-two capacity, even though its slots occupy no bytes.
        self.sequence_word(value, 16, Ty::Scalar(Scalar::U64), count, n);
        Some(value)
    }

    fn slots_at(
        &mut self,
        item: TyId,
        header: ValueId,
        index: ValueId,
        n: NodeId,
    ) -> Option<(AddressId, PlaceTy)> {
        let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let stride = self.layout_size(item, false, n)?;
        let (offset, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, index, stride, n);
        let place = self.place_ty(item, n)?;
        Some((
            self.address(AddressBase::Value(data), &[Step::Byte(offset)], place, n),
            place,
        ))
    }

    fn slots_count(&mut self, header: ValueId, add: bool, by: ValueId, n: NodeId) -> Option<()> {
        let count = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let op = if add { ArithOp::Add } else { ArithOp::Sub };
        let (count, _) = self.list_arith(op, ArithMode::Wrapping, count, by, n);
        self.sequence_word(header, 8, Ty::Scalar(Scalar::U64), count, n);
        Some(())
    }

    fn slots_free(&mut self, item: TyId, header: ValueId, n: NodeId) -> Option<()> {
        let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let capacity = self.read_sequence_word(header, 16, Ty::Scalar(Scalar::U64), n);
        let plan = self.list_plan(item, capacity, None, n)?;
        let null = self.konst(Ty::Ptr, 0, n)?;
        let nonnull = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result: nonnull,
                op: CompareOp::Ne,
                left: data,
                right: null,
            },
            n,
        );
        let free = self.new_block(&[]);
        let done = self.new_block(&[]);
        let yes = self.edge(free, &[]);
        let no = self.edge(done, &[]);
        self.end(
            Terminator::Branch {
                condition: nonnull,
                yes,
                no,
            },
            Some(n),
        );
        self.start(free);
        self.inst(
            Inst::Free {
                block: data,
                charge: plan.charge,
            },
            n,
        );
        self.jump(done, &[], n);
        self.start(done);
        self.consume(Some(header), n);
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModuleInput;
    use hirsel_core_resolve::{CorpusManifest, PackageManifest};
    use std::path::Path;

    #[test]
    fn own_finalizers_are_core_only() {
        let (index, modules) =
            crate::tests::program_inputs("type Hole { n: u64 } drop(h) {} fn go() {}");
        let p = Program::declare(&index, &modules);
        assert!(
            p.refusals
                .iter()
                .any(|r| r.rule == "FAIL-11" && r.message.contains("only core")),
            "{:?}",
            p.refusals
        );
    }

    /// MEM-17: a clone body replaces the structural copy, so outside core it would let
    /// any type mint copies that bypass its own fields' clones.
    #[test]
    fn clone_bodies_are_core_only() {
        let (index, modules) = crate::tests::program_inputs(
            "type Hole { n: u64 }\nclone(h: Hole) { Ok(Hole(n: 1)) }\nfn go() {}",
        );
        let p = Program::declare(&index, &modules);
        assert!(
            p.refusals
                .iter()
                .any(|r| r.rule == "MEM-17" && r.message.contains("only core")),
            "{:?}",
            p.refusals
        );
    }

    /// MEM-17 clone bodies (oracle: the body adds 100 to `n`, which no field walk does,
    /// and refuses `n == 0` with (7, 3)). `TestCopy` is declared in the prelude and its
    /// body in `list`, where the private kernel `test_bump` lives, as the Map's body sits
    /// beside its kernels; the body clones `List[T]` for an unbounded `T`, admitted only
    /// by the clone assumption. The derived clone calls each generic instance's body at
    /// depth and inside a generic body, and on a refusal drops what it already copied
    /// (`used` is 0) and returns the body's request.
    const CLONE_PRELUDE: &str =
        include_str!("../../../../core/front/fixtures/slots-clone-prelude.hirsel");
    const CLONE_LAWS: &str =
        include_str!("../../../../core/front/fixtures/slots-clone-laws.hirsel");

    /// The real core, with `prelude` appended to module `core` and `list` to `list`.
    fn core_inputs(
        prelude: &str,
        list: &str,
    ) -> (hirsel_core_resolve::NameIndex, Vec<ModuleInput>) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let package = hirsel_core_ids::PackageId {
            name: "core".into(),
            version: "1".into(),
        };
        let mut corpus = CorpusManifest {
            core: package.clone(),
            packages: vec![
                PackageManifest::directory(&root.join("core/core"), package, true).unwrap(),
            ],
        }
        .load()
        .unwrap();
        for (module, text) in [("core", prelude), ("list", list)] {
            corpus
                .sources
                .iter_mut()
                .find(|s| s.module.modules == [module])
                .unwrap()
                .text
                .push_str(text);
        }
        let parsed = corpus.parse();
        let index = corpus.index(&parsed);
        let modules = corpus
            .sources
            .iter()
            .zip(parsed.files)
            .enumerate()
            .map(|(source_id, (s, p))| ModuleInput {
                source: hirsel_core_resolve::SourceId(source_id as u32),
                id: index.module(&s.module).unwrap(),
                syntax: p.syntax,
            })
            .collect();
        (index, modules)
    }

    /// MEM-17: a clone body runs for every instance its type may be cloned at, so a
    /// second body for the type, or one assuming a bound the type does not declare (here
    /// `plain`, which `TestCopy[String]` lacks), is refused.
    #[test]
    fn clone_body_is_one_per_type_and_assumes_no_bound() {
        // The duplicate sits in another module than the first body: two in one module
        // are already one name declared twice in the index.
        for (prelude, extra, message) in [
            (
                "clone[T](t: TestCopy[T]) { Error(Capacity(requested: 1, available: 0)) }",
                "",
                "one clone body",
            ),
            (
                "",
                "type TestBound[T] { x: T }\nclone[T: plain](t: TestBound[T]) { Ok(TestBound(x: t.x)) }",
                "no stronger bounds",
            ),
            // No parameter names a type, yet the body is still an item to refuse.
            (
                "",
                "clone() { Error(Capacity(requested: 1, available: 0)) }",
                "one view of a core own type",
            ),
        ] {
            let (index, modules) = core_inputs(
                &format!("{CLONE_PRELUDE}\n{prelude}\n"),
                &format!("{CLONE_LAWS}\n{extra}\n"),
            );
            let p = Program::declare(&index, &modules);
            assert!(
                p.refusals
                    .iter()
                    .any(|r| r.rule == "MEM-17" && r.message.contains(message)),
                "{message}: {:?}",
                p.refusals
            );
        }
    }
}
