use super::*;

impl<'m, H: Host<'m>> Machine<'m, H> {
    pub(super) fn argon2id(&mut self, t: &mut Task, a: &[u64]) -> X<Vec<u64>> {
        use hirsel_core_net::argon2id::{self, Error, Plan};

        let len = a[11];
        let out = a[12];
        let inputs = [(a[0], a[1]), (a[2], a[3]), (a[4], a[5]), (a[6], a[7])];
        if !argon2id::valid_input_lengths(inputs.map(|(_, len)| len)) {
            return Ok(vec![0, 0, 1]);
        }
        let Ok(plan) = Plan::new(a[8] as u32, a[9] as u32, a[10] as u32, len) else {
            return Ok(vec![0, 0, 1]);
        };
        let old_len = self.load(out + 8, 8);
        let spare = self.load(out + 16, 8).saturating_sub(old_len);
        if len > spare {
            return Ok(vec![len, spare, 2]);
        }
        self.take_steps(t, plan.steps())?;
        let payer = t.limits.last().unwrap().clone();
        let charge = plan.memory_bytes();
        if let Err(available) = self.admit(&payer, charge) {
            return Ok(vec![charge, available, 2]);
        }

        let (output, offset) = self.check_write(self.load(out, 8) + old_len, len as usize);
        let output = self.mem.slot(output).expect("live output block");
        let inputs = inputs.map(|(ptr, len)| {
            if len == 0 {
                return None;
            }
            let (block, offset) = split(ptr);
            let slot = self.mem.slot(block).expect("live input block");
            Some((slot, offset, len as usize))
        });
        let (before, output_and_after) = self.mem.entries.split_at_mut(output);
        let (tag, after) = output_and_after.split_first_mut().unwrap();
        let [password, salt, key, data] = inputs.map(|input| match input {
            None => &[][..],
            Some((slot, at, len)) => {
                assert_ne!(slot, output, "mutable output must not alias an input");
                let block = if slot < output {
                    &before[slot]
                } else {
                    &after[slot - output - 1]
                };
                &block.1.bytes[at..at + len]
            }
        });
        let result = plan.hash(
            password,
            salt,
            key,
            data,
            &mut tag.1.bytes[offset..offset + len as usize],
        );
        self.refund(payer, charge);
        match result {
            Ok(()) => {
                self.store(out + 8, 8, old_len + len);
                Ok(vec![0, 0, 0])
            }
            Err(Error::Params) => Ok(vec![0, 0, 1]),
            Err(Error::HostMemory) => std::panic::resume_unwind(Box::new(RunStop("HostMemory"))),
        }
    }
}
