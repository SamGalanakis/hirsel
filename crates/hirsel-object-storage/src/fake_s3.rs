//! A fake S3 server for tests (feature `testing`): path-style GET, PUT,
//! DELETE and ListObjectsV2 on one bucket in memory, with S3's conditional
//! write semantics as AWS documents them (`If-None-Match: *` on an existing
//! key is 412; `If-Match` is 412 on another ETag and 404 on an absent key) and
//! faults drawn from a seeded SplitMix64 stream, as `sim::Bucket` draws them:
//!
//! - `refuse`: 503 before any effect;
//! - `lost_response`: the effect happens, then the connection closes with no
//!   response, a truncated one or a 500 (a further draw picks which);
//! - `incomplete_listing`: each key of a page is dropped with probability 1/2;
//! - `slow`: the effect happens and the response waits `slow_ms` first;
//! - `wrong_etag`: a PUT's response carries an ETag the object does not have.
//!
//! It checks that every request is signed (an `AWS4-HMAC-SHA256` authorization
//! for the expected access key) and that `x-amz-content-sha256` is the body's
//! digest, but does not recompute signatures: the AWS vectors test the signer.
//! It counts requests. Listings are chunked, other bodies length-delimited, so
//! both framings are exercised. A thread serves each connection (test code).

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Fault names [`FakeS3`] draws.
pub const FAULTS: [&str; 5] = [
    "incomplete_listing",
    "lost_response",
    "refuse",
    "slow",
    "wrong_etag",
];

/// The fake's state, shared with its connection threads.
pub struct World {
    pub objects: BTreeMap<String, Vec<u8>>,
    /// Requests received.
    pub requests: u64,
    draws: u64,
    pub faults: BTreeMap<String, u64>,
    /// When false, `If-None-Match: *` is ignored (Garage 2.4.1's behaviour).
    pub honour_if_none_match: bool,
    /// When false, `If-Match` is ignored.
    pub honour_if_match: bool,
    /// How long a `slow` response waits.
    pub slow_ms: u64,
    /// Close each connection after one response.
    pub close_after_each: bool,
}

impl World {
    fn draw(&mut self) -> u64 {
        self.draws = self.draws.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.draws;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn fault(&mut self, name: &str) -> bool {
        let one_in = self.faults.get(name).copied().unwrap_or(0);
        one_in != 0 && self.draw().is_multiple_of(one_in)
    }
}

/// The ETag a fake object's bytes have.
pub fn etag(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    let hex: String = d[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

/// A running fake on 127.0.0.1.
pub struct FakeS3 {
    pub addr: SocketAddr,
    pub world: Arc<Mutex<World>>,
    pub bucket: String,
    pub access_key: String,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for FakeS3 {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        // Wake the accept loop so it sees the flag.
        let _ = TcpStream::connect(self.addr);
    }
}

impl FakeS3 {
    /// Serve `bucket` with faults `faults` drawn from `seed`.
    pub fn start(seed: u64, faults: BTreeMap<String, u64>) -> FakeS3 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake s3");
        let addr = listener.local_addr().unwrap();
        let world = Arc::new(Mutex::new(World {
            objects: BTreeMap::new(),
            requests: 0,
            draws: seed,
            faults,
            honour_if_none_match: true,
            honour_if_match: true,
            slow_ms: 0,
            close_after_each: false,
        }));
        let fake = FakeS3 {
            addr,
            world: world.clone(),
            bucket: "fake-bucket".into(),
            access_key: "AKIDFAKE".into(),
            stop: Arc::default(),
        };
        let (bucket, access) = (fake.bucket.clone(), fake.access_key.clone());
        let stop = fake.stop.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                let Ok(stream) = stream else { continue };
                let _ = stream.set_nodelay(true);
                let (world, bucket, access) = (world.clone(), bucket.clone(), access.clone());
                std::thread::spawn(move || {
                    let _ = serve(stream, &world, &bucket, &access);
                });
            }
        });
        fake
    }

    pub fn requests(&self) -> u64 {
        self.world.lock().unwrap().requests
    }

    /// Write a credentials file into `dir` and return the provider entry
    /// for this fake under `prefix`.
    pub fn entry(
        &self,
        dir: &std::path::Path,
        prefix: &str,
        request_timeout_ms: u64,
    ) -> serde_json::Value {
        let creds = dir.join("s3-credentials.json");
        std::fs::write(
            &creds,
            format!(
                r#"{{"access_key":"{}","secret_key":"fake-secret"}}"#,
                self.access_key
            ),
        )
        .unwrap();
        serde_json::json!({
            "provider": "objects.s3@1",
            "endpoint": format!("http://{}", self.addr),
            "bucket": self.bucket,
            "region": "us-east-1",
            "prefix": prefix,
            "credentials": creds,
            "connect_timeout_ms": 2000,
            "request_timeout_ms": request_timeout_ms,
        })
    }
}

struct Head {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn get(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

fn read_head(reader: &mut BufReader<TcpStream>) -> std::io::Result<Option<Head>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.trim_end().split(' ');
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let mut headers = Vec::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((n, v)) = l.split_once(':') {
            headers.push((n.trim().to_string(), v.trim().to_string()));
        }
    }
    Ok(Some(Head {
        method,
        target,
        headers,
    }))
}

fn respond(
    out: &mut TcpStream,
    status: u16,
    headers: &[(&str, String)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut text = format!("HTTP/1.1 {status} X\r\n");
    for (n, v) in headers {
        text.push_str(&format!("{n}: {v}\r\n"));
    }
    text.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut all = text.into_bytes();
    all.extend_from_slice(body);
    out.write_all(&all)
}

fn respond_chunked(out: &mut TcpStream, status: u16, body: &[u8]) -> std::io::Result<()> {
    let mut text = format!("HTTP/1.1 {status} OK\r\nTransfer-Encoding: chunked\r\n\r\n");
    for chunk in body.chunks(700) {
        text.push_str(&format!("{:x};ext=1\r\n", chunk.len()));
        text.push_str(std::str::from_utf8(chunk).unwrap());
        text.push_str("\r\n");
    }
    text.push_str("0\r\n\r\n");
    out.write_all(text.as_bytes())
}

fn query(target: &str) -> (String, BTreeMap<String, String>) {
    let (path, q) = target.split_once('?').unwrap_or((target, ""));
    let decode = |s: &str| {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && i + 2 < b.len() {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).expect("percent escape"));
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).expect("UTF-8 query")
    };
    let params = q
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (k.to_string(), decode(v))
        })
        .collect();
    (path.to_string(), params)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn serve(
    stream: TcpStream,
    world: &Mutex<World>,
    bucket: &str,
    access: &str,
) -> std::io::Result<()> {
    let mut out = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    while let Some(head) = read_head(&mut reader)? {
        let length: usize = head
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        let mut w = world.lock().unwrap();
        w.requests += 1;
        let signed = head
            .get("authorization")
            .is_some_and(|a| a.starts_with(&format!("AWS4-HMAC-SHA256 Credential={access}/")));
        let digest: String = Sha256::digest(&body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if !signed
            || head.get("x-amz-content-sha256") != Some(digest.as_str())
            || head.get("x-amz-date").is_none()
        {
            drop(w);
            respond(
                &mut out,
                403,
                &[],
                b"<Error><Code>AccessDenied</Code></Error>",
            )?;
            continue;
        }
        if w.fault("refuse") {
            drop(w);
            respond(&mut out, 503, &[], b"<Error><Code>SlowDown</Code></Error>")?;
            continue;
        }
        let (path, params) = query(&head.target);
        let root = format!("/{bucket}");
        let key = path.strip_prefix(&format!("{root}/")).map(str::to_string);
        // Apply the request, then decide its response's fate.
        let mut extra: Vec<(&str, String)> = Vec::new();
        let (status, resp_body, chunked): (u16, Vec<u8>, bool) = match (head.method.as_str(), key) {
            ("GET", None)
                if path == root && params.get("list-type").map(String::as_str) == Some("2") =>
            {
                let prefix = params.get("prefix").cloned().unwrap_or_default();
                let after = params.get("start-after").cloned().unwrap_or_default();
                let max: usize = params
                    .get("max-keys")
                    .and_then(|m| m.parse().ok())
                    .unwrap_or(1000)
                    .min(1000);
                let mut keys: Vec<String> = w
                    .objects
                    .keys()
                    .filter(|k| k.starts_with(&prefix) && **k > after)
                    .take(max + 1)
                    .cloned()
                    .collect();
                let truncated = keys.len() > max;
                keys.truncate(max);
                if w.fault("incomplete_listing") {
                    keys.retain(|_| w.draw() & 1 == 0);
                }
                let mut xml = String::from(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
                );
                xml.push_str(&format!("<Name>{bucket}</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount><MaxKeys>{max}</MaxKeys><IsTruncated>{truncated}</IsTruncated>", escape(&prefix), keys.len()));
                for k in &keys {
                    let tag = etag(&w.objects[k]).replace('"', "&quot;");
                    xml.push_str(&format!("<Contents><Key>{}</Key><ETag>{tag}</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>", escape(k), w.objects[k].len()));
                }
                xml.push_str("</ListBucketResult>");
                (200, xml.into_bytes(), true)
            }
            ("GET", Some(k)) => match w.objects.get(&k) {
                Some(b) => {
                    extra.push(("ETag", etag(b)));
                    (200, b.clone(), false)
                }
                None => (
                    404,
                    b"<Error><Code>NoSuchKey</Code></Error>".to_vec(),
                    false,
                ),
            },
            ("PUT", Some(k)) => {
                let current = w.objects.get(&k).map(|b| etag(b));
                let none_match = head.get("if-none-match") == Some("*") && w.honour_if_none_match;
                let if_match = head
                    .get("if-match")
                    .filter(|_| w.honour_if_match)
                    .map(str::to_string);
                if none_match && current.is_some() {
                    (
                        412,
                        b"<Error><Code>PreconditionFailed</Code></Error>".to_vec(),
                        false,
                    )
                } else if let Some(want) = if_match {
                    match current {
                        None => (
                            404,
                            b"<Error><Code>NoSuchKey</Code></Error>".to_vec(),
                            false,
                        ),
                        Some(c) if c != want => (
                            412,
                            b"<Error><Code>PreconditionFailed</Code></Error>".to_vec(),
                            false,
                        ),
                        Some(_) => {
                            extra.push(("ETag", etag(&body)));
                            w.objects.insert(k, body);
                            (200, Vec::new(), false)
                        }
                    }
                } else {
                    extra.push(("ETag", etag(&body)));
                    w.objects.insert(k, body);
                    (200, Vec::new(), false)
                }
            }
            ("DELETE", Some(k)) => {
                w.objects.remove(&k);
                (204, Vec::new(), false)
            }
            _ => (
                400,
                b"<Error><Code>InvalidRequest</Code></Error>".to_vec(),
                false,
            ),
        };
        if w.fault("lost_response") {
            let how = w.draw() % 3;
            drop(w);
            match how {
                0 => {}
                1 => {
                    // A head promising more body than arrives.
                    let _ = out.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nETag: \"x\"\r\n\r\nshort",
                    );
                }
                _ => {
                    let _ = respond(
                        &mut out,
                        500,
                        &[],
                        b"<Error><Code>InternalError</Code></Error>",
                    );
                }
            }
            return Ok(());
        }
        if status == 200 && head.method == "PUT" && w.fault("wrong_etag") {
            extra.retain(|(n, _)| *n != "ETag");
            extra.push(("ETag", "\"not-the-objects-tag\"".into()));
        }
        let slow = w.fault("slow").then_some(w.slow_ms);
        let close = w.close_after_each;
        drop(w);
        if let Some(ms) = slow {
            std::thread::sleep(Duration::from_millis(ms));
        }
        if close {
            extra.push(("Connection", "close".into()));
        }
        if chunked {
            respond_chunked(&mut out, status, &resp_body)?;
        } else {
            respond(&mut out, status, &extra, &resp_body)?;
        }
        if close {
            return Ok(());
        }
    }
    Ok(())
}
