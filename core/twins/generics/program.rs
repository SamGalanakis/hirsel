use crate::support::*;
pub trait OrderedCounts: Default {
    fn add(&mut self, key: &str);
    fn entries(&self) -> Vec<(&str, u64)>;
}
fn map<T, U>(xs: &[T], mut f: impl FnMut(&T) -> Result<U>) -> Result<Vec<U>> {
    xs.iter().map(&mut f).collect()
}
fn count_if<T>(xs: &[T], mut f: impl FnMut(&T) -> bool) -> usize {
    xs.iter().filter(|x| f(x)).count()
}
pub fn run<M: OrderedCounts>() {
    let (input, variant) = input();
    let trace = Trace::default();
    let admission = Admission::new(&variant);
    let result = (|| {
        let _xs = Resource::new("list", &trace);
        let xs: Vec<_> = input.lines().map(String::from).collect();
        let _mapped = Resource::new("mapped", &trace);
        let mut position = 0;
        let ys = map(&xs, |s| {
            position += 1;
            admission.at(&format!("callback{position}.string"))?;
            if s == "!error" {
                return Err(format!("Callback@{position}"));
            }
            Ok(format!("{s}!"))
        })?;
        let _map = Resource::new("map", &trace);
        let mut counts = M::default();
        for (i, s) in ys.iter().enumerate() {
            admission.at(&format!("tally{}.key", i + 1))?;
            counts.add(s);
        }
        println!(
            "long={} bytes={}",
            count_if(&ys, |s| s.len() >= 6),
            ys.iter().map(String::len).sum::<usize>()
        );
        for (k, v) in counts.entries() {
            println!("{k}={v}");
        }
        Ok(())
    })();
    trace.print();
    finish(result);
}
