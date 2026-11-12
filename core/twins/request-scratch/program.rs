use crate::support::*;
use std::collections::HashMap;
use std::sync::Arc;
pub trait Scratch: Default {
    fn push(&mut self, key: &str, value: &str);
    fn get(&self, key: &str) -> Option<&str>;
}
pub fn run<S: Scratch>() {
    let (input, variant) = input();
    let cfg = Arc::new(HashMap::from([("/alpha", "welcome"), ("/beta", "ready")]));
    let trace = Trace::default();
    let admission = Admission::new(&variant);
    let mut bytes = 0;
    let mut checksum = 0;
    let mut requests = 0;
    let result = (|| {
        for (i, request) in input.split("\n\n").filter(|s| !s.is_empty()).enumerate() {
            let _socket = Resource::new(format!("socket#{}", i + 1), &trace);
            {
                let _region = Resource::new(format!("scratch#{}", i + 1), &trace);
                let mut scratch = S::default();
                let mut lines = request.lines();
                let path = lines
                    .next()
                    .unwrap()
                    .strip_prefix("GET ")
                    .ok_or("Parse@method")?;
                for (h, line) in lines.enumerate() {
                    admission.at(&format!("request{}.header{}", i + 1, h + 1))?;
                    let (key, value) = line.split_once(": ").ok_or("Parse@header")?;
                    scratch.push(key, value);
                }
                let body = format!(
                    "{}:{}:{}",
                    cfg.get(path).ok_or("Route@path")?,
                    scratch.get("X-Id").ok_or("Parse@id")?,
                    scratch.get("Content").ok_or("Parse@content")?
                );
                admission.at(&format!("request{}.reply", i + 1))?;
                if variant == "error" && i == 1 {
                    return Err("Io@request2.write".into());
                }
                bytes += body.len();
                checksum += sum(body.as_bytes());
                requests += 1;
            }
        }
        Ok(())
    })();
    println!("requests={requests} bytes={bytes} sum={checksum}");
    trace.print();
    finish(result);
}
