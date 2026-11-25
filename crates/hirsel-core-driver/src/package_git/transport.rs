//! Git's smart HTTP protocol, version 2 (GIT-7, GIT-8, GIT-10): one `info/refs` GET to learn
//! the base and capabilities, then `ls-refs` (only for `hirsel add`) and one `fetch` POST that
//! never follows a redirect. No credentials, no cookies and no content coding.

use super::hash::{Format, Oid};
use super::pack::{self, Incoming, Pack};
use super::pkt::{self, Packet, Reader};
use super::{Budget, Failure, quote_text};
use std::io::{self, Read};

const ADVERTISEMENT_LIMIT: u64 = 64 << 10;
const LISTING_LIMIT: u64 = 16 << 20;
const PACK_LIMIT: u64 = 512 << 20;
const REDIRECT_LIMIT: usize = 3;
const QUERY: &str = "service=git-upload-pack";

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fetch(text) | Self::Pack(text) | Self::Refusal(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for Failure {}

fn cause(text: impl Into<String>) -> Failure {
    Failure::Fetch(text.into())
}

fn not_v2() -> Failure {
    cause("the server does not speak Git protocol version 2")
}

fn timed_out(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::TimedOut
        || error.kind() == io::ErrorKind::WouldBlock
        || error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<ureq::Error>())
            .is_some_and(|inner| matches!(inner, ureq::Error::Timeout(_)))
}

/// A read failure of a response body, a stopped limit passed through unchanged.
pub(crate) fn io_failure(error: io::Error) -> Failure {
    if timed_out(&error) {
        return cause("the server sent nothing for 60 s");
    }
    match error.into_inner().map(|inner| inner.downcast::<Failure>()) {
        Some(Ok(failure)) => *failure,
        Some(Err(inner)) => cause(inner.to_string()),
        None => cause("the connection failed"),
    }
}

fn request_failure(error: ureq::Error) -> Failure {
    match error {
        ureq::Error::Timeout(_) => cause("the server sent nothing for 60 s"),
        ureq::Error::Io(error) => io_failure(error),
        other => cause(other.to_string()),
    }
}

/// The agent every git request uses: the `url` client's roots and proxy, no redirect
/// following, statuses returned to the caller, and the 60 s stall bound.
fn agent() -> ureq::Agent {
    crate::package_store::https_config()
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .new_agent()
}

/// A response body counted as received against the source's budget and its own limit.
struct Body<'a> {
    inner: ureq::BodyReader<'static>,
    budget: &'a mut Budget,
    read: u64,
    limit: u64,
    over: fn() -> Failure,
}

impl Read for Body<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        if self.read > self.limit {
            return Err(io::Error::other((self.over)()));
        }
        self.budget.receive(n).map_err(io::Error::other)?;
        Ok(n)
    }
}

/// Checks a response's status and coding and opens its body.
fn open<'a>(
    response: ureq::http::Response<ureq::Body>,
    budget: &'a mut Budget,
    limit: u64,
    over: fn() -> Failure,
) -> Result<Body<'a>, Failure> {
    let status = response.status().as_u16();
    if matches!(status, 401 | 403) {
        return Err(cause(
            "the repository does not exist or requires authentication; hirsel fetches public repositories only",
        ));
    }
    if status != 200 {
        return Err(cause(format!("the server answered {status}")));
    }
    if let Some(coding) = response.headers().get("content-encoding") {
        return Err(cause(format!(
            "the server answered with Content-Encoding {}",
            quote_text(coding.as_bytes())
        )));
    }
    Ok(Body {
        inner: response.into_body().into_reader(),
        budget,
        read: 0,
        limit,
        over,
    })
}

/// Resolves a `Location` against the URL that answered with it.
fn resolve(base: &str, location: &str) -> String {
    if location.contains("://") {
        return location.to_string();
    }
    let scheme_end = base.find("://").map_or(0, |at| at + 3);
    let authority_end = base[scheme_end..]
        .find('/')
        .map_or(base.len(), |at| scheme_end + at);
    if location.starts_with("//") {
        return format!("{}{location}", &base[..scheme_end - 2]);
    }
    if location.starts_with('/') {
        return format!("{}{location}", &base[..authority_end]);
    }
    let path = base[authority_end..].split(['?', '#']).next().unwrap_or("");
    let directory = &path[..path.rfind('/').map_or(0, |at| at + 1)];
    format!("{}{directory}{location}", &base[..authority_end])
}

/// The base of a redirect target, when it is an info/refs request of the exact form.
fn endpoint(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let (path, query) = rest.split_once('?')?;
    let authority = path.split('/').next()?;
    if authority.contains('@') || query != QUERY || url.contains('#') {
        return None;
    }
    url.split_once('?')?.0.strip_suffix("/info/refs")
}

pub(crate) struct Remote {
    base: String,
    format: Format,
    /// Whether the advertisement named an `object-format`, which only then is sent back.
    announced: bool,
    shallow: bool,
}

/// Opens a git source: one `info/refs` GET, redirects to `https` info/refs only, at most 3.
pub(crate) fn connect(git: &str, budget: &mut Budget) -> Result<Remote, Failure> {
    let agent = agent();
    let mut url = format!("{git}/info/refs?{QUERY}");
    let mut base = git.to_string();
    let mut redirects = 0;
    let response = loop {
        let response = agent
            .get(&url)
            .header("Git-Protocol", "version=2")
            .call()
            .map_err(request_failure)?;
        if !response.status().is_redirection() {
            break response;
        }
        redirects += 1;
        if redirects > REDIRECT_LIMIT {
            return Err(cause("too many redirects"));
        }
        let location = response.headers().get("location").ok_or_else(|| {
            cause(format!(
                "the server answered {}",
                response.status().as_u16()
            ))
        })?;
        let next = resolve(&url, &String::from_utf8_lossy(location.as_bytes()));
        let shown = quote_text(next.as_bytes());
        if !next.starts_with("https://") {
            return Err(cause(format!("redirected to {shown}, which is not https")));
        }
        base = endpoint(&next)
            .ok_or_else(|| {
                cause(format!(
                    "redirected to {shown}, which is not a Git endpoint"
                ))
            })?
            .to_string();
        url = next;
    };
    let body = open(response, budget, ADVERTISEMENT_LIMIT, || {
        cause("the response is over the limit of 64 KiB")
    })?;
    let mut reader = Reader::new(body, not_v2);
    let mut first = reader.next()?;
    if let Some(Packet::Data(line)) = &first
        && line.starts_with(b"# service=")
    {
        if !matches!(reader.next()?, Some(Packet::Flush)) {
            return Err(not_v2());
        }
        first = reader.next()?;
    }
    match first {
        Some(Packet::Data(line)) if pkt::text(&line) == b"version 2" => {}
        _ => return Err(not_v2()),
    }
    let (mut fetch, mut shallow, mut format) = (false, false, None);
    loop {
        match reader.next()? {
            Some(Packet::Flush) => break,
            Some(Packet::Data(line)) => {
                let line = pkt::text(&line);
                let (key, value) = match line.iter().position(|b| *b == b'=') {
                    Some(at) => (&line[..at], Some(&line[at + 1..])),
                    None => (line, None),
                };
                match key {
                    b"fetch" => {
                        fetch = true;
                        shallow = value.is_some_and(|value| {
                            value.split(|b| *b == b' ').any(|word| word == b"shallow")
                        });
                    }
                    b"object-format" => format = value.map(<[u8]>::to_vec),
                    _ => {}
                }
            }
            _ => return Err(not_v2()),
        }
    }
    if !fetch {
        return Err(not_v2());
    }
    let announced = format.is_some();
    let format = match format.as_deref() {
        None | Some(b"sha1") => Format::Sha1,
        Some(b"sha256") => Format::Sha256,
        Some(other) => {
            return Err(cause(format!(
                "the server's object format is {}, not sha1 or sha256",
                quote_text(other)
            )));
        }
    };
    Ok(Remote {
        base,
        format,
        announced,
        shallow,
    })
}

fn post<'a>(
    remote: &Remote,
    body: Vec<u8>,
    budget: &'a mut Budget,
    limit: u64,
    over: fn() -> Failure,
) -> Result<Body<'a>, Failure> {
    let response = agent()
        .post(format!("{}/git-upload-pack", remote.base))
        .header("Content-Type", "application/x-git-upload-pack-request")
        .header("Accept", "application/x-git-upload-pack-result")
        .header("Git-Protocol", "version=2")
        .send(&body[..])
        .map_err(request_failure)?;
    open(response, budget, limit, over)
}

fn request(remote: &Remote, command: &str) -> Vec<u8> {
    let mut out = Vec::new();
    pkt::line(&mut out, &format!("command={command}"));
    if remote.announced {
        pkt::line(&mut out, &format!("object-format={}", remote.format.name()));
    }
    pkt::delim(&mut out);
    out
}

/// One reference as `ls-refs` lists it.
pub(crate) struct Listed {
    pub(crate) name: String,
    pub(crate) id: Oid,
    /// The object an annotated tag peels to.
    pub(crate) peeled: Option<Oid>,
    pub(crate) symref: Option<String>,
}

/// One `ls-refs` request with `symrefs`, `peel` and a `ref-prefix` per candidate (GIT-10).
pub(crate) fn list_refs(
    git: &str,
    prefixes: &[String],
    budget: &mut Budget,
) -> Result<(Remote, Vec<Listed>), Failure> {
    let remote = connect(git, budget)?;
    let mut body = request(&remote, "ls-refs");
    pkt::line(&mut body, "symrefs");
    pkt::line(&mut body, "peel");
    for prefix in prefixes {
        pkt::line(&mut body, &format!("ref-prefix {prefix}"));
    }
    pkt::flush(&mut body);
    let bad = || cause("the server sent a malformed reference listing");
    let response = post(&remote, body, budget, LISTING_LIMIT, || {
        cause("the response is over the limit of 16 MiB")
    })?;
    let mut reader = Reader::new(response, bad);
    let mut listed = Vec::new();
    loop {
        match reader.next()? {
            Some(Packet::Flush) | None => break,
            Some(Packet::Data(line)) => {
                let line = std::str::from_utf8(pkt::text(&line)).map_err(|_| bad())?;
                let mut words = line.split(' ');
                let id = words.next().and_then(Oid::parse).ok_or_else(bad)?;
                let name = words.next().ok_or_else(bad)?.to_string();
                let (mut peeled, mut symref) = (None, None);
                for attribute in words {
                    if let Some(target) = attribute.strip_prefix("peeled:") {
                        peeled = Some(Oid::parse(target).ok_or_else(bad)?);
                    } else if let Some(target) = attribute.strip_prefix("symref-target:") {
                        symref = Some(target.to_string());
                    }
                }
                listed.push(Listed {
                    name,
                    id,
                    peeled,
                    symref,
                });
            }
            Some(_) => return Err(bad()),
        }
    }
    Ok((remote, listed))
}

/// The side band of the `packfile` section, as the pack parser asks for it.
struct Band<'r, R> {
    reader: &'r mut Reader<R>,
    done: bool,
}

impl<R: Read> Incoming for Band<'_, R> {
    fn next(&mut self) -> Result<Option<Vec<u8>>, Failure> {
        while !self.done {
            match self.reader.next()? {
                Some(Packet::Data(data)) => match data.first() {
                    Some(1) => return Ok(Some(data[1..].to_vec())),
                    Some(2) => {}
                    Some(3) => return Err(pkt::server_said(&data[1..])),
                    _ => return Err(cause("the server sent a malformed side band")),
                },
                Some(Packet::Flush | Packet::End) | None => self.done = true,
                Some(Packet::Delim) => return Err(cause("the server sent a malformed side band")),
            }
        }
        Ok(None)
    }
}

/// Fetches commit `rev` alone (GIT-7 step 2, GIT-8) and checks its pack (GIT-9).
pub(crate) fn fetch(remote: &Remote, rev: &str, budget: &mut Budget) -> Result<Pack, Failure> {
    let wanted = Format::of_rev(rev).expect("a binding's rev is a full commit id");
    if wanted != remote.format {
        return Err(cause(format!(
            "the server's object format is {}, not {}",
            remote.format.name(),
            wanted.name()
        )));
    }
    let mut body = request(remote, "fetch");
    pkt::line(&mut body, "ofs-delta");
    pkt::line(&mut body, "no-progress");
    if remote.shallow {
        pkt::line(&mut body, "deepen 1");
    }
    pkt::line(&mut body, &format!("want {rev}"));
    pkt::line(&mut body, "done");
    pkt::flush(&mut body);
    let malformed = || cause("the server sent a malformed fetch response");
    let response = post(remote, body, budget, PACK_LIMIT, || {
        cause(
            "the response is over the limit of 512 MiB; hirsel fetches the whole commit, so a repository whose commit is larger cannot be a git source",
        )
    })?;
    let mut reader = Reader::new(response, malformed);
    let refused = |failure: Failure| match failure {
        Failure::Fetch(text)
            if text.starts_with("the server said: ") && text.contains("not our ref") =>
        {
            cause(format!("the server would not send commit {rev}"))
        }
        other => other,
    };
    loop {
        let name = match reader.next().map_err(refused)? {
            Some(Packet::Data(line)) => pkt::text(&line).to_vec(),
            _ => return Err(malformed()),
        };
        match &name[..] {
            b"acknowledgments" | b"shallow-info" => loop {
                match reader.next().map_err(refused)? {
                    Some(Packet::Data(_)) => {}
                    Some(Packet::Delim) => break,
                    _ => return Err(malformed()),
                }
            },
            b"packfile" => {
                let mut band = Band {
                    reader: &mut reader,
                    done: false,
                };
                return pack::parse(remote.format, &mut band).map_err(refused);
            }
            other => {
                return Err(cause(format!(
                    "the server sent an unexpected {} section",
                    quote_text(other)
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{endpoint, resolve};

    // bug: a redirect target that is not an info/refs request becomes the POST base.
    // oracle: GIT-7 step 1's endpoint rule, worked by hand.
    #[test]
    fn package_git_redirect_endpoint() {
        let next = resolve(
            "https://alpha.test/r1/info/refs?service=git-upload-pack",
            "https://beta.test/r1/info/refs?service=git-upload-pack",
        );
        assert_eq!(endpoint(&next), Some("https://beta.test/r1"));
        assert_eq!(
            resolve(
                "https://alpha.test/a/info/refs?x",
                "/b/info/refs?service=git-upload-pack"
            ),
            "https://alpha.test/b/info/refs?service=git-upload-pack"
        );
        assert_eq!(
            endpoint("https://beta.test/r1/info/refs?service=git-upload-pack&x=1"),
            None
        );
        assert_eq!(
            endpoint("https://u@beta.test/r1/info/refs?service=git-upload-pack"),
            None
        );
        assert_eq!(
            endpoint("https://beta.test/r1/other?service=git-upload-pack"),
            None
        );
    }
}
