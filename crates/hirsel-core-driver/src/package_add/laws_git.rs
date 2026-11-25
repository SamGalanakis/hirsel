// Git source laws L23, L24 and L30 to L37 (packages.md section 12), included into
// `package_add_laws` after laws.rs. The fixture is a smart-HTTP protocol version 2 server
// behind the CONNECT proxy, routed by host (alpha.test, beta.test, github.com), holding
// repositories as tables of objects and refs. Its objects are checked against the commit ids
// Git 2.43.0 computed over the same bytes, so a writer bug fails the fixture, not the law.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use super::{PACKAGE_X_SOURCE, PACKAGE_Y_SOURCE};
use std::sync::Mutex;

const C1: &str = "32e212a4b69a9f3b39893b29f4e4f1e935626a2b";
const C2: &str = "0b8c48e76c8462464b5ce67fd884449440d57844";
const C1_TREE: &str = "f437e62ea2472c628183ef6ac6aad55bd0a9f7d9";
const C1_WRAP_BLOB: &str = "b8b63ee2ea62f1ec73509eedeed52c7206deaffe";
const R2: &str = "f9ae88b99451269b8b6b0d7d37bac85c316f44f7";
const R3: &str = "da14b90edd082f0beed3c0f09fc23aaad0a60849";
const R4: &str = "859eddd90bb3d572224eafa424b8fe05ccff88cf";
const R5: &str = "1ec6268ebb01745682597da6ad7e90ec97c89a85";
const R6: &str = "068a6be0a352dea1d8d7faf1e80eb05040387670";
const PACKAGE_W2: &str =
    "sha256:27b883e20a7c9640cf072d584cd1e8dd198aecd0795e034a7cfba52048f523a0";

const JSON_MANIFEST: &str =
    "[package]\nname = \"json\"\nmajor = 1\ncharge = \"charge-v1\"\n\n[sources]\n\"json.hirsel\" = \"json\"\n";
const WRAP_MANIFEST: &str = "[package]\nname = \"wrap\"\nmajor = 1\ncharge = \"charge-v1\"\n\n[sources]\n\"wrap.hirsel\" = \"wrap\"\n\n[dependencies]\njson = { major = 1, path = \"../json\" }\n";
const WRAP_V1: &str = "pub fn one() -> u64 { 1u64 }\n";
const WRAP_V2: &str = "pub fn one() -> u64 { 2u64 }\n";

// ---- Git objects, written from gitformat-pack and Git's object format ----

#[derive(Clone)]
enum Node {
    File(Vec<u8>),
    Link(String),
    Submodule(String),
    Tree(Tree),
}

/// A tree as names to nodes; a name is written verbatim, so a test can hold a `/` in one.
#[derive(Clone, Default)]
struct Tree(BTreeMap<Vec<u8>, Node>);

impl Tree {
    fn put(&mut self, path: &str, node: Node) {
        match path.split_once('/') {
            None => {
                self.0.insert(path.as_bytes().to_vec(), node);
            }
            Some((first, rest)) => {
                let entry = self
                    .0
                    .entry(first.as_bytes().to_vec())
                    .or_insert_with(|| Node::Tree(Tree::default()));
                let Node::Tree(tree) = entry else {
                    panic!("{first} is not a tree")
                };
                tree.put(rest, node);
            }
        }
    }

    fn file(mut self, path: &str, bytes: impl AsRef<[u8]>) -> Self {
        self.put(path, Node::File(bytes.as_ref().to_vec()));
        self
    }

    fn link(mut self, path: &str, target: &str) -> Self {
        self.put(path, Node::Link(target.to_owned()));
        self
    }

    fn submodule(mut self, path: &str, commit: &str) -> Self {
        self.put(path, Node::Submodule(commit.to_owned()));
        self
    }

    fn without(mut self, path: &str) -> Self {
        match path.split_once('/') {
            None => {
                self.0.remove(path.as_bytes());
            }
            Some((first, rest)) => {
                if let Some(Node::Tree(tree)) = self.0.remove(first.as_bytes()) {
                    self.0
                        .insert(first.as_bytes().to_vec(), Node::Tree(tree.without(rest)));
                }
            }
        }
        self
    }

    /// An entry of this tree named `name` exactly, `/` included.
    fn named(mut self, name: &str, node: Node) -> Self {
        self.0.insert(name.as_bytes().to_vec(), node);
        self
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
enum Kind {
    Commit = 1,
    Tree = 2,
    Blob = 3,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Tree => "tree",
            Self::Blob => "blob",
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn sha1(bytes: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    let mut hasher = sha1_checked::Sha1::default();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

/// Objects by id, as a server holds them.
#[derive(Clone, Default)]
struct Objects(BTreeMap<String, (Kind, Vec<u8>)>);

impl Objects {
    fn put(&mut self, kind: Kind, data: Vec<u8>) -> String {
        let mut framed = format!("{} {}\0", kind.name(), data.len()).into_bytes();
        framed.extend_from_slice(&data);
        let id = hex(&sha1(&framed));
        self.0.insert(id.clone(), (kind, data));
        id
    }

    fn tree(&mut self, tree: &Tree) -> String {
        let mut entries: Vec<(Vec<u8>, &str, String)> = Vec::new();
        for (name, node) in &tree.0 {
            let (mode, id, key) = match node {
                Node::File(bytes) => ("100644", self.put(Kind::Blob, bytes.clone()), name.clone()),
                Node::Link(target) => {
                    ("120000", self.put(Kind::Blob, target.as_bytes().to_vec()), name.clone())
                }
                Node::Submodule(commit) => ("160000", commit.clone(), name.clone()),
                Node::Tree(sub) => {
                    let mut key = name.clone();
                    key.push(b'/');
                    ("40000", self.tree(sub), key)
                }
            };
            entries.push((key, mode, id));
        }
        // Git orders entries by name, a tree's name compared as if it ended in `/`.
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let mut data = Vec::new();
        for (key, mode, id) in entries {
            let name = if mode == "40000" { &key[..key.len() - 1] } else { &key[..] };
            data.extend_from_slice(mode.as_bytes());
            data.push(b' ');
            data.extend_from_slice(name);
            data.push(0);
            data.extend_from_slice(&unhex(&id));
        }
        self.put(Kind::Tree, data)
    }

    fn commit(&mut self, tree: &Tree, parent: Option<&str>, time: u64, message: &str) -> String {
        let tree = self.tree(tree);
        let parent = parent.map_or(String::new(), |parent| format!("parent {parent}\n"));
        let person = format!("Fixture <fixture@hirsel.invalid> {time} +0000");
        self.put(
            Kind::Commit,
            format!("tree {tree}\n{parent}author {person}\ncommitter {person}\n\n{message}\n")
                .into_bytes(),
        )
    }

    /// Every object a depth-1 fetch of `commit` sends: the commit, its trees and blobs, and no
    /// submodule commit.
    fn reachable(&self, commit: &str) -> Vec<String> {
        let mut out = vec![commit.to_owned()];
        let (_, data) = &self.0[commit];
        let tree = std::str::from_utf8(&data[5..45]).unwrap().to_owned();
        let mut pending = vec![tree];
        let mut seen = HashSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            out.push(id.clone());
            let (kind, data) = &self.0[&id];
            if *kind != Kind::Tree {
                continue;
            }
            let mut rest = &data[..];
            while !rest.is_empty() {
                let space = rest.iter().position(|b| *b == b' ').unwrap();
                let mode = &rest[..space];
                let nul = rest.iter().position(|b| *b == 0).unwrap();
                let entry = hex(&rest[nul + 1..nul + 21]);
                rest = &rest[nul + 21..];
                if mode != b"160000" {
                    pending.push(entry);
                }
            }
        }
        out
    }
}

/// r1's tree at C1 (packages.md section 12, Git fixtures).
fn r1_tree() -> Tree {
    Tree::default()
        .file("hirsel.toml", "[workspace]\nmembers = [\"json\", \"wrap\"]\n")
        .file("json/hirsel.toml", JSON_MANIFEST)
        .file("json/json.hirsel", PACKAGE_X_SOURCE)
        .file("wrap/hirsel.toml", WRAP_MANIFEST)
        .file("wrap/wrap.hirsel", WRAP_V1)
}

/// r1's objects, with C1 and C2 checked against Git 2.43.0's ids.
fn r1_objects() -> Objects {
    let mut objects = Objects::default();
    let c1 = objects.commit(&r1_tree(), None, 1_700_000_000, "v1");
    assert_eq!(objects.tree(&r1_tree()), C1_TREE, "fixture tree writer");
    assert_eq!(c1, C1, "fixture commit writer");
    let c2 = objects.commit(
        &r1_tree().file("wrap/wrap.hirsel", WRAP_V2),
        Some(C1),
        1_700_000_060,
        "v2",
    );
    assert_eq!(c2, C2, "fixture commit writer");
    objects
}

// ---- Packs ----

enum Entry {
    Whole(Kind, Vec<u8>),
    /// A delta against the entry at this index of the pack.
    Offset(usize, Vec<u8>),
    /// A delta against the object with this id.
    Reference(String, Vec<u8>),
    /// Bytes written as they are, header and all.
    Raw(Vec<u8>),
}

fn varint(out: &mut Vec<u8>, mut n: u64) {
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn entry_header(out: &mut Vec<u8>, kind: u8, size: u64) {
    let mut byte = (kind << 4) | (size & 15) as u8;
    let mut size = size >> 4;
    while size != 0 {
        out.push(byte | 0x80);
        byte = (size & 0x7f) as u8;
        size >>= 7;
    }
    out.push(byte);
}

fn deflate(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}

/// A copy instruction of `size` bytes from `offset` of the base.
fn copy_op(out: &mut Vec<u8>, offset: u32, size: u32) {
    let mut op = 0x80u8;
    let mut args = Vec::new();
    for i in 0..4 {
        let byte = (offset >> (8 * i)) as u8;
        if byte != 0 {
            op |= 1 << i;
            args.push(byte);
        }
    }
    for i in 0..3 {
        let byte = (size >> (8 * i)) as u8;
        if byte != 0 {
            op |= 0x10 << i;
            args.push(byte);
        }
    }
    out.push(op);
    out.extend(args);
}

/// A delta that copies the common prefix of `base` and `target` and inserts the rest.
fn delta(base: &[u8], target: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, base.len() as u64);
    varint(&mut out, target.len() as u64);
    let prefix = base
        .iter()
        .zip(target)
        .take_while(|(a, b)| a == b)
        .count()
        .min(0xffff);
    if prefix > 0 {
        copy_op(&mut out, 0, prefix as u32);
    }
    for chunk in target[prefix..].chunks(127) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    out
}

/// A version 2 pack of `entries`, with its trailing checksum.
fn write_pack(entries: &[Entry]) -> Vec<u8> {
    let mut out = b"PACK".to_vec();
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    let mut offsets = Vec::new();
    for entry in entries {
        let at = out.len();
        offsets.push(at);
        match entry {
            Entry::Whole(kind, data) => {
                entry_header(&mut out, *kind as u8, data.len() as u64);
                out.extend(deflate(data));
            }
            Entry::Offset(base, data) => {
                entry_header(&mut out, 6, data.len() as u64);
                let mut n = (at - offsets[*base]) as u64;
                let mut bytes = vec![(n & 0x7f) as u8];
                n >>= 7;
                while n != 0 {
                    n -= 1;
                    bytes.push(0x80 | (n & 0x7f) as u8);
                    n >>= 7;
                }
                bytes.reverse();
                out.extend(bytes);
                out.extend(deflate(data));
            }
            Entry::Reference(base, data) => {
                entry_header(&mut out, 7, data.len() as u64);
                out.extend(unhex(base));
                out.extend(deflate(data));
            }
            Entry::Raw(bytes) => out.extend_from_slice(bytes),
        }
    }
    let sum = sha1(&out);
    out.extend(sum);
    out
}

/// How a repository answers a fetch.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum Packing {
    /// Every object whole.
    #[default]
    Whole,
    /// Blobs and trees as `OFS_DELTA` and `REF_DELTA` entries, one `REF_DELTA` before its base.
    Deltas,
}

fn pack_of(objects: &Objects, ids: &[String], packing: Packing) -> Vec<u8> {
    let whole = |id: &String| {
        let (kind, data) = &objects.0[id];
        Entry::Whole(*kind, data.clone())
    };
    if packing == Packing::Whole {
        return write_pack(&ids.iter().map(whole).collect::<Vec<_>>());
    }
    let (trees, blobs): (Vec<&String>, Vec<&String>) = ids[1..]
        .iter()
        .partition(|id| objects.0[*id].0 == Kind::Tree);
    let data = |id: &String| objects.0[id].1.clone();
    let mut entries = Vec::new();
    // The second tree as a delta named by id, before the first tree it is made from.
    if trees.len() > 1 {
        entries.push(Entry::Reference(
            trees[0].clone(),
            delta(&data(trees[0]), &data(trees[1])),
        ));
    }
    entries.push(whole(&ids[0]));
    entries.push(whole(trees[0]));
    for tree in trees.iter().skip(2) {
        entries.push(whole(tree));
    }
    let first = entries.len();
    entries.push(whole(blobs[0]));
    for (n, blob) in blobs.iter().enumerate().skip(1) {
        let base = blobs[n - 1];
        if n % 2 == 1 {
            entries.push(Entry::Offset(first + n - 1, delta(&data(base), &data(blob))));
        } else {
            entries.push(Entry::Reference(base.to_string(), delta(&data(base), &data(blob))));
        }
    }
    write_pack(&entries)
}

// ---- The server ----

fn pkt(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(format!("{:04x}", data.len() + 4).as_bytes());
    out.extend_from_slice(data);
}

fn pkt_lines(body: &[u8]) -> Vec<Option<String>> {
    let mut out = Vec::new();
    let mut rest = body;
    while rest.len() >= 4 {
        let length = usize::from_str_radix(std::str::from_utf8(&rest[..4]).unwrap(), 16).unwrap();
        if length < 4 {
            out.push(None);
            rest = &rest[4..];
            continue;
        }
        let line = String::from_utf8_lossy(&rest[4..length]);
        out.push(Some(line.trim_end_matches('\n').to_owned()));
        rest = &rest[length..];
    }
    out
}

#[derive(Clone, Default)]
struct Repo {
    objects: Objects,
    refs: BTreeMap<String, String>,
    /// The branch `HEAD` points at.
    head: Option<String>,
    packing: Packing,
}

impl Repo {
    /// A repository whose `refs/tags/v1`, `refs/heads/main` and `HEAD` name `commit`.
    fn at(objects: Objects, commit: &str) -> Self {
        Self {
            objects,
            refs: BTreeMap::from([
                ("refs/heads/main".into(), commit.into()),
                ("refs/tags/v1".into(), commit.into()),
            ]),
            head: Some("refs/heads/main".into()),
            packing: Packing::Whole,
        }
    }

    fn r1() -> Self {
        Self::at(r1_objects(), C1)
    }
}

struct Request {
    host: String,
    method: String,
    path: String,
    command: Option<String>,
    args: Vec<String>,
}

enum Reply {
    Ok(&'static str, Vec<u8>),
    Status(u16, Vec<(&'static str, String)>),
    /// The whole response, written by the law.
    Raw(Box<dyn FnOnce(&mut dyn Write) + Send>),
}

type Hook = Box<dyn FnMut(&Request) -> Option<Reply> + Send>;

#[derive(Default)]
struct Served {
    /// Repositories by host and path, as `alpha.test/r1`.
    repos: BTreeMap<String, Repo>,
    log: Vec<String>,
    hook: Option<Hook>,
}

fn advertisement() -> Vec<u8> {
    let mut out = Vec::new();
    for line in ["version 2\n", "agent=git/fixture\n", "ls-refs\n", "fetch=shallow\n", "object-format=sha1\n"] {
        pkt(&mut out, line.as_bytes());
    }
    out.extend_from_slice(b"0000");
    out
}

fn ls_refs(repo: &Repo, prefixes: &[&str]) -> Vec<u8> {
    let wanted = |name: &str| prefixes.is_empty() || prefixes.iter().any(|p| name.starts_with(p));
    let mut out = Vec::new();
    if let Some(head) = &repo.head
        && let Some(id) = repo.refs.get(head)
        && wanted("HEAD")
    {
        pkt(&mut out, format!("{id} HEAD symref-target:{head}\n").as_bytes());
    }
    for (name, id) in &repo.refs {
        if wanted(name) {
            pkt(&mut out, format!("{id} {name}\n").as_bytes());
        }
    }
    out.extend_from_slice(b"0000");
    out
}

/// A `fetch` response: shallow-info when deepened, then the pack on side band 1.
fn fetch_reply(repo: &Repo, wants: &[&str], deepen: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for want in wants {
        if repo.objects.0.get(*want).is_none_or(|(kind, _)| *kind != Kind::Commit) {
            pkt(&mut out, format!("ERR upload-pack: not our ref {want}").as_bytes());
            return out;
        }
    }
    let mut ids = Vec::new();
    for want in wants {
        ids.extend(repo.objects.reachable(want));
    }
    let pack = pack_of(&repo.objects, &ids, repo.packing);
    framed_pack(&mut out, wants, deepen, &pack);
    out
}

fn framed_pack(out: &mut Vec<u8>, wants: &[&str], deepen: bool, pack: &[u8]) {
    if deepen {
        pkt(out, b"shallow-info\n");
        for want in wants {
            pkt(out, format!("shallow {want}\n").as_bytes());
        }
        out.extend_from_slice(b"0001");
    }
    pkt(out, b"packfile\n");
    for chunk in pack.chunks(65515) {
        let mut data = vec![1u8];
        data.extend_from_slice(chunk);
        pkt(out, &data);
    }
    out.extend_from_slice(b"0000");
}

fn answer(served: &mut Served, request: &Request) -> Reply {
    let mut entry = format!("{} {}", request.host, request.method);
    match &request.command {
        Some(command) => {
            entry = format!("{} {command} {}", request.host, request.path);
            if !request.args.is_empty() {
                entry = format!("{entry} {}", request.args.join(", "));
            }
        }
        None => entry = format!("{entry} {}", request.path),
    }
    served.log.push(entry);
    if let Some(hook) = served.hook.as_mut()
        && let Some(reply) = hook(request)
    {
        return reply;
    }
    let route = |suffix: &str| {
        request
            .path
            .strip_suffix(suffix)
            .map(|repo| format!("{}{repo}", request.host))
    };
    if request.method == "GET"
        && let Some(key) = route("/info/refs")
        && served.repos.contains_key(&key)
    {
        return Reply::Ok("application/x-git-upload-pack-advertisement", advertisement());
    }
    if request.method == "POST"
        && let Some(key) = route("/git-upload-pack")
        && let Some(repo) = served.repos.get(&key)
    {
        let lines: Vec<&str> = request.args.iter().map(String::as_str).collect();
        return match request.command.as_deref() {
            Some("ls-refs") => {
                let prefixes: Vec<&str> =
                    lines.iter().filter_map(|l| l.strip_prefix("ref-prefix ")).collect();
                Reply::Ok("application/x-git-upload-pack-result", ls_refs(repo, &prefixes))
            }
            Some("fetch") => {
                let wants: Vec<&str> =
                    lines.iter().filter_map(|l| l.strip_prefix("want ")).collect();
                let deepen = lines.iter().any(|l| l.starts_with("deepen "));
                Reply::Ok("application/x-git-upload-pack-result", fetch_reply(repo, &wants, deepen))
            }
            _ => Reply::Status(400, Vec::new()),
        };
    }
    Reply::Ok("text/html", b"<!doctype html><html><body>a repository page</body></html>\n".to_vec())
}

fn read_request(stream: &mut impl BufRead, host: &str) -> Option<Request> {
    let mut line = String::new();
    stream.read_line(&mut line).ok()?;
    let mut words = line.split(' ');
    let method = words.next()?.to_owned();
    let target = words.next()?.to_owned();
    let mut length = 0;
    let mut chunked = false;
    loop {
        let mut header = String::new();
        stream.read_line(&mut header).ok()?;
        if header == "\r\n" || header.is_empty() {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            length = value.trim().parse().ok()?;
        }
        if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size = String::new();
            stream.read_line(&mut size).ok()?;
            let size = usize::from_str_radix(size.trim(), 16).ok()?;
            let mut chunk = vec![0; size + 2];
            stream.read_exact(&mut chunk).ok()?;
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    } else {
        body.resize(length, 0);
        stream.read_exact(&mut body).ok()?;
    }
    let path = target.split_once('?').map_or(target.as_str(), |(path, _)| path).to_owned();
    let (mut command, mut args) = (None, Vec::new());
    if method == "POST" {
        let mut after = false;
        for line in pkt_lines(&body) {
            match line {
                None => after = true,
                Some(line) => {
                    if let Some(name) = line.strip_prefix("command=") {
                        command = Some(name.to_owned());
                    } else if after
                        && (line.starts_with("want ") || line.starts_with("ref-prefix "))
                    {
                        args.push(line);
                    }
                }
            }
        }
    }
    Some(Request {
        host: host.to_owned(),
        method,
        path,
        command,
        args,
    })
}

fn tls_config(leaf: &[u8], key: &[u8]) -> Arc<rustls::ServerConfig> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
    Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from_pem_slice(leaf).unwrap()],
            PrivateKeyDer::from_pem_slice(key).unwrap(),
        )
        .unwrap(),
    )
}

/// The git fixture server: a CONNECT proxy in front of routed TLS hosts.
struct GitServer {
    served: Arc<Mutex<Served>>,
    proxy: String,
    ca: std::path::PathBuf,
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl GitServer {
    fn start(law: &PackageLaw) -> Self {
        let mut ca = include_bytes!("../../../hirsel-core-rt/testdata/tls/ca.pem").to_vec();
        ca.extend_from_slice(include_bytes!("../../../hirsel-core-rt/testdata/tls/github-ca.pem"));
        law.write("git-ca.pem", ca);
        let configs = BTreeMap::from([
            (
                "alpha.test",
                tls_config(
                    include_bytes!("../../../hirsel-core-rt/testdata/tls/alpha.pem"),
                    include_bytes!("../../../hirsel-core-rt/testdata/tls/alpha.key.pem"),
                ),
            ),
            (
                "beta.test",
                tls_config(
                    include_bytes!("../../../hirsel-core-rt/testdata/tls/beta.pem"),
                    include_bytes!("../../../hirsel-core-rt/testdata/tls/beta.key.pem"),
                ),
            ),
            (
                "github.com",
                tls_config(
                    include_bytes!("../../../hirsel-core-rt/testdata/tls/github.pem"),
                    include_bytes!("../../../hirsel-core-rt/testdata/tls/github.key.pem"),
                ),
            ),
        ]);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let served = Arc::new(Mutex::new(Served::default()));
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let state = served.clone();
        let thread = thread::spawn(move || {
            loop {
                let socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stopped.recv_timeout(Duration::from_millis(5))
                            != Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                        {
                            return;
                        }
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                socket.set_nonblocking(false).unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
                socket.set_write_timeout(Some(Duration::from_secs(30))).unwrap();
                let mut socket = BufReader::new(socket);
                let connect = headers(&mut socket);
                let host = connect
                    .strip_prefix("CONNECT ")
                    .and_then(|rest| rest.split_once(":443 "))
                    .map(|(host, _)| host.to_owned());
                let Some(config) = host.as_deref().and_then(|host| configs.get(host)) else {
                    state.lock().unwrap().log.push(format!("proxy {}", connect.trim_end()));
                    continue;
                };
                let host = host.unwrap();
                let socket = socket.get_mut();
                if socket
                    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .is_err()
                {
                    continue;
                }
                let Ok(connection) = rustls::ServerConnection::new(config.clone()) else {
                    continue;
                };
                let mut tls = BufReader::new(rustls::StreamOwned::new(connection, socket));
                let Some(request) = read_request(&mut tls, &host) else {
                    continue;
                };
                let reply = answer(&mut state.lock().unwrap(), &request);
                let tls = tls.get_mut();
                let _ = match reply {
                    Reply::Ok(content_type, body) => write!(
                        tls,
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    )
                    .and_then(|_| tls.write_all(&body)),
                    Reply::Status(status, headers) => {
                        let mut head = format!("HTTP/1.1 {status} Fixture\r\nConnection: close\r\nContent-Length: 0\r\n");
                        for (name, value) in headers {
                            head.push_str(&format!("{name}: {value}\r\n"));
                        }
                        head.push_str("\r\n");
                        tls.write_all(head.as_bytes())
                    }
                    Reply::Raw(write) => {
                        write(tls);
                        Ok(())
                    }
                };
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
        });
        Self {
            served,
            proxy,
            ca: law.path("git-ca.pem"),
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    fn serve(&self, key: &str, repo: Repo) {
        self.served.lock().unwrap().repos.insert(key.to_owned(), repo);
    }

    fn with<R>(&self, f: impl FnOnce(&mut Served) -> R) -> R {
        f(&mut self.served.lock().unwrap())
    }

    fn hook(&self, hook: impl FnMut(&Request) -> Option<Reply> + Send + 'static) {
        self.served.lock().unwrap().hook = Some(Box::new(hook));
    }

    /// The requests since the last call.
    fn log(&self) -> Vec<String> {
        std::mem::take(&mut self.served.lock().unwrap().log)
    }

    fn route(&self, command: &mut Command) {
        command
            .env("ALL_PROXY", &self.proxy)
            .env("NO_PROXY", "")
            .env_remove("no_proxy")
            .env_remove("HTTPS_PROXY")
            .env_remove("https_proxy")
            .env("SSL_CERT_FILE", &self.ca)
            .env_remove("SSL_CERT_DIR");
    }

    /// `hirsel add ARGS` from `dir` over the law's store `packages`.
    fn add(&self, law: &PackageLaw, dir: &str, args: &[&str]) -> (Option<i32>, String, String) {
        let mut command = add(law, dir, args);
        self.route(&mut command);
        result(command.output().unwrap())
    }

    /// Any other command over package store `store`.
    fn run(&self, law: &PackageLaw, args: &[&str], store: &str) -> (Option<i32>, String, String) {
        let mut command = law.command(args, store);
        self.route(&mut command);
        PackageLaw::output(command)
    }
}

impl Drop for GitServer {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// ---- Helpers ----

/// The roots a package store holds.
fn stored(law: &PackageLaw, store: &str) -> BTreeSet<String> {
    match fs::read_dir(law.path(store)) {
        Ok(entries) => entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(|name| format!("sha256:{name}"))
            .collect(),
        Err(_) => BTreeSet::new(),
    }
}

fn roots(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|root| (*root).to_owned()).collect()
}

/// The binding L23 writes, for `git` at `rev`, with `reference` when one was resolved.
fn wrap_binding(git: &str, rev: &str, root: &str, reference: Option<&str>) -> String {
    let reference = reference.map_or(String::new(), |r| format!(", ref = \"{r}\""));
    format!(
        "wrap = {{ major = 1, root = \"{root}\", git = \"{git}\", rev = \"{rev}\"{reference}, dir = \"wrap\" }}"
    )
}

fn manifest_text(law: &PackageLaw, dir: &str) -> String {
    fs::read_to_string(law.path(&format!("{dir}/hirsel.toml"))).unwrap()
}

/// A fresh single-package workspace `dir` binding nothing.
fn app_at(law: &PackageLaw, dir: &str) {
    law.app(dir, "");
}

fn info(host: &str, repo: &str) -> String {
    format!("{host} GET /{repo}/info/refs")
}

fn listing(host: &str, repo: &str, prefixes: &[&str]) -> String {
    let args: Vec<String> = prefixes.iter().map(|p| format!("ref-prefix {p}")).collect();
    format!("{host} ls-refs /{repo}/git-upload-pack {}", args.join(", "))
}

fn fetching(host: &str, repo: &str, want: &str) -> String {
    format!("{host} fetch /{repo}/git-upload-pack want {want}")
}

const L30_LOCK: &str = "# hirsel.lock: written by hirsel; do not edit.
version = 1

[[package]]
id = \"app@1\"
source = \"member:.\"
edges = [\"wrap@1\"]

[[package]]
id = \"json@1\"
root = \"sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58\"
source = \"via:wrap@1\"
edges = []

[[package]]
id = \"wrap@1\"
root = \"sha256:d863de7c3cf6ed4aa5535ee816d90fd603b45462eb6c2b78c846c584bd429689\"
source = \"git:https://alpha.test/r1#32e212a4b69a9f3b39893b29f4e4f1e935626a2b/wrap\"
edges = [\"json@1\"]
";
