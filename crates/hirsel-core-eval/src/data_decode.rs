use super::*;
use hirsel_data::{DataError, ErrorKind, Limits, guest::Plan, image::Image};

impl<'m, H: Host<'m>> Machine<'m, H> {
    pub(super) fn data_decode(&mut self, t: &mut Task, a: &[u64]) -> X<Vec<u64>> {
        let limits = Limits::from(<[u64; 7]>::try_from(&a[2..9]).unwrap());
        let witness_len = self.load(a[0] + 8, 8) as usize;
        let input_len = self.load(a[1] + 8, 8) as usize;
        self.take_steps(t, hirsel_data::guest::steps(witness_len, input_len, limits))?;
        let key = hirsel_core_layout::map_hash::PreparedKey::new(&self.maps.strong_key);
        let payer = t.limits.last().unwrap().clone();
        let mut scratch = hirsel_data::guest::witness_charge(witness_len);
        if let Err(available) = self.admit(&payer, scratch) {
            return self.data_refusal(
                t,
                ErrorKind::Capacity {
                    requested: scratch,
                    available,
                },
            );
        }
        let witness = self.get(self.load(a[0], 8), witness_len);
        let (schema, plan) = match Plan::from_compiler(witness) {
            Ok(value) => value,
            Err(e) => {
                self.refund(payer, scratch);
                return self.data_refusal(t, e.kind);
            }
        };
        let input = self.load(a[1], 8);
        let (id, offset) = if input_len == 0 { (0, 0) } else { split(input) };
        // Move the Vec header out while admissions borrow the machine. The
        // read is synchronous and its input bytes are neither copied nor moved.
        let backing = if input_len == 0 {
            Vec::new()
        } else {
            std::mem::take(&mut self.mem[id].bytes)
        };
        let source = if input_len == 0 {
            &[]
        } else {
            &backing[offset..offset + input_len]
        };
        let image = Image::prepare(&schema, &plan, source, limits, key, &mut |requested| {
            self.admit(&payer, requested).map_err(|available| {
                DataError::resource(ErrorKind::Capacity {
                    requested,
                    available,
                })
            })?;
            scratch += requested;
            Ok(())
        });
        if input_len != 0 {
            self.mem[id].bytes = backing;
        }
        let result = match image {
            Ok(mut image) => {
                let mut result = Ok(());
                for i in 1..image.blocks.len() {
                    let b = &mut image.blocks[i];
                    if let Err(available) = self.admit(&payer, b.charge) {
                        let requested = b.charge;
                        for b in &image.blocks[1..i] {
                            self.free(b.address);
                        }
                        result = Err(ErrorKind::Capacity {
                            requested,
                            available,
                        });
                        break;
                    }
                    let ptr = self.block_with_bytes(
                        std::mem::take(&mut b.bytes),
                        Some(payer.clone()),
                        b.charge,
                        None,
                    );
                    self.mem[split(ptr).0].align = b.align;
                    self.mem[split(ptr).0].class = Some(Some(if b.graph {
                        Cleanup::Dynamic
                    } else {
                        Cleanup::Terminal
                    }));
                    b.address = ptr;
                    let (parent, offset) = b.parent.unwrap();
                    if parent == 0 {
                        image.blocks[0].bytes[offset..offset + 8]
                            .copy_from_slice(&ptr.to_le_bytes());
                    } else {
                        self.store(image.blocks[parent].address + offset as u64, 8, ptr);
                    }
                }
                if result.is_ok() {
                    self.put(a[9], &image.blocks[0].bytes);
                }
                result
            }
            Err(e) => Err(e.kind),
        };
        self.refund(payer, scratch);
        match result {
            Ok(()) => Ok(Vec::new()),
            Err(kind) => self.data_refusal(t, kind),
        }
    }

    fn data_refusal(&mut self, t: &mut Task, kind: ErrorKind) -> X<Vec<u64>> {
        match kind {
            ErrorKind::Capacity {
                requested,
                available,
            } => {
                let site = self.site(t);
                self.record(
                    t,
                    Cause {
                        kind: "Capacity",
                        message: [requested, available, 0],
                        site,
                    },
                );
                Err(Stop::Chained)
            }
            ErrorKind::HostMemory => std::panic::resume_unwind(Box::new(RunStop("HostMemory"))),
            _ => unreachable!("document errors are guest values"),
        }
    }
}
