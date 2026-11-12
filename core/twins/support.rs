#![allow(dead_code)]
use std::cell::RefCell;
use std::rc::Rc;

pub type Result<T> = std::result::Result<T, String>;
pub fn input() -> (String, String) {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 3, "usage: twin INPUT VARIANT");
    (
        std::fs::read_to_string(&args[1]).expect("fixture"),
        args[2].clone(),
    )
}
pub fn sum(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0u64, |n, b| n.checked_add(u64::from(*b)).unwrap())
}
pub fn finish(result: Result<()>) {
    if let Err(error) = result {
        println!("failure={error}");
        std::process::exit(if error.starts_with("Capacity") {
            3
        } else if error.starts_with("Cancelled") {
            4
        } else {
            2
        });
    }
}
#[derive(Clone, Default)]
pub struct Trace(Rc<RefCell<Vec<String>>>);
impl Trace {
    pub fn event(&self, event: impl Into<String>) {
        self.0.borrow_mut().push(event.into());
    }
    pub fn print(&self) {
        for event in self.0.borrow().iter() {
            println!("{event}");
        }
    }
}
pub struct Resource {
    name: String,
    trace: Trace,
}
impl Resource {
    pub fn new(name: impl Into<String>, trace: &Trace) -> Self {
        let name = name.into();
        trace.event(format!("acquire={name}"));
        Self {
            name,
            trace: trace.clone(),
        }
    }
}
impl Drop for Resource {
    fn drop(&mut self) {
        self.trace.event(format!("release={}", self.name));
    }
}
pub struct Admission<'a> {
    variant: &'a str,
}
impl<'a> Admission<'a> {
    pub fn new(variant: &'a str) -> Self {
        Self { variant }
    }
    pub fn at(&self, position: &str) -> Result<()> {
        if self.variant.strip_prefix("capacity:") == Some(position) {
            Err(format!("Capacity@{position}"))
        } else {
            Ok(())
        }
    }
}
