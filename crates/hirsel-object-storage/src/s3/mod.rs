//! `objects.s3@1`: objects in an S3-compatible bucket, path-style, over
//! hand-written HTTP/1.1 ([`http`]) signed with SigV4 ([`sigv4`]).
//!
//! An object key `k` is the S3 key `<prefix>/k` (or `k` with no prefix) in the
//! entry's bucket. `put_new` is a PUT with `If-None-Match: *` (412 is
//! `Exists`); `replace` adds `If-None-Match: *` or `If-Match: <tag>` (412, or
//! 404 for an absent key, is `Mismatch`); a tag is the server's ETag exactly as
//! sent. A 409 (a conflicting conditional write in flight), any 5xx, a timeout,
//! a lost or malformed response are `Unavailable`: the request may or may not
//! have taken effect, which the authority's settle rule decides. Nothing here
//! retries a write; a GET, listing or DELETE is resent once only when a pooled
//! connection fails before any response byte.
//!
//! Opening probes the server: a create-only PUT twice on a scratch key under
//! the prefix (the second must be refused), then an `If-Match` PUT on a tag the
//! object does not have (it must be refused), then a DELETE. A server that
//! overwrites (Garage 2.4.1 does) cannot keep a store and fails the open.
//!
//! Requests run on the caller's thread. One provider handle serialises its
//! callers on a mutex; `get_many` runs its GETs over up to [`http::SLOTS`]
//! keep-alive connections at once, driven by `poll(2)`. Steady-state requests
//! build their head, canonical request and signature in reused buffers; what a
//! call still allocates is what the trait returns (a GET's bytes and tag, a
//! replace's tag, a listing's keys).

mod http;
mod list;
pub(crate) mod sigv4;

use crate::{
    Condition, LIST_PAGE, Listing, ObjectStorage, Put, Replaced, StorageError, Tag, check_key,
};
use http::{Driver, Endpoint, Fail, Request, Response};
use sha2::{Digest, Sha256};
use sigv4::{EMPTY_HASH, Signer, amz_stamp, hex_into};
use std::net::ToSocketAddrs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Connect timeout without `connect_timeout_ms`.
pub const DEFAULT_CONNECT_MS: u64 = 5_000;
/// Whole-request timeout without `request_timeout_ms`.
pub const DEFAULT_REQUEST_MS: u64 = 30_000;

/// The members an `objects.s3@1` entry takes.
pub(crate) const MEMBERS: &[&str] = &[
    "provider",
    "endpoint",
    "bucket",
    "region",
    "prefix",
    "credentials",
    "roots",
    "connect_timeout_ms",
    "request_timeout_ms",
];

/// An `objects.s3@1` storage.
pub struct S3Objects {
    client: Mutex<Client>,
}

struct Client {
    driver: Driver,
    signer: Signer,
    /// `host[:port]` as the endpoint names it: the Host header.
    authority: String,
    bucket: String,
    /// `<prefix>/`, or empty.
    prefix: String,
    session_token: Option<String>,
    path: Vec<u8>,
    query: Vec<u8>,
    auth: Vec<u8>,
}

/// One request's description, before signing.
#[derive(Clone, Copy)]
enum Cond<'a> {
    None,
    Absent,
    Matches(&'a str),
}

fn unavailable(op: &str, key: &str, why: &str) -> StorageError {
    StorageError::Unavailable(format!("{op} {key}: {why}"))
}

fn status_error(op: &str, key: &str, status: u16) -> StorageError {
    if status >= 500 || status == 409 {
        unavailable(op, key, &format!("status {status}"))
    } else {
        StorageError::Io(format!("{op} {key}: status {status}"))
    }
}

/// Append `text` RFC 3986-encoded for a query value, as SigV4's canonical
/// query requires: unreserved bytes as they are, every other byte `%XX`.
fn encode_query(text: &str, out: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b);
        } else {
            out.extend_from_slice(&[b'%', HEX[(b >> 4) as usize], HEX[(b & 15) as usize]]);
        }
    }
}

impl Client {
    /// Write the head of `method` on S3 key `<prefix><key>` into `head`.
    #[allow(clippy::too_many_arguments)]
    fn head(
        &mut self,
        head: &mut Vec<u8>,
        method: &str,
        key: &str,
        list_query: Option<(&str, &str)>,
        body: &[u8],
        cond: Cond<'_>,
    ) {
        self.path.clear();
        self.path.push(b'/');
        self.path.extend_from_slice(self.bucket.as_bytes());
        if list_query.is_none() {
            self.path.push(b'/');
            self.path.extend_from_slice(self.prefix.as_bytes());
            self.path.extend_from_slice(key.as_bytes());
        }
        self.query.clear();
        if let Some((prefix, after)) = list_query {
            // Sorted by name, as the canonical query requires.
            self.query
                .extend_from_slice(b"list-type=2&max-keys=1000&prefix=");
            encode_query(&self.prefix, &mut self.query);
            encode_query(prefix, &mut self.query);
            if !after.is_empty() {
                self.query.extend_from_slice(b"&start-after=");
                encode_query(&self.prefix, &mut self.query);
                encode_query(after, &mut self.query);
            }
        }
        let mut payload = [0u8; 64];
        let payload: &str = if body.is_empty() {
            EMPTY_HASH
        } else {
            hex_into(&Sha256::digest(body).into(), &mut payload);
            std::str::from_utf8(&payload).unwrap()
        };
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let stamp = amz_stamp(seconds);
        let stamp_str = std::str::from_utf8(&stamp).unwrap();
        let mut headers: [(&str, &str); 6] = [("", ""); 6];
        let mut n = 0;
        let mut add = |name, value| {
            headers[n] = (name, value);
            n += 1;
        };
        add("host", self.authority.as_str());
        match cond {
            Cond::None => {}
            Cond::Absent => add("if-none-match", "*"),
            Cond::Matches(tag) => add("if-match", tag),
        }
        add("x-amz-content-sha256", payload);
        add("x-amz-date", stamp_str);
        if let Some(token) = &self.session_token {
            add("x-amz-security-token", token.as_str());
        }
        self.auth.clear();
        self.signer.sign(
            method,
            &self.path,
            &self.query,
            &headers[..n],
            payload,
            &stamp,
            &mut self.auth,
        );
        head.extend_from_slice(method.as_bytes());
        head.push(b' ');
        head.extend_from_slice(&self.path);
        if !self.query.is_empty() {
            head.push(b'?');
            head.extend_from_slice(&self.query);
        }
        head.extend_from_slice(b" HTTP/1.1\r\n");
        for (name, value) in &headers[..n] {
            head.extend_from_slice(name.as_bytes());
            head.extend_from_slice(b": ");
            head.extend_from_slice(value.as_bytes());
            head.extend_from_slice(b"\r\n");
        }
        if method == "PUT" {
            head.extend_from_slice(b"content-length: ");
            let mut digits = [0u8; 20];
            let mut at = digits.len();
            let mut v = body.len();
            loop {
                at -= 1;
                digits[at] = b'0' + (v % 10) as u8;
                v /= 10;
                if v == 0 {
                    break;
                }
            }
            head.extend_from_slice(&digits[at..]);
            head.extend_from_slice(b"\r\n");
        }
        head.extend_from_slice(b"authorization: ");
        head.extend_from_slice(&self.auth);
        head.extend_from_slice(b"\r\n\r\n");
    }

    /// One request; `use_response` maps its outcome.
    fn one<T>(
        &mut self,
        method: &str,
        key: &str,
        list_query: Option<(&str, &str)>,
        body: &[u8],
        cond: Cond<'_>,
        own_body: bool,
        use_response: impl FnOnce(Result<Response, Fail>, &[u8]) -> T,
    ) -> T {
        let mut driver = std::mem::replace(&mut self.driver, Driver::placeholder());
        let mut use_response = Some(use_response);
        let mut out = None;
        driver.run(
            1,
            |_, head| {
                self.head(head, method, key, list_query, body, cond);
                Request {
                    body,
                    idempotent: method != "PUT",
                    own_body,
                }
            },
            |_, result, scratch| out = Some((use_response.take().unwrap())(result, scratch)),
        );
        self.driver = driver;
        out.expect("one request finishes")
    }

    fn put(&mut self, key: &str, bytes: &[u8], cond: Cond<'_>) -> Result<Response, StorageError> {
        self.one("PUT", key, None, bytes, cond, false, |r, _| {
            r.map_err(|f| unavailable("put", key, f.describe()))
        })
    }

    fn delete_raw(&mut self, key: &str) -> Result<(), StorageError> {
        self.one(
            "DELETE",
            key,
            None,
            b"",
            Cond::None,
            false,
            |r, _| match r {
                Err(f) => Err(unavailable("delete", key, f.describe())),
                Ok(r) if matches!(r.status, 200 | 204 | 404) => Ok(()),
                Ok(r) => Err(status_error("delete", key, r.status)),
            },
        )
    }

    fn get_tagged(&mut self, key: &str) -> Result<Option<(Vec<u8>, Tag)>, StorageError> {
        self.one("GET", key, None, b"", Cond::None, true, |r, _| match r {
            Err(f) => Err(unavailable("get", key, f.describe())),
            Ok(mut r) if r.status == 200 => {
                let tag = Tag(r
                    .etag()
                    .ok_or_else(|| unavailable("get", key, "response has no ETag"))?
                    .to_string());
                Ok(Some((r.owned.take().unwrap_or_default(), tag)))
            }
            Ok(r) if r.status == 404 => Ok(None),
            Ok(r) => Err(status_error("get", key, r.status)),
        })
    }

    /// Check that the server honours create-only and If-Match writes.
    fn probe(&mut self, endpoint: &str) -> Result<(), String> {
        let mut seed = Sha256::new();
        seed.update(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .to_le_bytes(),
        );
        seed.update(std::process::id().to_le_bytes());
        seed.update((&raw const seed as usize).to_le_bytes());
        let mut hex = [0u8; 64];
        hex_into(&seed.finalize().into(), &mut hex);
        // A leading '.' keeps the scratch key outside the object key grammar.
        let key = format!(".probe-{}", std::str::from_utf8(&hex[..16]).unwrap());
        let fail = |e: StorageError| format!("objects.s3@1 probe of {endpoint}: {e}");
        let first = self.put(&key, b"probe-1", Cond::Absent).map_err(fail)?;
        if first.status != 200 {
            return Err(fail(status_error("put", &key, first.status)));
        }
        let second = self.put(&key, b"probe-2", Cond::Absent).map_err(fail);
        let third = self
            .put(
                &key,
                b"probe-3",
                Cond::Matches("\"hirsel-probe-no-such-tag\""),
            )
            .map_err(fail);
        let deleted = self.delete_raw(&key).map_err(fail);
        match second?.status {
            412 | 409 => {}
            200 => {
                return Err(format!(
                    "objects.s3@1: {endpoint} does not honour If-None-Match: * (a second create-only PUT replaced the object), so it cannot keep a store"
                ));
            }
            other => return Err(fail(status_error("put", &key, other))),
        }
        match third?.status {
            412 => {}
            200 => {
                return Err(format!(
                    "objects.s3@1: {endpoint} does not honour If-Match (a PUT on a tag the object does not have replaced it), so it cannot keep a store"
                ));
            }
            other => return Err(fail(status_error("put", &key, other))),
        }
        deleted
    }
}

impl S3Objects {
    /// Open an `objects.s3@1` entry (its members already checked against
    /// [`MEMBERS`]) and probe the server.
    pub fn open(members: &serde_json::Map<String, serde_json::Value>) -> Result<Self, String> {
        let text = |name: &str| -> Result<Option<&str>, String> {
            match members.get(name) {
                None => Ok(None),
                Some(v) => v
                    .as_str()
                    .map(Some)
                    .ok_or_else(|| format!("objects.s3@1 {name} is a string")),
            }
        };
        let required = |name: &str| -> Result<&str, String> {
            text(name)?
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("objects.s3@1 needs {name}"))
        };
        let millis = |name: &str, default: u64| -> Result<Duration, String> {
            match members.get(name) {
                None => Ok(Duration::from_millis(default)),
                Some(v) => v
                    .as_u64()
                    .filter(|&m| m > 0)
                    .map(Duration::from_millis)
                    .ok_or_else(|| format!("objects.s3@1 {name} is a positive integer")),
            }
        };
        let endpoint = required("endpoint")?;
        let (tls, authority) = if let Some(rest) = endpoint.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = endpoint.strip_prefix("http://") {
            (false, rest)
        } else {
            return Err(
                "objects.s3@1 endpoint is http://host[:port] or https://host[:port]".into(),
            );
        };
        if authority.is_empty()
            || !authority
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
        {
            return Err(
                "objects.s3@1 endpoint is http://host[:port] or https://host[:port], with no path"
                    .into(),
            );
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !h.ends_with(':') && (!h.starts_with('[') || h.ends_with(']')) => (
                h,
                p.parse::<u16>()
                    .map_err(|_| "objects.s3@1 endpoint port is a number".to_string())?,
            ),
            _ => (authority, if tls { 443 } else { 80 }),
        };
        let bare_host = host.trim_start_matches('[').trim_end_matches(']');
        let bucket = required("bucket")?;
        if bucket.len() < 3
            || bucket.len() > 63
            || !bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'))
        {
            return Err("objects.s3@1 bucket is an S3 bucket name".into());
        }
        let region = required("region")?;
        if !region
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("objects.s3@1 region is an S3 region name".into());
        }
        let prefix = text("prefix")?.unwrap_or("");
        if !prefix.is_empty() && check_key(prefix).is_err() {
            return Err("objects.s3@1 prefix is an object key (no leading or trailing '/')".into());
        }
        let credentials = required("credentials")?;
        if !Path::new(credentials).is_absolute() {
            return Err("objects.s3@1 credentials is an absolute file path".into());
        }
        let creds = read_credentials(Path::new(credentials))?;
        let tls = if tls {
            let roots = match text("roots")? {
                Some(path) if Path::new(path).is_absolute() => {
                    let pem = std::fs::read(path)
                        .map_err(|e| format!("objects.s3@1 roots {path}: {e}"))?;
                    hirsel_run_schema::tls::pem_roots(&pem)
                        .ok_or_else(|| format!("objects.s3@1 roots {path} holds no trust anchor"))?
                }
                Some(_) => return Err("objects.s3@1 roots is an absolute PEM file path".into()),
                None => webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            let config = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("objects.s3@1 TLS: {e}"))?
            .with_root_certificates(rustls::RootCertStore { roots })
            .with_no_client_auth();
            let name = rustls::pki_types::ServerName::try_from(bare_host.to_string())
                .map_err(|_| format!("objects.s3@1 endpoint host {bare_host} is not a TLS name"))?;
            Some((Arc::new(config), name))
        } else if members.contains_key("roots") {
            return Err("objects.s3@1 roots applies to an https endpoint".into());
        } else {
            None
        };
        let addrs: Vec<_> = (bare_host, port)
            .to_socket_addrs()
            .map_err(|e| format!("objects.s3@1 endpoint {endpoint}: {e}"))?
            .collect();
        if addrs.is_empty() {
            return Err(format!(
                "objects.s3@1 endpoint {endpoint} resolves to no address"
            ));
        }
        let driver = Driver::new(Endpoint {
            addrs,
            tls,
            connect_timeout: millis("connect_timeout_ms", DEFAULT_CONNECT_MS)?,
            request_timeout: millis("request_timeout_ms", DEFAULT_REQUEST_MS)?,
        });
        let mut client = Client {
            driver,
            signer: Signer::new(&creds.access_key, creds.secret_key.as_bytes(), region, "s3"),
            authority: authority.to_string(),
            bucket: bucket.to_string(),
            prefix: if prefix.is_empty() {
                String::new()
            } else {
                format!("{prefix}/")
            },
            session_token: creds.session_token,
            path: Vec::with_capacity(512),
            query: Vec::with_capacity(512),
            auth: Vec::with_capacity(256),
        };
        client.probe(endpoint)?;
        Ok(S3Objects {
            client: Mutex::new(client),
        })
    }

    /// Requests sent since opening, the probe's included.
    pub fn requests(&self) -> u64 {
        self.client.lock().expect("s3 client").driver.requests
    }
}

struct Credentials {
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
}

/// The credentials JSON `packages/s3@1` `from_grants` reads:
/// `{"access_key": .., "secret_key": .., "session_token": ..}`, the token
/// optional. Errors never quote the file's contents.
fn read_credentials(path: &Path) -> Result<Credentials, String> {
    let shown = path.display();
    let bytes =
        std::fs::read(path).map_err(|e| format!("objects.s3@1 credentials {shown}: {e}"))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| format!("objects.s3@1 credentials {shown} is not JSON"))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("objects.s3@1 credentials {shown} is not a JSON object"))?;
    if let Some(unknown) = object
        .keys()
        .find(|k| !matches!(k.as_str(), "access_key" | "secret_key" | "session_token"))
    {
        return Err(format!(
            "objects.s3@1 credentials {shown} has no member {unknown:?}"
        ));
    }
    let field = |name: &str| {
        object.get(name).map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty() && s.bytes().all(|b| (0x21..0x7f).contains(&b)))
                .map(str::to_string)
                .ok_or_else(|| {
                    format!("objects.s3@1 credentials {shown} {name} is a printable string")
                })
        })
    };
    let access_key = field("access_key")
        .ok_or_else(|| format!("objects.s3@1 credentials {shown} needs access_key"))??;
    let secret_key = field("secret_key")
        .ok_or_else(|| format!("objects.s3@1 credentials {shown} needs secret_key"))??;
    if secret_key.len() > 128 {
        return Err(format!(
            "objects.s3@1 credentials {shown} secret_key exceeds 128 bytes"
        ));
    }
    let session_token = field("session_token").transpose()?;
    Ok(Credentials {
        access_key,
        secret_key,
        session_token,
    })
}

/// A tag usable as an If-Match value: what a server's ETag header can be.
fn header_safe(tag: &Tag) -> bool {
    !tag.0.is_empty()
        && tag.0.len() <= http::ETAG_CAP
        && tag.0.bytes().all(|b| (0x21..0x7f).contains(&b))
}

impl ObjectStorage for S3Objects {
    fn get_tagged(&self, key: &str) -> Result<Option<(Vec<u8>, Tag)>, StorageError> {
        check_key(key)?;
        self.client.lock().expect("s3 client").get_tagged(key)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        check_key(key)?;
        let mut c = self.client.lock().expect("s3 client");
        c.one("GET", key, None, b"", Cond::None, true, |r, _| match r {
            Err(f) => Err(unavailable("get", key, f.describe())),
            Ok(mut r) if r.status == 200 => Ok(Some(r.owned.take().unwrap_or_default())),
            Ok(r) if r.status == 404 => Ok(None),
            Ok(r) => Err(status_error("get", key, r.status)),
        })
    }

    fn get_many(&self, keys: &[String]) -> Vec<Result<Option<Vec<u8>>, StorageError>> {
        let mut out: Vec<Result<Option<Vec<u8>>, StorageError>> = Vec::with_capacity(keys.len());
        let mut valid = Vec::with_capacity(keys.len());
        for key in keys {
            match check_key(key) {
                Ok(()) => {
                    valid.push(out.len());
                    out.push(Ok(None));
                }
                Err(e) => out.push(Err(e)),
            }
        }
        let mut guard = self.client.lock().expect("s3 client");
        let c = &mut *guard;
        let mut driver = std::mem::replace(&mut c.driver, Driver::placeholder());
        driver.run(
            valid.len(),
            |i, head| {
                c.head(head, "GET", &keys[valid[i]], None, b"", Cond::None);
                Request {
                    body: b"",
                    idempotent: true,
                    own_body: true,
                }
            },
            |i, r, _| {
                let key = &keys[valid[i]];
                out[valid[i]] = match r {
                    Err(f) => Err(unavailable("get", key, f.describe())),
                    Ok(mut r) if r.status == 200 => Ok(Some(r.owned.take().unwrap_or_default())),
                    Ok(r) if r.status == 404 => Ok(None),
                    Ok(r) => Err(status_error("get", key, r.status)),
                };
            },
        );
        c.driver = driver;
        out
    }

    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<Put, StorageError> {
        check_key(key)?;
        let r = self
            .client
            .lock()
            .expect("s3 client")
            .put(key, bytes, Cond::Absent)?;
        match r.status {
            200 => Ok(Put::Created),
            412 => Ok(Put::Exists),
            other => Err(status_error("put", key, other)),
        }
    }

    fn replace(
        &self,
        key: &str,
        bytes: &[u8],
        condition: Condition,
    ) -> Result<Replaced, StorageError> {
        check_key(key)?;
        let cond = match &condition {
            Condition::Absent => Cond::Absent,
            Condition::Matches(tag) if header_safe(tag) => Cond::Matches(&tag.0),
            // No server ETag is this, so no object has it.
            Condition::Matches(_) => return Ok(Replaced::Mismatch),
        };
        let r = self
            .client
            .lock()
            .expect("s3 client")
            .put(key, bytes, cond)?;
        match r.status {
            200 => Ok(Replaced::Replaced(Tag(r
                .etag()
                .ok_or_else(|| unavailable("replace", key, "response has no ETag"))?
                .to_string()))),
            412 => Ok(Replaced::Mismatch),
            404 if matches!(condition, Condition::Matches(_)) => Ok(Replaced::Mismatch),
            other => Err(status_error("replace", key, other)),
        }
    }

    fn list(&self, prefix: &str, after: &str) -> Result<Listing, StorageError> {
        let mut c = self.client.lock().expect("s3 client");
        let store_prefix = c.prefix.clone();
        c.one(
            "GET",
            "",
            Some((prefix, after)),
            b"",
            Cond::None,
            false,
            |r, body| match r {
                Err(f) => Err(unavailable("list", prefix, f.describe())),
                Ok(r) if r.status == 200 => {
                    let page = list::scan(body).map_err(|why| unavailable("list", prefix, why))?;
                    let mut keys = Vec::with_capacity(page.keys.len().min(LIST_PAGE));
                    for full in page.keys {
                        let Some(key) = full.strip_prefix(store_prefix.as_str()) else {
                            return Err(unavailable(
                                "list",
                                prefix,
                                "server listed a key outside the prefix",
                            ));
                        };
                        // Keys outside the grammar (and the probe's) are not
                        // objects of this storage; a listing is a hint.
                        if key.starts_with(prefix) && key > after && check_key(key).is_ok() {
                            keys.push(key.to_string());
                        }
                    }
                    keys.sort();
                    keys.dedup();
                    let truncated = page.truncated || keys.len() > LIST_PAGE;
                    keys.truncate(LIST_PAGE);
                    Ok(Listing { keys, truncated })
                }
                Ok(r) => Err(status_error("list", prefix, r.status)),
            },
        )
    }

    fn delete(&self, key: &str) -> Result<(), StorageError> {
        check_key(key)?;
        self.client.lock().expect("s3 client").delete_raw(key)
    }
}
