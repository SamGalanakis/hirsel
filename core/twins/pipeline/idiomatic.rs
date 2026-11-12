mod program;
#[path = "../support.rs"]
mod support;
use std::sync::mpsc::Sender;
struct Batch {
    id: usize,
    rows: Vec<String>,
    released: Sender<usize>,
}
impl Drop for Batch {
    fn drop(&mut self) {
        self.released.send(self.id).unwrap();
    }
}
impl program::Batch for Batch {
    fn new(id: usize, rows: Vec<String>, released: Sender<usize>) -> Self {
        Self { id, rows, released }
    }
    fn upper(&mut self) {
        self.rows = self
            .rows
            .iter()
            .map(|row| row.to_ascii_uppercase())
            .collect();
    }
    fn stats(&self) -> (usize, usize, u64) {
        (
            self.rows.len(),
            self.rows.iter().map(String::len).sum(),
            self.rows.iter().map(|s| support::sum(s.as_bytes())).sum(),
        )
    }
}
fn main() {
    program::run::<Batch>();
}
