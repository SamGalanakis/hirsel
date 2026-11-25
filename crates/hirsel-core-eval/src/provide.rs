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
        let VirtualCell::Port { calls, .. } = &self.host.world.cells[port] else {
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
        if matches!(op, HostOp::Write | HostOp::WriteAt | HostOp::ZeroAt)
            && let VirtualCell::Port { handles, .. } = &self.host.world.cells[c]
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
        if matches!(op, HostOp::WriteAt | HostOp::ZeroAt)
            && let VirtualCell::Port { handles, .. } = &self.host.world.cells[c]
            && let Some(handle) = handles.get(&h)
        {
            if op == HostOp::ZeroAt {
                if !matches!(handle.file_mode, 1 | 3 | 4 | MODE_DURABLE | MODE_DIRECT) {
                    return Ok(vec![0, DENIED, 0, 0]);
                }
                if let Some((tag, code)) = zero_at_error(handle.file_mode, a[1], a[2]) {
                    return Ok(vec![0, tag, code, 0]);
                }
            } else if handle.file_mode == MODE_DIRECT {
                let (_, data, len, _) = self.carrier(a[2]);
                if !a[1].is_multiple_of(DIRECT_ALIGNMENT)
                    || !len.is_multiple_of(DIRECT_ALIGNMENT)
                    || !self.direct_buffer_aligned(data)
                {
                    return Ok(vec![0, OTHER, 22, 0]);
                }
            }
        }
        let path = |m: &Self, i: usize| m.carrier_bytes(a[i]).to_vec();
        let dir_op = DirOp::from_host(op.code()).expect("a Dir, file or listing operation");
        let (kind, fields, payload, out): (u8, [u64; 3], Vec<Vec<u8>>, Option<u64>) = match dir_op {
            DirOp::Open => {
                let p = path(self, 1);
                (0, [h, p.len() as u64, a[2]], vec![vec![], p], None)
            }
            DirOp::OpenDir | DirOp::Stat | DirOp::Mkdir | DirOp::Remove | DirOp::RemoveTree => {
                let p = path(self, 1);
                (0, [h, p.len() as u64, 0], vec![vec![], p], None)
            }
            DirOp::Mkdtemp | DirOp::Mkstemp => {
                let (prefix, ..) = self.carrier(a[1]);
                let room = spare(self, a[1]);
                (
                    0,
                    [h, prefix.len() as u64, room],
                    vec![vec![], prefix],
                    Some(a[1]),
                )
            }
            DirOp::Rename => {
                let (from, to) = (path(self, 1), path(self, 2));
                (
                    0,
                    [h, from.len() as u64, to.len() as u64],
                    vec![vec![], from, to],
                    None,
                )
            }
            DirOp::SyncDir | DirOp::ZeroExtension => (0, [h, 0, 0], vec![], None),
            DirOp::Entries => {
                let p = if a[1] == 0 { vec![] } else { path(self, 1) };
                (0, [h, p.len() as u64, 0], vec![vec![], p], None)
            }
            DirOp::Read => (1, [h, spare(self, a[1]), 0], vec![], Some(a[1])),
            DirOp::ReadAt => (1, [h, a[1], spare(self, a[2])], vec![], Some(a[2])),
            DirOp::Write => {
                let (b, ..) = self.carrier(a[1]);
                (1, [h, b.len() as u64, 0], vec![vec![], b], None)
            }
            DirOp::WriteAt => {
                let (b, ..) = self.carrier(a[2]);
                (1, [h, a[1], b.len() as u64], vec![vec![], vec![], b], None)
            }
            DirOp::ZeroAt => (1, [h, a[1], a[2]], vec![], None),
            DirOp::StatFile
            | DirOp::Sync
            | DirOp::TryLock
            | DirOp::TryLockShared
            | DirOp::Unlock
            | DirOp::Close => (1, [h, 0, 0], vec![], None),
            DirOp::NextEntries => (2, [h, spare(self, a[1]), 0], vec![], Some(a[1])),
            DirOp::Released => unreachable!("a notice is queued at a drop"),
        };
        let VirtualCell::Port { handles, .. } = &self.host.world.cells[c] else {
            unreachable!()
        };
        let live = handles.get(&h).is_some_and(|handle| {
            handle.kind == kind && handle.state == PortState::Live && handle.write.is_none()
        });
        let mut w = [0; 9];
        w[0] = dir_op.word();
        w[1..4].copy_from_slice(&fields);
        let mut answer = self.port_call(t, c, w, payload, out, live)?;
        if answer[1] == 0 {
            let narrow = a[0] & DIR_READ_ONLY != 0;
            if op == HostOp::Open {
                let (_, file) = self.port_handle(answer[0]).expect("a registered file");
                let (_, handles, ..) = self.host.world.cells[c].port();
                let handle = handles.get_mut(&file).unwrap();
                handle.read_only_file = narrow;
                handle.file_mode = a[2] as u8;
            } else if narrow && matches!(op, HostOp::OpenDir | HostOp::Mkdtemp) {
                answer[0] |= DIR_READ_ONLY;
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
        let handle = CELL_BASE + self.host.world.cells.len() as u64;
        self.host.world.cells.push(VirtualCell::Random(tape));
        self.host.world.grant_charges.push((payer.clone(), charge));
        vec![handle, 0, 0, 0]
    }

    pub(super) fn draw_virtual_random(&mut self, cell: usize, carrier: u64) -> X<Vec<u64>> {
        let (_, data, len, cap) = self.carrier(carrier);
        let spare = cap.saturating_sub(len) as usize;
        if spare == 0 {
            return Ok(vec![]);
        }
        let (block, offset) = self.check_write(data + len, spare);
        let VirtualCell::Random(stream) = &mut self.host.world.cells[cell] else {
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

pub(super) fn zero_at_error(mode: u8, at: u64, len: u64) -> Option<(u64, u64)> {
    if mode == MODE_DIRECT
        && (!at.is_multiple_of(DIRECT_ALIGNMENT) || !len.is_multiple_of(DIRECT_ALIGNMENT))
    {
        Some((OTHER, 22))
    } else if at.checked_add(len).is_none_or(|end| end > i64::MAX as u64) {
        Some((OTHER, 27))
    } else {
        None
    }
}
