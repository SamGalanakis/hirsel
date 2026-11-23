//! Forward a terminal call into the caller's uninitialized result destination.
//! A cleanup, store, read or fallible projection after the call keeps the staged
//! result: failure cleanup may have to drop it before abandoning the return.
use super::*;
use hirsel_core_ir::native_layout::CallLayouts;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Root {
    Input(ValueId),
    Slot(SlotId),
}

pub(super) struct Forwarding<'a> {
    body: &'a Body,
    incoming: Vec<bool>,
    materialized: Vec<bool>,
    call_layouts: Option<&'a CallLayouts>,
    pointers: Vec<Option<AddressId>>,
    permanent: Vec<bool>,
    roots: Vec<Option<Option<Root>>>,
    safe_addresses: Vec<Option<bool>>,
}

pub(super) struct Ending {
    pub prefix: usize,
    pub call: CallId,
    pub destination: bool,
    pub frame_safe: bool,
}

impl<'a> Forwarding<'a> {
    pub fn new(body: &'a Body) -> Self {
        let mut incoming = vec![false; body.values.len()];
        for v in body.blocks[0].params.get(&body.pools.values).unwrap() {
            incoming[v.index()] = true;
        }
        let mut pointers = vec![None; body.values.len()];
        let mut permanent = vec![false; body.values.len()];
        for inst in &body.insts {
            match *inst {
                Inst::Pointer { result, address } => pointers[result.index()] = Some(address),
                Inst::ReadStatic { result, .. }
                | Inst::FunctionRef { result, .. }
                | Inst::DescriptorRef { result, .. } => permanent[result.index()] = true,
                Inst::Const { result, bits: 0 } if body.values[result.index()] == Ty::Ptr => {
                    permanent[result.index()] = true
                }
                _ => {}
            }
        }
        Self {
            body,
            incoming,
            materialized: vec![false; body.values.len()],
            call_layouts: None,
            pointers,
            permanent,
            roots: vec![None; body.addresses.len()],
            safe_addresses: vec![None; body.addresses.len()],
        }
    }

    pub fn native(body: &'a Body, sig: &Signature, layouts: &'a CallLayouts) -> Self {
        let mut forwarding = Self::new(body);
        forwarding.call_layouts = Some(layouts);
        let values = body.blocks[0].params.get(&body.pools.values).unwrap();
        let skip = usize::from(sig.dest.is_some());
        if layouts.result(sig).is_some() || layouts.ok_result(sig).is_some() {
            forwarding.materialized[values[0].index()] = true;
        }
        for (&value, &param) in values[skip..].iter().zip(&sig.params) {
            forwarding.materialized[value.index()] = layouts.parameter(param).is_some();
        }
        forwarding
    }

    fn root(&mut self, a: AddressId) -> Option<Root> {
        if let Some(root) = self.roots[a.index()] {
            return root;
        }
        let info = &self.body.addresses[a.index()];
        let root = if info.steps.len != 0 {
            None
        } else {
            match info.base {
                AddressBase::Slot(s) => Some(Root::Slot(s)),
                AddressBase::Address(a) => self.root(a),
                AddressBase::Value(v) if self.incoming[v.index()] => Some(Root::Input(v)),
                AddressBase::Value(v) => self.pointers[v.index()].and_then(|a| self.root(a)),
            }
        };
        self.roots[a.index()] = Some(root);
        root
    }

    fn value_safe(&mut self, v: ValueId) -> bool {
        (self.incoming[v.index()] && !self.materialized[v.index()])
            || self.permanent[v.index()]
            || self.pointers[v.index()].is_some_and(|a| self.address_safe(a))
    }

    fn address_safe(&mut self, a: AddressId) -> bool {
        if let Some(safe) = self.safe_addresses[a.index()] {
            return safe;
        }
        let info = &self.body.addresses[a.index()];
        let safe = !info
            .steps
            .get(&self.body.pools.steps)
            .unwrap()
            .contains(&Step::Deref)
            && match info.base {
                AddressBase::Slot(_) => false,
                AddressBase::Address(a) => self.address_safe(a),
                AddressBase::Value(v) => self.value_safe(v),
            };
        self.safe_addresses[a.index()] = Some(safe);
        safe
    }

    pub fn ending(
        &mut self,
        module: &Module,
        recipes: &[hirsel_core_layout::RecipeRow],
        signature: &Signature,
        constants: &[Option<u64>],
        block: usize,
    ) -> Option<Ending> {
        let Terminator::Return(returned) = &self.body.terminators[block] else {
            return None;
        };
        let insts = self.body.blocks[block].insts.get(&self.body.insts).unwrap();
        let prefix = insts.iter().rposition(|i| matches!(i, Inst::Call { .. }))?;
        let Inst::Call { call } = insts[prefix] else {
            unreachable!()
        };
        let site = &self.body.calls[call.index()];
        if matches!(site.callee, Callee::Runtime(_)) || site.fail.is_some() {
            return None;
        }
        let callee = &module.signatures[site.signature.index()];
        if signature.dest != callee.dest
            || signature.results != callee.results
            || site.results.get(&self.body.pools.values) != returned.get(&self.body.pools.values)
        {
            return None;
        }
        let arguments = site.args.get(&self.body.pools.args).unwrap();
        let dest = signature.dest.is_some();
        let mut staged = None;
        if let Some(t) = signature.dest {
            let Arg::Address(a) = arguments[0] else {
                return None;
            };
            let input = self.body.blocks[0]
                .params
                .get(&self.body.pools.values)
                .unwrap()[0];
            let target = Root::Input(input);
            match self.root(a)? {
                root if root == target => {}
                source @ Root::Slot(_) => {
                    let hirsel_core_layout::Layout::Fixed(layout) = &recipes[t.index()].layout
                    else {
                        return None;
                    };
                    staged = Some((source, target, layout.size));
                }
                Root::Input(_) => return None,
            }
        }
        for inst in &insts[prefix + 1..] {
            match *inst {
                Inst::Const { .. } | Inst::Pointer { .. } | Inst::StorageDead { .. } => {}
                Inst::Address {
                    address,
                    fail: None,
                } if self.body.addresses[address.index()].steps.len == 0 => {}
                Inst::Copy { to, from, bytes } => {
                    let (source, target, size) = staged.take()?;
                    if self.root(from) != Some(source)
                        || self.root(to) != Some(target)
                        || constants[bytes.index()] != Some(size)
                    {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        if staged.is_some() {
            return None;
        }
        let frame_safe = arguments.iter().enumerate().all(|(i, arg)| {
            if dest && i == 0 {
                return true;
            }
            if self.call_layouts.is_some_and(|layouts| {
                layouts
                    .parameter(callee.params[i - usize::from(dest)])
                    .is_some()
            }) {
                return true;
            }
            match *arg {
                Arg::Address(a) => self.address_safe(a),
                Arg::Value(v) => self.body.values[v.index()] != Ty::Ptr || self.value_safe(v),
            }
        });
        Some(Ending {
            prefix,
            call,
            destination: dest,
            frame_safe,
        })
    }
}
