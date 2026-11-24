use super::*;

impl Machine<'_> {
    pub(super) fn read_call_payload(
        &mut self,
        key: u64,
        index: u64,
        max: u64,
        out: u64,
    ) -> X<Vec<u64>> {
        let (port, id) = Self::call_of(key);
        let VirtualCell::Port { calls, .. } = &self.world.cells[port] else {
            unreachable!("a port");
        };
        let call = &calls[&id];
        if call.finished || call.answer.is_some() {
            return Ok(vec![0; 4]);
        }
        let field = call
            .fields
            .get(index as usize)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let field_len = field.len() as u64;
        let count = field_len.min(max);
        let len = self.load(out + 8, 8);
        let cap = self.load(out + 16, 8);
        if cap.saturating_sub(len) < count {
            return Err(Stop::Cause("Bounds"));
        }
        if count != 0 {
            let bytes = field[..count as usize].to_vec();
            let data = self.load(out, 8);
            self.put(data + len, &bytes);
            self.store(out + 8, 8, len + count);
        }
        Ok(vec![field_len - count, 0, 0, count])
    }

    pub(super) fn dir_call(
        &mut self,
        t: &Task,
        op: HostOp,
        c: usize,
        h: u64,
        a: &[u64],
    ) -> X<Vec<u64>> {
        self.check_cancel(t).inspect_err(|_| {
            // A close cancelled before issue still ends the file: the server
            // learns by Released (native `host_file` CLOSE under a shield).
            if op == HostOp::Close {
                self.port_release(a[0]);
            }
        })?;
        if matches!(op, HostOp::Write | HostOp::WriteAt)
            && let VirtualCell::Port { handles, .. } = &self.world.cells[c]
            && handles
                .get(&h)
                .is_some_and(|file| file.read_only_file && file.state == PortState::Live)
        {
            return Ok(vec![0, DENIED, 0, 0]);
        }
        let spare = |m: &Self, carrier: u64| {
            let (_, _, len, cap) = m.carrier(carrier);
            cap.saturating_sub(len)
        };
        // The caller-side checks of every provider's entry (native `drive`).
        match op {
            HostOp::Mkdtemp | HostOp::Mkstemp if spare(self, a[1]) < 32 => {
                return Err(Stop::Cause("Bounds"));
            }
            HostOp::NextEntries if spare(self, a[1]) < 258 => return Err(Stop::Cause("Bounds")),
            HostOp::WriteAt
                if a[1]
                    .checked_add(self.carrier_bytes(a[2]).len() as u64)
                    .is_none() =>
            {
                return Err(Stop::Cause("Bounds"));
            }
            _ => {}
        }
        let path = |m: &Self, i: usize| m.carrier_bytes(a[i]).to_vec();
        let (kind, words, fields, out): (u8, Vec<u64>, Vec<Vec<u8>>, Option<u64>) = match op {
            HostOp::Open => {
                let p = path(self, 1);
                (0, vec![0, h, p.len() as u64, a[2]], vec![vec![], p], None)
            }
            HostOp::OpenDir
            | HostOp::Stat
            | HostOp::Mkdir
            | HostOp::Remove
            | HostOp::RemoveTree => {
                let tag = match op {
                    HostOp::OpenDir => 1,
                    HostOp::Stat => 3,
                    HostOp::Mkdir => 4,
                    HostOp::Remove => 5,
                    _ => 6,
                };
                let p = path(self, 1);
                (0, vec![tag, h, p.len() as u64], vec![vec![], p], None)
            }
            HostOp::Mkdtemp | HostOp::Mkstemp => {
                let (prefix, ..) = self.carrier(a[1]);
                let room = spare(self, a[1]);
                let tag = if op == HostOp::Mkstemp {
                    DIR_MKSTEMP
                } else {
                    2
                };
                (
                    0,
                    vec![tag, h, prefix.len() as u64, room],
                    vec![vec![], prefix],
                    Some(a[1]),
                )
            }
            HostOp::Rename => {
                let (from, to) = (path(self, 1), path(self, 2));
                (
                    0,
                    vec![7, h, from.len() as u64, to.len() as u64],
                    vec![vec![], from, to],
                    None,
                )
            }
            HostOp::SyncDir => (0, vec![8, h], vec![], None),
            HostOp::ZeroExtension => (0, vec![DIR_ZERO_EXTENSION, h], vec![], None),
            HostOp::Entries => {
                let p = if a[1] == 0 { vec![] } else { path(self, 1) };
                (0, vec![9, h, p.len() as u64], vec![vec![], p], None)
            }
            HostOp::Read => (1, vec![10, h, spare(self, a[1])], vec![], Some(a[1])),
            HostOp::ReadAt => (1, vec![11, h, a[1], spare(self, a[2])], vec![], Some(a[2])),
            HostOp::Write => {
                let (b, ..) = self.carrier(a[1]);
                (1, vec![12, h, b.len() as u64], vec![vec![], b], None)
            }
            HostOp::WriteAt => {
                let (b, ..) = self.carrier(a[2]);
                (
                    1,
                    vec![13, h, a[1], b.len() as u64],
                    vec![vec![], vec![], b],
                    None,
                )
            }
            HostOp::StatFile => (1, vec![14, h], vec![], None),
            HostOp::Sync => (1, vec![15, h], vec![], None),
            HostOp::TryLock => (1, vec![16, h], vec![], None),
            HostOp::Unlock => (1, vec![17, h], vec![], None),
            HostOp::Close => (1, vec![DIR_CLOSE, h], vec![], None),
            HostOp::NextEntries => (2, vec![19, h, spare(self, a[1])], vec![], Some(a[1])),
            _ => unreachable!("a Dir, file or listing operation"),
        };
        let VirtualCell::Port { handles, .. } = &self.world.cells[c] else {
            unreachable!()
        };
        let live = handles.get(&h).is_some_and(|handle| {
            handle.kind == kind && handle.state == PortState::Live && handle.write.is_none()
        });
        let mut w = [0; 9];
        w[..words.len()].copy_from_slice(&words);
        let mut answer = self.port_call(t, c, w, fields, out, live)?;
        if a[0] & DIR_READ_ONLY != 0 && answer[1] == 0 {
            if matches!(op, HostOp::OpenDir | HostOp::Mkdtemp) {
                answer[0] |= DIR_READ_ONLY;
            } else if op == HostOp::Open {
                let (_, file) = self.port_handle(answer[0]).expect("a registered file");
                let (_, handles, ..) = self.world.cells[c].port();
                handles.get_mut(&file).unwrap().read_only_file = true;
            }
        }
        Ok(answer)
    }
}

impl Machine<'_> {
    pub(super) fn provide_random_tape(&mut self, task: &Task, source: u64) -> Vec<u64> {
        let charge = hirsel_core_layout::charge::PROVIDE_CELL + self.load(source + 8, 8);
        let payer = task.limits.last().unwrap();
        if let Err(available) = self.admit(payer, charge) {
            return vec![charge, 1, 0, available];
        }
        let tape = RandomStream::tape(self.carrier_bytes(source).to_vec());
        let handle = CELL_BASE + self.world.cells.len() as u64;
        self.world.cells.push(VirtualCell::Random(tape));
        self.world.grant_charges.push((payer.clone(), charge));
        vec![handle, 0, 0, 0]
    }

    pub(super) fn draw_virtual_random(&mut self, cell: usize, carrier: u64) -> X<Vec<u64>> {
        let (_, data, len, cap) = self.carrier(carrier);
        let spare = cap.saturating_sub(len) as usize;
        if spare == 0 {
            return Ok(vec![]);
        }
        let (block, offset) = self.check_write(data + len, spare);
        let VirtualCell::Random(stream) = &mut self.world.cells[cell] else {
            unreachable!()
        };
        let bytes = &mut self.mem[block].bytes[offset..offset + spare];
        // SAFETY: MaybeUninit<u8> has the layout of u8 and draw only writes initialized bytes.
        let out = unsafe { std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast(), spare) };
        if !stream.draw(out) {
            return Err(Stop::Cause("Bounds"));
        }
        self.store(carrier + 8, 8, len + spare as u64);
        Ok(vec![])
    }
}
