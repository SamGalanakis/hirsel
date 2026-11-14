//! Core-private uninhabited storage over the shared List admission/carrier.
//! Slots is the nominal single-field record { raw: List[T] }. Its header's
//! length counts live slots; locations of holes remain core's responsibility.
use super::*;
use hirsel_core_ir::{ArithMode, ArithOp};

impl<'c, 'a> Cx<'c, 'a, '_> {
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
            "slots_put" => {
                let header = vals[0]?;
                let index = vals[1]?;
                let (at, ty) = self.slots_at(item, header, index, n)?;
                let rep = self.rep(item).ok()?;
                self.install(at, rep, vals[2], n);
                self.consume(vals[2], n);
                let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
                self.inst(
                    Inst::Mark {
                        address: at,
                        count: one,
                        ty,
                        live: true,
                    },
                    n,
                );
                self.slots_count(header, true, one, n)?;
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
            "slots_move" | "slots_shift" | "slots_from_list" | "slots_to_list" => {
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
            .get(&self.b.pools.values)?[0];
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
        let result_id = match self.rep(result).ok()? {
            Rep::Place(id) => id,
            _ => return None,
        };
        let payload = match self.t.kind(result) {
            TyKind::Adt(_, args) => args[0],
            _ => return None,
        };
        let payload_id = match self.rep(payload).ok()? {
            Rep::Place(id) => id,
            _ => return None,
        };
        let at = self.address(
            AddressBase::Value(value),
            &[],
            PlaceTy::Declared(result_id),
            n,
        );
        let tag = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Case {
                result: tag,
                address: at,
            },
            n,
        );
        let zero = self.konst(Ty::Scalar(Scalar::U32), 0, n)?;
        let ok = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result: ok,
                op: CompareOp::Eq,
                left: tag,
                right: zero,
            },
            n,
        );
        let yes = self.new_block(&[]);
        let done = self.new_block(&[]);
        let ye = self.edge(yes, &[]);
        let ne = self.edge(done, &[]);
        self.end(
            Terminator::Branch {
                condition: ok,
                yes: ye,
                no: ne,
            },
            Some(n),
        );
        self.start(yes);
        let carrier = self.address(
            AddressBase::Address(at),
            &[Step::CaseField { case: 0, field: 0 }],
            PlaceTy::Declared(payload_id),
            n,
        );
        let pointer = self.pointer(carrier, n);
        self.sequence_word(pointer, 16, Ty::Scalar(Scalar::U64), count, n);
        self.jump(done, &[], n);
        self.start(done);
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
    use std::{collections::HashMap, path::Path};

    const SLOT_LAWS: &str = r#"// test-of: core.list
// test-reason: exercise private uninhabited slot ownership and evaluator marks

// expect: run slots_move_law() = 11
fn slots_move_law() -> u64 {
    let mut s: Slots[u64] = result.unwrap(slots_new(4))
    slots_put(mut s, 1, 11)
    slots_shift(mut s, 1, 3)
    let got = slots_take(mut s, 3)
    slots_free(take s)
    got
}

// expect: run slots_owned_law() = true
fn slots_owned_law() -> bool {
    let mut s: Slots[String] = result.unwrap(slots_new(4))
    slots_put(mut s, 0, result.unwrap(string.from("abc")))
    slots_shift(mut s, 0, 2)
    let text = slots_take(mut s, 2)
    slots_free(take s)
    let another = result.unwrap(string.from("xyz"))
    text == "abc" && another == "xyz"
}

// expect: run slots_capacity_law() = 32
fn slots_capacity_law() -> u64 {
    let r = bound bytes 0 {
        let s: Result[Slots[u64], Capacity] = slots_new(4)
        s
    }
    match r { Ok(Error(c)) => c.requested, _ => 99 }
}

// expect: run slots_zero_size_law() = true
fn slots_zero_size_law() -> bool {
    let mut a: Slots[()] = result.unwrap(slots_new(4))
    let mut b: Slots[()] = result.unwrap(slots_new(4))
    slots_put(mut a, 0, ())
    slots_put(mut a, 1, ())
    slots_put(mut b, 0, ())
    let _ = slots_take(mut a, 1)
    let _ = slots_take(mut b, 0)
    let _ = slots_take(mut a, 0)
    let caps = list.capacity(a.raw) == 4 && list.capacity(b.raw) == 4
    slots_free(take a)
    slots_free(take b)
    caps
}

// expect: slot-fault put over the live slot
fn slots_double_put() {
    let mut s: Slots[u64] = result.unwrap(slots_new(4))
    slots_put(mut s, 0, 1)
    slots_put(mut s, 0, 2)
}

// expect: slot-fault read of the empty slot
fn slots_double_take() -> u64 {
    let mut s: Slots[u64] = result.unwrap(slots_new(4))
    slots_put(mut s, 0, 1)
    let _ = slots_take(mut s, 0)
    slots_take(mut s, 0)
}

// expect: slot-fault read of the empty slot
fn slots_empty_view() -> u64 {
    let mut s: Slots[u64] = result.unwrap(slots_new(4))
    slots_put(mut s, 1, 7)
    let _ = slots_take(mut s, 1)
    let x: u64 = slots_view(s, 1)
    x
}

// expect: slot-fault free of a block whose slot
fn slots_free_live() {
    let mut s: Slots[String] = result.unwrap(slots_new(4))
    slots_put(mut s, 3, result.unwrap(string.from("abc")))
    slots_free(take s)
}

// expect: slot-fault put over the live slot
fn slots_move_over_live() {
    let mut s: Slots[u64] = result.unwrap(slots_new(4))
    slots_put(mut s, 0, 1)
    slots_put(mut s, 1, 2)
    slots_shift(mut s, 0, 1)
}

// expect: run slots_cross_block_law() = 711
fn slots_cross_block_law() -> u64 {
    let mut a: Slots[u64] = result.unwrap(slots_new(2))
    let mut b: Slots[u64] = result.unwrap(slots_new(4))
    slots_put(mut a, 1, 7)
    slots_put(mut b, 3, 11)
    slots_move(mut a, 1, mut b, 0, 1)
    slots_shift(mut b, 3, 3)
    let answer = slots_take(mut b, 0) * 100 + slots_take(mut b, 3)
    slots_free(take a)
    slots_free(take b)
    answer
}
// expect: run slots_forget_ring_law() = true
fn slots_forget_ring_law() -> bool {
    let mut s: Slots[u64] = result.unwrap(slots_new(4))
    slots_put(mut s, 3, 11)
    slots_put(mut s, 0, 7)
    slots_forget_ring(mut s, 3)
    let empty = list.len(s.raw) == 0
    slots_free(take s)
    empty
}

"#;

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
    const CLONE_PRELUDE: &str = "\npub type TestCopy[T] { xs: List[T], n: u64 }\n";
    const CLONE_LAWS: &str = r#"
clone[T](t: TestCopy[T]) {
    match t.n {
        0 => Error(Capacity(requested: 7, available: 3))
        _ => Ok(TestCopy(xs: clone(t.xs)?, n: test_bump(t)))
    }
}
fn test_bump[T](t: TestCopy[T]) -> u64 { t.n + 100 }
type TestPair { a: TestCopy[u64], b: TestCopy[String] }
fn test_dup[T: plain](seed: T, n: u64) -> u64 {
    let x: TestCopy[T] = TestCopy(xs: list.new(), n: n)
    let y: TestCopy[T] = result.unwrap(clone(x))
    y.n
}

// expect: run clone_body_law() = 105107110
fn clone_body_law() -> u64 {
    let mut xs: List[u64] = list.new()
    result.unwrap(list.push(mut xs, 4))
    let p = TestPair(a: TestCopy(xs: take xs, n: 5), b: TestCopy(xs: list.new(), n: 7))
    let q: TestPair = result.unwrap(clone(p))
    let seed: u8 = 0
    q.a.n * 1000000 + q.b.n * 1000 + test_dup(seed, 9) + list.len(q.a.xs)
}

// expect: run clone_body_refusal_law() = 73
fn clone_body_refusal_law() -> u64 {
    let p = TestPair(a: TestCopy(xs: list.new(), n: 5), b: TestCopy(xs: list.new(), n: 0))
    let q: Result[TestPair, Capacity] = clone(p)
    match q { Ok(_) => 0, Error(c) => c.requested * 10 + c.available }
}
type TestSlotCopy[T] { s: Slots[T], n: u64 }
clone[T](t: TestSlotCopy[T]) {
    Ok(TestSlotCopy(s: slots_empty(), n: t.n + 100))
}
type TestSlotPair { value: TestSlotCopy[u64] }
// A declared body stops the structural walk before its private Slots field.
// expect: run clone_kernel_hook_law() = 1050
fn clone_kernel_hook_law() -> u64 {
    let mut s: Slots[u64] = result.unwrap(slots_new(1))
    slots_put(mut s, 0, 7)
    let p = TestSlotPair(value: TestSlotCopy(s: take s, n: 5))
    let q: TestSlotPair = result.unwrap(clone(p))
    q.value.n * 10 + list.len(q.value.s.raw)
}

"#;

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
            .map(|(s, p)| ModuleInput {
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

    #[test]
    fn private_slot_contracts_and_misuse() {
        let mut laws = SLOT_LAWS.to_string();
        laws.push_str(CLONE_LAWS);
        laws.push_str(include_str!(
            "../../../../core/front/fixtures/slots-needs-drop.hirsel"
        ));
        laws.push_str("\ntype TestOwn { xs: List[u64] } drop(t) { list.clear(mut t.xs) }\n// expect: run slots_own_finalizer() = 9\nfn slots_own_finalizer() -> u64 { let mut xs: List[u64] = list.new(); result.unwrap(list.push(mut xs, 1)); result.unwrap(list.push(mut xs, 2)); let x = TestOwn(xs: take xs); let _ = take x; 9 }\n");
        let (index, modules) = core_inputs(CLONE_PRELUDE, &laws);
        let p = Program::declare(&index, &modules);
        assert!(p.refusals.is_empty(), "{:?}", p.refusals);
        let mut emitter = Emitter::new();
        let mut functions = HashMap::new();
        for (f, fd) in p.fns.iter().enumerate().filter(|(_, f)| f.body.is_some()) {
            let check = p.check_lower(f, &mut emitter);
            assert!(
                check.refusals.is_empty() && check.gaps.is_empty(),
                "{}: {check:?}",
                fd.path
            );
            functions.insert(fd.path.0.name.clone(), check.function.unwrap());
        }
        emitter.materialize_instances().unwrap();
        hirsel_core_ir::wf::module(&emitter.module).unwrap();
        let mut passed = 0;
        let mut faults = 0;
        let mut pending_fault = None;
        for line in laws.lines() {
            if let Some(expect) = line.strip_prefix("// expect: run ") {
                let (call, want) = expect.split_once(" = ").unwrap();
                let name = call.strip_suffix("()").unwrap();
                let f = functions[name];
                eprintln!("running {name}");
                let sig = &emitter.module.signatures
                    [emitter.module.functions[f.index()].signature.index()];
                let report = hirsel_core_ir::eval::run(&emitter.module, f, &[], u64::MAX);
                assert_eq!(
                    report.outcome,
                    hirsel_core_ir::eval::Outcome::Returned(vec![hirsel_core_ir::eval::scalar(
                        sig.results[0],
                        want
                    )]),
                    "{name}"
                );
                assert_eq!(report.used, 0, "{name} leaked its admission");
                passed += 1;
            } else if let Some(fault) = line.strip_prefix("// expect: slot-fault ") {
                pending_fault = Some(fault);
            } else if let Some(fault) = pending_fault.take() {
                let name = line.strip_prefix("fn ").unwrap().split('(').next().unwrap();
                let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    hirsel_core_ir::eval::run(&emitter.module, functions[name], &[], u64::MAX)
                }))
                .err()
                .expect("slot misuse must fault on the evaluator");
                let message = panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap();
                assert!(message.contains(fault), "{name}: {message}, want {fault}");
                faults += 1;
            }
        }
        assert_eq!((passed, faults), (11, 5));
    }
}
