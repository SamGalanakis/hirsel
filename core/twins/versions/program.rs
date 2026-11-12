use crate::support::*;
pub trait Store: Default {
    type Root;
    fn build(&mut self, n: u32) -> Self::Root;
    fn pin(&mut self, r: &Self::Root) -> Self::Root;
    fn set(
        &mut self,
        r: &Self::Root,
        key: u32,
        value: u64,
        admission: &Admission,
    ) -> Result<Self::Root>;
    fn get(&self, r: &Self::Root, key: u32) -> u64;
    fn release(&mut self, r: Self::Root);
    fn live(&self) -> usize;
}
pub fn run<S: Store>() {
    let (input, variant) = input();
    let admission = Admission::new(&variant);
    let mut s = S::default();
    let mut lines = input.lines();
    let n: u32 = lines.next().unwrap().parse().unwrap();
    let mut root = s.build(n);
    let pin = s.pin(&root);
    let mut updates = 0;
    let result = (|| {
        for line in lines {
            let fields: Vec<u64> = line
                .split_whitespace()
                .map(|s| s.parse().unwrap())
                .collect();
            if fields[0] >= u64::from(n) {
                return Err("InvalidKey".into());
            }
            let next = s.set(&root, fields[0] as u32, fields[1], &admission)?;
            s.release(std::mem::replace(&mut root, next));
            updates += 1;
        }
        Ok(())
    })();
    let old = (0..n).map(|k| s.get(&pin, k)).sum::<u64>();
    let current = (0..n).map(|k| s.get(&root, k)).sum::<u64>();
    println!(
        "updates={updates} pinned={old} current={current} live={}",
        s.live()
    );
    s.release(pin);
    println!("after-unpin={}", s.live());
    s.release(root);
    println!("after-current={}", s.live());
    finish(result);
}
