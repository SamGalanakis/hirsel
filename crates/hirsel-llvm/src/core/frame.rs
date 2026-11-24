use super::*;

impl FunctionEmitter<'_> {
    pub(super) fn allocate_storage(&mut self) {
        for (i, t) in self.body.slots.iter().enumerate() {
            let (bytes, align) = self.place_layout(*t);
            self.sink
                .alloca(Local::Slot(i), &Type::bytes(bytes), align as u32);
        }
        for storage in self.body.frame_obligations() {
            let i = storage.index();
            self.sink.alloca(Local::FrameCharge(i), &Type::I64, 8);
            let active = Local::FrameActive(i);
            self.sink.alloca(active, &Type::I8, 1);
            self.store(&Type::I8, &int(0), &Value::Local(active), 1);
        }
    }

    pub(super) fn frame(&mut self, inst: &Inst) {
        match *inst {
            Inst::FrameAdmit {
                storage,
                admitted,
                available,
                charge,
                overflow,
                replace,
            } => {
                let slot = Value::Local(Local::FrameCharge(storage.index()));
                let charge = self.numeric(charge);
                let (ok, room) = self.frame_admit(&charge, overflow);
                let flag = self.zext(&Type::I1, &ok, &Type::I8);
                if replace {
                    let n = self.counter();
                    let update = Label::new("frame_replace", n);
                    let done = Label::new("frame_replaced", n);
                    self.likely(&ok, update, done);
                    self.block(update);
                    self.refund_frame_charge(&slot);
                    self.store(&Type::I64, &charge, &slot, 8);
                    self.br(done);
                    self.block(done);
                } else {
                    self.store(&Type::I64, &charge, &slot, 8);
                    let active = Value::Local(Local::FrameActive(storage.index()));
                    self.store(&Type::I8, &flag, &active, 1);
                }
                self.put(admitted, &flag);
                self.put(available, &room);
            }
            Inst::FrameActive { storage, result } => {
                let active = Value::Local(Local::FrameActive(storage.index()));
                let flag = self.load(&Type::I8, &active, 1);
                self.put(result, &flag);
            }
            Inst::FrameRefund { storage } => {
                self.refund_frame_charge(&Value::Local(Local::FrameCharge(storage.index())));
                let active = Value::Local(Local::FrameActive(storage.index()));
                self.store(&Type::I8, &int(0), &active, 1);
            }
            _ => unreachable!(),
        }
    }
}
