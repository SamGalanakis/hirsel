// Matched to json/matched.hirsel and std.json: exact preorder indexed blob, sizing/write passes.
// Independent review pending.
mod matched_json;
mod matched_sort;
#[path = "../matched_support.rs"]
mod support;
use matched_json::Blob;
use support::*;
fn put_bytes(s: &[u8]) -> Result<()> {
    use std::io::Write;
    std::io::stdout()
        .write_all(s)
        .map_err(|_| "Io@stdout".into())
}
fn document(
    source: &str,
    path: &str,
    variant: &str,
    number: u64,
    trace: &Trace,
) -> Result<(usize, u64)> {
    let _owner = trace.marker(&name("document", number, ""));
    if variant.strip_prefix("capacity:") == Some(name("document", number, ".blob").as_str()) {
        matched_json::size(source).map_err(|e| name(&format!("{e}@document"), number, ""))?;
        admission(variant, &name("document", number, ".blob"))?;
    }
    let blob = Blob::parse(source).map_err(|e| name(&format!("{e}@document"), number, ""))?;
    let mut pretty = Vec::new();
    blob.render(&mut pretty, 0, 0, true);
    let mut answer = name("query", number, "=").into_bytes();
    blob.query(&mut answer, 0, path, 1)?;
    answer.push(b'\n');
    put_bytes(&answer)?;
    if variant == "print" {
        put_bytes(&pretty)?;
        put("\n")?;
    }
    Ok((pretty.len(), sum(&pretty)))
}
fn run(input: &str, variant: &str, trace: &Trace) -> Result<()> {
    let mut at = 0;
    let path = line(input, &mut at);
    let (mut count, mut total, mut checksum) = (0u64, 0usize, 0u64);
    let result = (|| {
        while at < input.len() {
            let (n, s) = document(line(input, &mut at), path, variant, count + 1, trace)?;
            count += 1;
            total += n;
            checksum += s;
        }
        Ok(())
    })();
    let mut out = name("documents=", count, " pretty-bytes=");
    num(&mut out, total);
    out.push_str(" sum=");
    num(&mut out, checksum);
    out.push('\n');
    put(&out)?;
    result
}
fn main() {
    let (input, variant) = match input() {
        Ok(x) => x,
        Err(_) => std::process::exit(2),
    };
    let mut at = 0;
    let mut events = 2;
    while at < input.len() {
        line(&input, &mut at);
        events += 2;
    }
    let trace = Trace::new(events);
    let result = run(&input, &variant, &trace);
    trace.finish(result);
}
