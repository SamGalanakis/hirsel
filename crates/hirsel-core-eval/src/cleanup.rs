//! Cleanup (FAIL-8, FAIL-12, SEM-9) as one worklist, in constant native stack like
//! native drop glue: a value's depth decides only the worklist's length. Each step
//! pushes what follows it in reverse, and a block's free goes below what it holds, so
//! the order is the recursive walk's: fields in order, each owned block's contents
//! before the block, list rows in order with each row's columns in field order, map
//! entries in insertion order with each key before its value.
use super::*;
use hirsel_core_layout::MapEntry;

enum Work {
    Value(u64, TypeRef),
    Field(u64, FieldTy),
    /// A list slot is a take before its cleanup (`drop_marked`).
    Marked(u64, usize),
    Free(u64, Option<u64>, Option<u64>, &'static str),
    /// Row `i` of `length` onward, over `(column, base, stride)`.
    Rows(Vec<(FieldTy, u64, u64)>, u64, u64),
    /// Map storage slots from `i` of `slots` onward.
    Entries {
        block: u64,
        slots: u64,
        i: u64,
        e: MapEntry,
        k: FieldTy,
        v: FieldTy,
    },
}

impl<'m, H: Host<'m>> Machine<'m, H> {
    // Each returns whether a finalizer failed.
    pub(super) fn drop_value(&mut self, t: &mut Task, at: u64, ty: TypeRef) -> bool {
        self.clean(t, vec![Work::Value(at, ty)])
    }
    pub(super) fn drop_fields(&mut self, t: &mut Task, base: u64, ty: TypeRef) -> bool {
        let mut work = Vec::new();
        self.push_fields(&mut work, base, ty);
        self.clean(t, work)
    }

    fn push_fields(&self, work: &mut Vec<Work>, base: u64, ty: TypeRef) {
        let row = match self.fixed(ty).tag {
            Some(_) => self.case_at(base, ty) as usize,
            None => 0,
        };
        let fields = &self.recipes[ty.index()].drop.as_ref().unwrap().fields[row];
        work.extend(
            fields
                .iter()
                .rev()
                .map(|f| Work::Field(base + f.offset, f.ty.clone())),
        );
    }

    fn clean(&mut self, t: &mut Task, mut work: Vec<Work>) -> bool {
        let mut failed = false;
        while let Some(item) = work.pop() {
            match item {
                Work::Value(at, ty) => {
                    if self.recipes[ty.index()].drop.is_none() {
                        continue;
                    }
                    if self.is_resource(ty) {
                        let blk = self.load(at, 8);
                        failed |= self.drop_resource(t, blk, ty);
                        continue;
                    }
                    // An own row's core finalizer (crate docs, "Slot kernels") gets the
                    // value's address first, with no grant and no cause cell. wf refuses
                    // one that may fail, so it only moves slots; the fields' cleanup does
                    // the dropping.
                    if let Some(f) = self.shape(ty).finalizer {
                        let _ = self.call(t, FuncId(f.0), &[at]);
                    }
                    self.push_fields(&mut work, at, ty);
                }
                Work::Field(at, ty) => failed |= self.clean_field(t, &mut work, at, &ty),
                Work::Marked(at, size) => self.drop_marked(at, size),
                Work::Free(p, charge, size, what) => self.free_as(p, charge, size, what),
                Work::Rows(columns, i, length) => {
                    if i == length {
                        continue;
                    }
                    let row: Vec<_> = columns
                        .iter()
                        .rev()
                        .flat_map(|(column, base, stride)| {
                            let at = base + i * stride;
                            [
                                Work::Field(at, column.clone()),
                                Work::Marked(at, *stride as usize),
                            ]
                        })
                        .collect();
                    work.push(Work::Rows(columns, i + 1, length));
                    work.extend(row);
                }
                Work::Entries {
                    block,
                    slots,
                    mut i,
                    e,
                    k,
                    v,
                } => {
                    // SEM-5: removed entries (state 0) were cleaned by `map.remove`.
                    while i < slots && self.load(block + i * e.stride + e.meta, 8) == 0 {
                        i += 1;
                    }
                    if i == slots {
                        continue;
                    }
                    let entry = block + i * e.stride;
                    let (key, value) = (
                        Work::Field(entry + e.key, k.clone()),
                        Work::Field(entry + e.value, v.clone()),
                    );
                    work.push(Work::Entries {
                        block,
                        slots,
                        i: i + 1,
                        e,
                        k,
                        v,
                    });
                    work.push(value);
                    work.push(key);
                }
            }
        }
        failed
    }

    fn clean_field(&mut self, t: &mut Task, work: &mut Vec<Work>, at: u64, ty: &FieldTy) -> bool {
        match ty {
            FieldTy::Scalar(_)
            | FieldTy::Fn
            | FieldTy::RegionRef(_)
            | FieldTy::Locator(_)
            | FieldTy::Borrow
            | FieldTy::View(_)
            | FieldTy::Handle(Handle::Capability) => {}
            FieldTy::Inline(x) => work.push(Work::Value(at, *x)),
            // SEM-9: an inline array is cleaned from index 0 up.
            FieldTy::Array(e, n) => {
                let stride = self.stride(e);
                work.extend(
                    (0..*n)
                        .rev()
                        .map(|i| Work::Field(at + i * stride, (**e).clone())),
                );
            }
            FieldTy::Box(x) => {
                let p = self.load(at, 8);
                self.edge(p, ty, "a box");
                let size = (!self.is_resource(*x)).then(|| self.held(*x));
                work.push(Work::Free(
                    p,
                    charge::boxed(self.logical(*x)),
                    size,
                    "a box",
                ));
                work.push(Work::Value(p, *x));
            }
            FieldTy::FrozenBytes => {
                let capacity = self.load(at + 16, 8);
                if capacity != 0 {
                    let meta = self.load(at + 8, 8);
                    let block = if meta & 1 == 0 {
                        self.load(at, 8)
                    } else {
                        meta & !1
                    };
                    let size = (meta & 1 != 0).then_some(0);
                    self.free_as(block, charge::seq(capacity, 1), size, "frozen bytes");
                }
            }
            FieldTy::Seq(element) | FieldTy::Columns(element) => {
                self.push_rows(work, at, ty, element)
            }
            // SEM-5, SEM-9: live entries in insertion order, each key before its value
            // (`map_entry`). Storage exists exactly when the capacity is not 0.
            FieldTy::Map(k, v) => {
                let block = self.load(at, 8);
                let slots = self.load(at + 8, 8);
                let capacity = self.load(at + 16, 8);
                let e = map_entry(&self.m.types, &self.recipes, k, v);
                if capacity != 0 {
                    self.edge(block, ty, "map storage");
                    let key = field_size(&self.m.types, &self.recipes, k).2;
                    let value = field_size(&self.m.types, &self.recipes, v).2;
                    work.push(Work::Free(
                        block,
                        charge::map(capacity, key, value),
                        None,
                        "map storage",
                    ));
                }
                work.push(Work::Entries {
                    block,
                    slots,
                    i: 0,
                    e,
                    k: (**k).clone(),
                    v: (**v).clone(),
                });
            }
            // The environment names its captures' row; they are cleaned as that row
            // from offset 8, then the block is freed (crate docs, "Callable
            // environments").
            FieldTy::OwnFn => {
                let environment = self.load(at + 8, 8);
                if environment == 0 {
                    return false;
                }
                self.edge(environment, ty, "an environment");
                let row = TypeRef(
                    u32::try_from(self.load(environment, 8) - ENV_BASE).expect("an environment"),
                );
                let size = 8 + self.fixed(row).size.next_multiple_of(8);
                let charge = charge::closure_env(self.logical(row));
                work.push(Work::Free(
                    environment,
                    charge,
                    Some(size),
                    "an environment",
                ));
                work.push(Work::Value(environment + 8, row));
            }
            FieldTy::Rc(x) | FieldTy::Arc(x) => {
                let node = self.load(at, 8);
                self.release(node, *x);
            }
            FieldTy::Shared(x) => {
                let node = self.load(at, 8);
                self.release_shared(node, *x);
            }
            FieldTy::Handle(Handle::Causes) => {
                let head = self.load(at, 8);
                let cells = self.causes.remove(&head).expect("Causes dropped twice");
                let mut cell = head;
                while cell != 0 {
                    let next = self.load(cell + CELL_NEXT, 8);
                    let capacity = self.load(cell + CAUSE_CAPACITY, 8);
                    if capacity != 0 && capacity < CAUSE_KIND_START {
                        let data = self.load(cell, 8);
                        self.free(data);
                    }
                    self.free(cell);
                    cell = next;
                }
                for (l, n) in cells {
                    self.refund(l, n);
                }
            }
            FieldTy::Handle(h) => return H::drop_handle(self, t, at, *h),
            other => panic!("cleanup of {other:?} is not modelled"),
        }
        false
    }

    /// SEM-4, MEM-24: the rows of a List or Columns carrier at `at`. A Columns block
    /// holds column k at the sum of the columns before it, each `capacity × stride`
    /// rounded up to 8; a List is its one column. Rows are cleaned in order, each
    /// row's columns in field order, and the block is refunded as List storage of
    /// the row.
    fn push_rows(&self, work: &mut Vec<Work>, at: u64, ty: &FieldTy, row: &FieldTy) {
        let block = self.load(at, 8);
        let length = self.load(at + 8, 8);
        let capacity = self.load(at + 16, 8);
        let list = matches!(ty, FieldTy::Seq(_));
        let what = if list {
            "list storage"
        } else {
            "columns storage"
        };
        if self.stride(row) == 0 {
            // SEM-4: no element storage, nothing to clean (capacity 2^64 - 1).
            return;
        }
        if capacity != 0 {
            self.edge(block, ty, what);
        }
        let strides = match list {
            true => vec![(row.clone(), self.stride(row))],
            false => hirsel_core_layout::column_strides(&self.m.types, &self.recipes, row),
        };
        let mut columns = Vec::new();
        let mut base = block;
        for (column, stride) in strides {
            columns.push((column, base, stride));
            base += (capacity * stride).next_multiple_of(8);
        }
        // Capacity 0 has no storage: empty, or static data the carrier does not own
        // (core.md "Literals").
        if capacity != 0 {
            let element = field_size(&self.m.types, &self.recipes, row).2;
            let size = (!list).then_some(base - block);
            work.push(Work::Free(
                block,
                charge::seq(capacity, element),
                size,
                what,
            ));
        }
        work.push(Work::Rows(columns, 0, length));
    }
}
