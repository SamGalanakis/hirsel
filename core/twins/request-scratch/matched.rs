mod program;
#[path = "../support.rs"]
mod support;
#[derive(Default)]
struct Scratch {
    blob: Vec<u8>,
    fields: Vec<(u32, u32, u32, u32)>,
}
impl program::Scratch for Scratch {
    fn push(&mut self, k: &str, v: &str) {
        let a = u32::try_from(self.blob.len()).unwrap();
        self.blob.extend_from_slice(k.as_bytes());
        let b = u32::try_from(self.blob.len()).unwrap();
        self.blob.extend_from_slice(v.as_bytes());
        self.fields
            .push((a, b, b, u32::try_from(self.blob.len()).unwrap()));
    }
    fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|&&(a, b, _, _)| &self.blob[a as usize..b as usize] == key.as_bytes())
            .map(|&(_, _, a, b)| std::str::from_utf8(&self.blob[a as usize..b as usize]).unwrap())
    }
}
fn main() {
    program::run::<Scratch>();
}
