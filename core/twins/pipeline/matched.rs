mod program;
#[path = "../support.rs"]
mod support;
use std::sync::mpsc::Sender;
struct Batch {
    id: usize,
    bytes: Vec<u8>,
    offsets: Vec<u32>,
    released: Sender<usize>,
}
impl Drop for Batch {
    fn drop(&mut self) {
        self.released.send(self.id).unwrap();
    }
}
impl program::Batch for Batch {
    fn new(id: usize, rows: Vec<String>, released: Sender<usize>) -> Self {
        let mut bytes = Vec::new();
        let mut offsets = vec![0];
        for row in rows {
            bytes.extend_from_slice(row.as_bytes());
            offsets.push(u32::try_from(bytes.len()).unwrap());
        }
        Self {
            id,
            bytes,
            offsets,
            released,
        }
    }
    fn upper(&mut self) {
        self.bytes = self.bytes.to_ascii_uppercase();
    }
    fn stats(&self) -> (usize, usize, u64) {
        (
            self.offsets.len() - 1,
            self.bytes.len(),
            support::sum(&self.bytes),
        )
    }
}
fn main() {
    program::run::<Batch>();
}
