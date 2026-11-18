//! Native lowering of counted receive loops. The scalar CoreIR and evaluator
//! remain the reference. Only the loop's receive call and normal exit change
//! at emission; the cold step and raise entries release before their work.

use crate::*;

/// Call sites to fuse and block entries that finish a receive run.
#[derive(Default)]
pub struct ReceiveLoops {
    calls: Vec<bool>,
    exits: Vec<bool>,
}

impl ReceiveLoops {
    pub fn new(body: &Body) -> Self {
        if !body
            .calls
            .iter()
            .any(|call| matches!(call.callee, Callee::Runtime(RtOp::Receive { .. })))
        {
            return Self::default();
        }
        let mut loops = Self {
            calls: vec![false; body.calls.len()],
            exits: vec![false; body.blocks.len()],
        };
        let mut pointers = vec![None; body.values.len()];
        let mut reads = vec![None; body.values.len()];
        let mut constants = vec![None; body.values.len()];
        for inst in &body.insts {
            match *inst {
                Inst::Pointer { result, address } => pointers[result.index()] = Some(address),
                Inst::Read {
                    result, address, ..
                } => reads[result.index()] = Some(address),
                Inst::Const { result, bits } => constants[result.index()] = Some(bits),
                _ => {}
            }
        }
        let places = Places { body, pointers };
        for (header, row) in body.blocks.iter().enumerate() {
            let [
                Inst::Read {
                    result: index,
                    address,
                    ..
                },
                Inst::Compare {
                    result,
                    op: CompareOp::Lt,
                    left,
                    ..
                },
            ] = row.insts.get(&body.insts).unwrap()
            else {
                continue;
            };
            let Terminator::Branch { condition, yes, no } = body.terminators[header] else {
                continue;
            };
            if index != left || *result != condition || row.params.len != 0 {
                continue;
            }
            let Some(index_slot) = places.slot(*address) else {
                continue;
            };
            let entry = body.edges[yes.index()].target.index();
            let exit = body.edges[no.index()].target.index();
            let mut region = Region {
                places: &places,
                constants: &constants,
                reads: &reads,
                header,
                exit,
                index: *index,
                index_slot,
                index_address: *address,
                active: Vec::new(),
                done: Vec::new(),
                instructions: 0,
                writes: Vec::new(),
                receive: None,
                latch: None,
            };
            if let Some(call) = region.find(entry) {
                loops.calls[call.index()] = true;
                loops.exits[exit] = true;
            }
        }
        loops
    }

    pub fn call(&self, id: CallId) -> bool {
        self.calls.get(id.index()).copied().unwrap_or(false)
    }

    pub fn exit(&self, block: usize) -> bool {
        self.exits.get(block).copied().unwrap_or(false)
    }
}

struct Places<'a> {
    body: &'a Body,
    pointers: Vec<Option<AddressId>>,
}

impl Places<'_> {
    /// A statically projected frame slot. In particular, a pointer read
    /// from memory may name shared storage and does not prove locality.
    fn slot(&self, mut address: AddressId) -> Option<SlotId> {
        loop {
            let place = &self.body.addresses[address.index()];
            if !place
                .steps
                .get(&self.body.pools.steps)
                .unwrap()
                .iter()
                .all(|step| matches!(step, Step::Field(_) | Step::CaseField { .. }))
            {
                return None;
            }
            let parent = match place.base {
                AddressBase::Slot(slot) => return Some(slot),
                AddressBase::Address(parent) => parent,
                AddressBase::Value(pointer) => self.pointers[pointer.index()]?,
            };
            if parent.index() >= address.index() {
                return None;
            }
            address = parent;
        }
    }
}

struct Region<'a> {
    places: &'a Places<'a>,
    constants: &'a [Option<u64>],
    reads: &'a [Option<AddressId>],
    header: usize,
    exit: usize,
    index: ValueId,
    index_slot: SlotId,
    index_address: AddressId,
    active: Vec<usize>,
    done: Vec<usize>,
    instructions: usize,
    writes: Vec<SlotId>,
    receive: Option<(CallId, SlotId)>,
    latch: Option<usize>,
}

impl Region<'_> {
    fn find(&mut self, entry: usize) -> Option<CallId> {
        if entry == self.exit || self.header == self.exit {
            return None;
        }
        self.visit(entry)?;
        self.latch?;
        let (call, receiver) = self.receive?;
        let entry_insts = self.places.body.blocks[entry].insts;
        let entry_insts = entry_insts.get(&self.places.body.insts).unwrap();
        (entry_insts
            .iter()
            .any(|inst| matches!(inst, Inst::Call { call: id } if *id == call))
            && !self.writes.contains(&receiver)
            && receiver != self.index_slot)
            .then_some(call)
    }

    fn visit(&mut self, block: usize) -> Option<()> {
        if block == self.exit || self.done.contains(&block) {
            return Some(());
        }
        // Limit the work done with a retained token as well as the search.
        // Large loop bodies and inner cycles keep the ordinary scalar calls.
        if block == self.header
            || self.active.contains(&block)
            || self.active.len() + self.done.len() >= 32
        {
            return None;
        }
        self.active.push(block);
        let body = self.places.body;
        let insts = body.blocks[block].insts.get(&body.insts).unwrap();
        self.instructions += insts.len();
        if self.instructions > 128 {
            return None;
        }
        if let Terminator::Jump(edge) = body.terminators[block]
            && body.edges[edge.index()].target.index() == self.header
        {
            let [
                Inst::Const {
                    result: one,
                    bits: 1,
                },
                Inst::IntArith {
                    result: next,
                    op: ArithOp::Add,
                    mode: ArithMode::Wrapping,
                    left,
                    right,
                    flag: None,
                },
                Inst::Store { address, value },
            ] = insts
            else {
                return None;
            };
            if *left != self.index
                || right != one
                || next != value
                || *address != self.index_address
                || self.latch.replace(block).is_some()
            {
                return None;
            }
        } else {
            for inst in insts {
                self.private(inst)?;
            }
            let target = |edge: EdgeId| body.edges[edge.index()].target.index();
            match body.terminators[block] {
                Terminator::Jump(edge) => self.visit(target(edge))?,
                Terminator::Branch { yes, no, .. } => {
                    self.visit(target(yes))?;
                    self.visit(target(no))?;
                }
                Terminator::Raise {
                    cause: RaiseCause::Unwrap,
                    ..
                } => {}
                _ => return None,
            }
        }
        self.active.pop();
        self.done.push(block);
        Some(())
    }

    fn private(&mut self, inst: &Inst) -> Option<()> {
        match *inst {
            Inst::Const { .. }
            | Inst::LayoutConst { .. }
            | Inst::Step { .. }
            | Inst::StorageLive { .. }
            | Inst::StorageDead { .. }
            | Inst::Unary { .. }
            | Inst::Binary { .. }
            | Inst::IntArith { .. }
            | Inst::Compare { .. }
            | Inst::UpdateMask { .. } => {}
            Inst::Address {
                address,
                fail: None,
            }
            | Inst::Pointer { address, .. }
            | Inst::Read { address, .. }
            | Inst::Case { address, .. } => {
                self.places.slot(address)?;
            }
            Inst::Store { address, .. } | Inst::SetCase { address, .. } => self.write(address)?,
            Inst::Copy { to, from, bytes } if self.constants[bytes.index()]? <= 32 => {
                self.places.slot(from)?;
                self.write(to)?;
            }
            Inst::Call { call } if self.receive.is_none() => {
                let body = self.places.body;
                let row = &body.calls[call.index()];
                let Callee::Runtime(RtOp::Receive { .. }) = row.callee else {
                    return None;
                };
                let [Arg::Value(receiver), Arg::Address(destination)] =
                    row.args.get(&body.pools.args).unwrap()
                else {
                    return None;
                };
                self.write(*destination)?;
                self.receive = Some((call, self.places.slot(self.reads[receiver.index()]?)?));
            }
            _ => return None,
        }
        Some(())
    }

    fn write(&mut self, address: AddressId) -> Option<()> {
        let slot = self.places.slot(address)?;
        if slot == self.index_slot {
            return None;
        }
        self.writes.push(slot);
        Some(())
    }
}
