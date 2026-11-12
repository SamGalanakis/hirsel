mod program;
#[path = "../support.rs"]
mod support;
#[derive(Default)]
struct Scratch(Vec<(String, String)>);
impl program::Scratch for Scratch {
    fn push(&mut self, k: &str, v: &str) {
        self.0.push((k.into(), v.into()));
    }
    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}
fn main() {
    program::run::<Scratch>();
}
