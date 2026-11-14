// Matched to request-scratch/matched.hirsel: counted fixed header span and owned byte arena.
// Independent review pending.
#[path = "../matched_map.rs"]
mod matched_map;
#[path = "../matched_support.rs"]
mod support;
use matched_map::Map;
use std::sync::Arc;
use support::*;
struct Config {
    routes: Map<String>,
}
#[derive(Clone, Copy)]
struct Header {
    name_lo: u32,
    name_hi: u32,
    value_lo: u32,
    value_hi: u32,
}
fn header_value<'a>(s: &'a String, hs: &[Header], key: &str) -> Option<View<'a>> {
    for h in hs {
        if s.view()
            .slice(h.name_lo as usize, h.name_hi as usize)
            .as_bytes()
            == key.as_bytes()
        {
            return Some(s.view().slice(h.value_lo as usize, h.value_hi as usize));
        }
    }
    None
}
fn request(
    block: View<'_>,
    variant: &str,
    number: u64,
    config: Arc<Config>,
    trace: &Trace,
) -> Result<(usize, u64)> {
    let _socket = trace.marker(&name("socket#", number, ""));
    let _scratch = trace.marker(&name("scratch#", number, ""));
    let mut at = 0;
    let route = line(block, &mut at)
        .strip_prefix("GET ")
        .ok_or("Parse@method")?;
    let start = at;
    let mut count = 0usize;
    while at < block.len() {
        line(block, &mut at);
        count += 1;
    }
    let mut headers = Vec::with_capacity(count);
    let mut arena = String::new();
    at = start;
    let mut h = 0u64;
    while at < block.len() {
        let row = line(block, &mut at);
        h += 1;
        let mut site = name("request", number, ".header");
        num(&mut site, h);
        admission(variant, &site, 0)?;
        let colon = row.find(": ").ok_or("Parse@header")?;
        let name_lo = narrow(arena.len());
        arena.push_str(row.slice(0, colon));
        let name_hi = narrow(arena.len());
        arena.push_str(row.slice(colon + 2, row.len()));
        let value_hi = narrow(arena.len());
        headers.push(Header {
            name_lo,
            name_hi,
            value_lo: name_hi,
            value_hi,
        });
    }
    let banner = config.routes.get(route).ok_or("Route@path")?;
    let id = header_value(&arena, &headers, "X-Id").ok_or("Parse@id")?;
    let content = header_value(&arena, &headers, "Content").ok_or("Parse@content")?;
    let size = banner.len() + id.len() + content.len() + 2;
    admission(variant, &name("request", number, ".reply"), size as u64)?;
    let mut reply = String::with_capacity(size);
    reply.push_str(banner);
    reply.push(':');
    reply.push_str(id);
    reply.push(':');
    reply.push_str(content);
    if variant == "error" && number == 2 {
        return Err("Io@request2.write".into());
    }
    Ok((reply.len(), sum(reply.as_bytes())))
}
fn run(input: View<'_>, variant: &str, trace: &Trace) -> Result<()> {
    let mut routes = Map::new();
    routes.insert(String::literal("/alpha"), String::literal("welcome"));
    routes.insert(String::literal("/beta"), String::literal("ready"));
    let config = Arc::new(Config { routes });
    let mut requests = 0u64;
    let mut bytes = 0usize;
    let mut checksum = 0u64;
    let mut at = 0;
    let result = (|| {
        while at < input.len() {
            let end = input
                .slice(at, input.len())
                .find("\n\n")
                .map_or(input.len(), |p| at + p);
            if end > at {
                let (n, s) = request(
                    input.slice(at, end),
                    variant,
                    requests + 1,
                    config.clone(),
                    trace,
                )?;
                bytes += n;
                checksum += s;
                requests += 1;
            }
            at = if end < input.len() { end + 2 } else { end };
        }
        Ok(())
    })();
    let mut out = name("requests=", requests, " bytes=");
    num(&mut out, bytes);
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
    let mut events = 4;
    while at < input.len() {
        if line(&input, &mut at).starts_with("GET ") {
            events += 4;
        }
    }
    let trace = Trace::new(events);
    let result = run(input.view(), &variant, &trace);
    trace.finish(result);
}
