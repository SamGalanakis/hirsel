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
}
