use crate::support::*;
pub trait Text: Default {
    type Root;
    fn new_text(&mut self, text: &str) -> Self::Root;
    fn snapshot(&mut self, r: &Self::Root) -> Self::Root;
    fn edit(&mut self, r: &Self::Root, pos: usize, len: usize, text: &str) -> Result<Self::Root>;
    fn render(&self, r: &Self::Root) -> String;
    fn release(&mut self, r: Self::Root);
    fn live(&self) -> usize;
}
pub fn run<T: Text>() {
    let (input, variant) = input();
    let mut lines = input.lines();
    let mut t = T::default();
    let mut root = t.new_text(lines.next().unwrap());
    let mut undo = Vec::new();
    let mut edits = 0;
    let admission = Admission::new(&variant);
    let result = (|| {
        for line in lines {
            if line == "UNDO" {
                let old = undo.pop().ok_or("EmptyUndo")?;
                t.release(std::mem::replace(&mut root, old));
                continue;
            }
            let f: Vec<_> = line.splitn(3, ' ').collect();
            let pos = f[0].parse().unwrap();
            let len = f[1].parse().unwrap();
            admission.at(&format!("edit{}.piece", edits + 1))?;
            let next = t.edit(&root, pos, len, f[2])?;
            undo.push(t.snapshot(&root));
            t.release(std::mem::replace(&mut root, next));
            edits += 1;
            if undo.len() > 64 {
                t.release(undo.remove(0));
            }
        }
        Ok(())
    })();
    let rendered = t.render(&root);
    println!(
        "edits={edits} undo={} bytes={} sum={} live={}",
        undo.len(),
        rendered.len(),
        sum(rendered.as_bytes()),
        t.live()
    );
    while let Some(r) = undo.pop() {
        t.release(r);
    }
    println!("after-history={}", t.live());
    t.release(root);
    println!("after-text={}", t.live());
    finish(result);
}
