//! Conservative code-word provenance, independent of capture values. Joining all
//! writes keeps loop-local environments dynamic without guessing a callable after
//! reassignment. Escaping or mutably lent carrier slots are never bound.
use hirsel_core_ir::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Origin {
    Function(FuncId),
    Parameter(u32),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Root {
    Slot(SlotId),
    Parameter(u32),
}
#[derive(Clone, Copy, Debug)]
struct Location {
    root: Root,
    offset: Option<u64>,
}
#[derive(Clone, Copy)]
enum Write {
    Value(ValueId),
    Copy(AddressId, u64),
}
#[derive(Clone, Copy)]
enum Def {
    Unknown,
    Const(u64),
    Function(FuncId),
    Pointer(AddressId),
    Read(AddressId),
}

pub(super) struct Origins<'a> {
    body: &'a Body,
    defs: Vec<Def>,
    parameters: BTreeMap<ValueId, u32>,
    addresses: Vec<Option<Location>>,
    writes: BTreeMap<(Root, u64), Vec<Write>>,
    escaped: BTreeSet<Root>,
    cells: BTreeMap<(Root, u64), Option<Origin>>,
    visiting: BTreeSet<(Root, u64)>,
}
impl<'a> Origins<'a> {
    pub(super) fn new(module: &Module, body: &'a Body) -> Self {
        let mut defs = vec![Def::Unknown; body.values.len()];
        for inst in &body.insts {
            match *inst {
                Inst::Const { result, bits } => defs[result.index()] = Def::Const(bits),
                Inst::FunctionRef { result, function } => {
                    defs[result.index()] = Def::Function(function)
                }
                Inst::Pointer { result, address } => defs[result.index()] = Def::Pointer(address),
                Inst::Read {
                    result, address, ..
                } => defs[result.index()] = Def::Read(address),
                _ => {}
            }
        }
        let entry = body.blocks[0].params.get(&body.pools.values).unwrap();
        let parameters = body
            .callable_parameters
            .iter()
            .map(|v| (*v, entry.iter().position(|p| p == v).unwrap() as u32))
            .collect();
        let mut s = Self {
            body,
            defs,
            parameters,
            addresses: vec![None; body.addresses.len()],
            writes: BTreeMap::new(),
            escaped: BTreeSet::new(),
            cells: BTreeMap::new(),
            visiting: BTreeSet::new(),
        };
        let mut visiting = vec![false; body.addresses.len()];
        for i in 0..body.addresses.len() {
            s.address(AddressId(i as u32), &mut visiting);
        }
        // Record only code-word cells. Every candidate callable carrier starts at
        // zero in its own slot; copies preserve the byte offset of that word.
        for inst in &body.insts {
            match *inst {
                Inst::Store { address, value } => {
                    if let Some(loc) = s.addresses[address.index()] {
                        match loc.offset {
                            Some(0) => s
                                .writes
                                .entry((loc.root, 0))
                                .or_default()
                                .push(Write::Value(value)),
                            Some(n) if n >= 8 => {}
                            _ => {
                                s.escaped.insert(loc.root);
                            }
                        }
                    }
                    // Storing a pointer to a carrier lets another path change it.
                    if let Some(loc) = s.pointer(value) {
                        s.escaped.insert(loc.root);
                    }
                }
                Inst::Copy { to, from, bytes } => {
                    if let Some(loc) = s.addresses[to.index()] {
                        match (loc.offset, s.constant(bytes)) {
                            (_, Some(0)) => {}
                            (Some(0), Some(n)) if n >= 8 => s
                                .writes
                                .entry((loc.root, 0))
                                .or_default()
                                .push(Write::Copy(from, 0)),
                            (Some(n), _) if n >= 8 => {}
                            _ => {
                                s.escaped.insert(loc.root);
                            }
                        }
                    }
                }
                Inst::Call { call } => {
                    let c = &body.calls[call.index()];
                    let sig = &module.signatures[c.signature.index()];
                    for (i, arg) in c.args.get(&body.pools.args).unwrap().iter().enumerate() {
                        let parameter = i
                            .checked_sub(usize::from(sig.dest.is_some()))
                            .and_then(|i| sig.params.get(i));
                        if matches!(
                            parameter,
                            Some(Param::Address {
                                loan: Loan::View | Loan::Take,
                                ..
                            })
                        ) {
                            continue;
                        }
                        let loc = match arg {
                            Arg::Address(a) => s.addresses[a.index()],
                            Arg::Value(v) => s.pointer(*v),
                        };
                        if let Some(loc) = loc {
                            s.escaped.insert(loc.root);
                        }
                    }
                }
                // A case write into a carrier or a runtime write represented by
                // another instruction is not a source callable construction.
                Inst::SetCase { address, .. } => {
                    if let Some(loc) = s.addresses[address.index()] {
                        s.escaped.insert(loc.root);
                    }
                }
                _ => {}
            }
        }
        s
    }
    fn constant(&self, v: ValueId) -> Option<u64> {
        match self.defs[v.index()] {
            Def::Const(n) => Some(n),
            _ => None,
        }
    }
    fn pointer(&self, v: ValueId) -> Option<Location> {
        if let Some(&p) = self.parameters.get(&v) {
            return Some(Location {
                root: Root::Parameter(p),
                offset: Some(0),
            });
        }
        match self.defs[v.index()] {
            Def::Pointer(a) => self.addresses[a.index()],
            _ => None,
        }
    }
    fn address(&mut self, a: AddressId, visiting: &mut [bool]) -> Option<Location> {
        if let Some(loc) = self.addresses[a.index()] {
            return Some(loc);
        }
        if visiting[a.index()] {
            return None;
        }
        visiting[a.index()] = true;
        let info = &self.body.addresses[a.index()];
        let mut loc = match info.base {
            AddressBase::Slot(s) => Some(Location {
                root: Root::Slot(s),
                offset: Some(0),
            }),
            AddressBase::Address(a) => self.address(a, visiting),
            AddressBase::Value(v) => {
                if let Def::Pointer(a) = self.defs[v.index()] {
                    self.address(a, visiting)
                } else {
                    self.pointer(v)
                }
            }
        };
        if let Some(l) = &mut loc {
            for step in info.steps.get(&self.body.pools.steps).unwrap() {
                let offset = match *step {
                    Step::Byte(v) => self.constant(v),
                    Step::Field(0) => Some(0),
                    Step::Deref => {
                        loc = None;
                        break;
                    }
                    _ => None,
                };
                l.offset = l.offset.and_then(|n| offset.and_then(|o| n.checked_add(o)));
            }
        }
        visiting[a.index()] = false;
        self.addresses[a.index()] = loc;
        loc
    }
    pub(super) fn argument(&mut self, a: Arg) -> Option<Origin> {
        match a {
            Arg::Value(v) => self
                .value(v)
                .or_else(|| self.pointer(v).and_then(|l| self.cell(l))),
            Arg::Address(a) => self.addresses[a.index()].and_then(|l| self.cell(l)),
        }
    }
    pub(super) fn value(&mut self, v: ValueId) -> Option<Origin> {
        if let Some(&p) = self.parameters.get(&v) {
            return Some(Origin::Parameter(p));
        }
        match self.defs[v.index()] {
            Def::Function(f) => Some(Origin::Function(f)),
            Def::Read(a) => self.addresses[a.index()].and_then(|l| self.cell(l)),
            _ => None,
        }
    }
    fn cell(&mut self, loc: Location) -> Option<Origin> {
        let offset = loc.offset?;
        if let Root::Parameter(p) = loc.root {
            return (offset == 0).then_some(Origin::Parameter(p));
        }
        if self.escaped.contains(&loc.root) {
            return None;
        }
        let key = (loc.root, offset);
        if let Some(origin) = self.cells.get(&key) {
            return *origin;
        }
        if !self.visiting.insert(key) {
            return None;
        }
        let writes = self.writes.get(&key).cloned().unwrap_or_default();
        let mut result = None;
        for write in writes {
            let origin = match write {
                Write::Value(v) => self.value(v),
                Write::Copy(a, delta) => self.addresses[a.index()].and_then(|mut loc| {
                    loc.offset = loc.offset.and_then(|n| n.checked_add(delta));
                    self.cell(loc)
                }),
            };
            if origin.is_none() || result.is_some_and(|r| Some(r) != origin) {
                result = None;
                break;
            }
            result = origin;
        }
        self.visiting.remove(&key);
        self.cells.insert(key, result);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn carrier_reassignment_and_mutable_escape_never_bind_the_old_code_word() {
        let text = r#"core v1
sig g0 = () -> ()
sig g1 = (ptr) -> ()
inst n0 = app@1/main/root [] []
inst n1 = app@1/main/a [] []
inst n2 = app@1/main/b [] []
inst n3 = app@1/main/mutate [] []
fn f0 = n0 g0 {
 slot s0 = ptr
 addr a0 = s0 : ptr
b0:
 v0: ptr = fnref f1
 v1: ptr = fnref f2
 store a0, v0
 v2: ptr = read a0
 return
}
fn f1 = n1 g0 extern
fn f2 = n2 g0 extern
fn f3 = n3 g1 extern
"#;
        let mut m = hirsel_core_ir::text::parse(text).unwrap();
        let b = m.functions[0].body.as_ref().unwrap();
        assert_eq!(
            Origins::new(&m, b).value(ValueId(2)),
            Some(Origin::Function(FuncId(1)))
        );
        m.functions[0].body.as_mut().unwrap().insts.insert(
            3,
            Inst::Store {
                address: AddressId(0),
                value: ValueId(1),
            },
        );
        let b = m.functions[0].body.as_ref().unwrap();
        assert_ne!(
            Origins::new(&m, b).value(ValueId(2)),
            Some(Origin::Function(FuncId(1)))
        );
        m.functions[0].body.as_mut().unwrap().insts.remove(3);
        let b = m.functions[0].body.as_mut().unwrap();
        let v = ValueId(b.values.len() as u32);
        b.values.push(Ty::Ptr);
        b.insts.insert(
            3,
            Inst::Pointer {
                result: v,
                address: AddressId(0),
            },
        );
        let args = PoolRange::push(&mut b.pools.args, [Arg::Value(v)]);
        b.calls.push(CallSite {
            callee: Callee::Direct(FuncId(3)),
            signature: SigId(1),
            args,
            results: PoolRange::EMPTY,
            fail: None,
        });
        b.insts.insert(4, Inst::Call { call: CallId(0) });
        let b = m.functions[0].body.as_ref().unwrap();
        assert_eq!(Origins::new(&m, b).value(ValueId(2)), None);
    }
}
