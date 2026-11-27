use crate::native::tests::process::{CHILD_MARK, request_command_at};
use crate::package_content::{Binding, Content, Root};
use crate::registry_route::laws::{bundle, merkle, note, route};
use crate::registry_verify::notes::Log;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

const ORDER: [&str; 4] = ["base", "left", "right", "root"];
const DESCRIPTION: &str = "A release with \"quotes\", é, and a\nline break.";
const SOURCE: &str = "0123456789012345678901234567890123456789";
const CA: &[u8] = include_bytes!("../../../hirsel-core-rt/testdata/tls/sts-ca.pem");

#[test]
fn diamond_workspace_retries_the_middle_upload() {
    let fixture = Fixture::new();
    let peer = Peer::start();
    fixture.configure(&peer);
    let failed = fixture.publish("workspace", &["--workspace"]);
    assert!(
        failed.0 != Some(0) && failed.2.contains("unavailable"),
        "{failed:?}"
    );
    assert!(
        failed
            .2
            .contains("committed: fixture.test/base@1, fixture.test/left@1"),
        "{failed:?}"
    );
    assert!(
        failed.2.contains("unattempted: fixture.test/root@1"),
        "{failed:?}"
    );
    let before = peer.state.lock().unwrap().snapshot();
    assert_eq!(before.0, ORDER[..3]);
    assert_eq!(before.1.len(), 2);

    let retry = fixture.publish("workspace", &["--workspace"]);
    assert_eq!(retry.0, Some(0), "{retry:?}");
    let after = peer.state.lock().unwrap().snapshot();
    assert_eq!(after.0, [ORDER[..3].to_vec(), ORDER.to_vec()].concat());
    assert_eq!(after.1.len(), 4);
    for (member, release) in before.1 {
        assert_eq!(after.1[&member], release);
    }
    let mut roots: Vec<_> = after.1.values().map(|(_, root)| *root).collect();
    roots.sort();
    roots.dedup();
    assert_eq!(roots.len(), 4);
}

#[test]
fn workspace_refuses_unpublishable_members_before_contact() {
    let fixture = Fixture::new();
    let peer = Peer::start();
    fixture.configure(&peer);
    let root = fixture.dir.path();
    let members = root.join("workspace/hirsel.toml");
    fs::write(
        &members,
        "[workspace]\nmembers = [\"base\", \"left\", \"right\", \"root\", \"no\"]\n",
    )
    .unwrap();
    fs::create_dir(root.join("workspace/no")).unwrap();
    let manifest = root.join("workspace/no/hirsel.toml");
    let run = |name: &str, omit_version: bool, source: &str, extra: &str, expected: &str| {
        fs::write(&manifest, format!(
            "[package]\nname = {name:?}\n{}charge = \"charge-v1\"\n[sources]\n\"no.hirsel\" = \"no\"\n{extra}",
            if omit_version { "" } else { "version = \"1.0.0\"\n" },
        )).unwrap();
        fs::write(root.join("workspace/no/no.hirsel"), source).unwrap();
        let result = fixture.publish("workspace", &["--workspace"]);
        assert!(
            result.0 != Some(0) && result.2.contains(expected),
            "{result:?}"
        );
        assert_eq!(peer.state.lock().unwrap().connections, 0, "{result:?}");
    };
    let valid = "pub fn one() -> u64 { 1u64 }\n";
    run("local/no", false, valid, "", "scoped");
    run("no", false, valid, "", "scoped");
    run("fixture.test/no", true, valid, "", "version");
    run(
        "fixture.test/no",
        false,
        valid,
        "[dependencies]\noutside = { package = \"fixture.test/outside\", major = 1, path = \"../../outside\" }\n",
        "is not a member",
    );
    run(
        "fixture.test/no",
        false,
        "pub fn one() -> u64 { missing }\n",
        "",
        "does not check",
    );
}

fn check_registry_dependency(fixture: &Fixture, peer: &Peer) {
    let dir = fixture.dir.path().join("external");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("lib.hirsel"), "pub fn one() -> u64 { 2u64 }\n").unwrap();
    let (_, root) = peer.state.lock().unwrap().committed["base"];
    fs::write(dir.join("hirsel.toml"), format!(
        "[package]\nname = \"fixture.test/left\"\nversion = \"1.2.3\"\ncharge = \"charge-v1\"\n[sources]\n\"lib.hirsel\" = \"lib\"\n[dependencies]\nbase = {{ package = \"fixture.test/base\", major = 1, root = \"{root}\", registry = \"https://{}\" }}\n",
        peer.authority,
    )).unwrap();

    peer.state.lock().unwrap().misdirect_hint = true;
    let refused = fixture.publish("external", &[]);
    assert!(
        refused.0 != Some(0) && refused.2.contains("no authenticated release locator"),
        "{refused:?}"
    );
    assert_eq!(peer.state.lock().unwrap().posts, ["base"]);

    peer.state.lock().unwrap().misdirect_hint = false;
    let accepted = fixture.publish("external", &[]);
    assert_eq!(accepted.0, Some(0), "{accepted:?}");
    assert_eq!(peer.state.lock().unwrap().posts, ["base", "left"]);
}

#[test]
fn single_publish_and_recipe_publish_use_distinct_commands() {
    let fixture = Fixture::new();
    let peer = Peer::start();
    fixture.configure(&peer);
    let single = fixture.publish("workspace/base", &[]);
    assert_eq!(single.0, Some(0), "{single:?}");
    let recipe = fixture.run(
        "recipe",
        &[
            "publish",
            ".",
            "--libraries",
            fixture.libraries.to_str().unwrap(),
            "--code-store",
            fixture.dir.path().join("code").to_str().unwrap(),
            "--store",
            fixture.dir.path().join("objects").to_str().unwrap(),
        ],
    );
    assert_eq!(recipe.0, Some(0), "{recipe:?}");
    let hash = recipe.1.trim();
    assert_eq!(hash.len(), 64, "{recipe:?}");
    assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()), "{recipe:?}");
    assert_eq!(peer.state.lock().unwrap().snapshot().0, ["base"]);
    let help = fixture.run("recipe", &["help", "registry", "publish", "--json"]);
    assert_eq!(help.0, Some(0), "{help:?}");
    let help: Value = serde_json::from_str(&help.1).unwrap();
    assert_eq!(help["result"]["name"], "registry publish");
    assert!(
        help["result"]["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| { option["names"] == json!(["--workspace"]) })
    );
    check_registry_dependency(&fixture, &peer);
}

struct Fixture {
    dir: tempfile::TempDir,
    libraries: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("workspace")).unwrap();
        fs::write(
            root.join("workspace/hirsel.toml"),
            "[workspace]\nmembers = [\"root\", \"right\", \"left\", \"base\"]\n",
        )
        .unwrap();
        for (name, deps) in [
            ("root", &["right", "left"][..]),
            ("right", &["base"][..]),
            ("left", &["base"][..]),
            ("base", &[][..]),
        ] {
            let member = root.join("workspace").join(name);
            fs::create_dir(&member).unwrap();
            let mut manifest = format!(
                "[package]\nname = \"fixture.test/{name}\"\nversion = \"1.2.3\"\ncharge = \"charge-v1\"\n[sources]\n\"lib.hirsel\" = \"lib\"\n[dependencies]\n"
            );
            for dep in deps {
                manifest.push_str(&format!("{dep} = {{ package = \"fixture.test/{dep}\", major = 1, path = \"../{dep}\" }}\n"));
            }
            fs::write(member.join("hirsel.toml"), manifest).unwrap();
            fs::write(member.join("lib.hirsel"), "pub fn one() -> u64 { 1u64 }\n").unwrap();
        }
        fs::create_dir(root.join("recipe")).unwrap();
        fs::write(root.join("recipe/hirsel.toml"),
            "[package]\nname = \"local/recipe\"\nversion = \"1.0.0\"\ncharge = \"charge-v1\"\n[sources]\n\"main.hirsel\" = \"main\"\n").unwrap();
        fs::write(
            root.join("recipe/main.hirsel"),
            "pub fn main() -> Result[(), Exit] { Ok(()) }\n",
        )
        .unwrap();
        fs::write(root.join("ca.pem"), CA).unwrap();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        fs::write(root.join("signing.pk8"), pkcs8.as_ref()).unwrap();
        Self {
            dir,
            libraries: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../core")
                .canonicalize()
                .unwrap(),
        }
    }

    fn configure(&self, peer: &Peer) {
        let dir = self.dir.path();
        let conf = dir.join("config/hirsel");
        fs::create_dir_all(&conf).unwrap();
        fs::write(
            conf.join("config.toml"),
            route("fixture.test/*", &peer.authority, true, 42),
        )
        .unwrap();
        let key = Ed25519KeyPair::from_pkcs8(&fs::read(dir.join("signing.pk8")).unwrap()).unwrap();
        peer.state.lock().unwrap().public_key = key.public_key().as_ref().to_vec();
    }

    fn publish(&self, dir: &str, extra: &[&str]) -> (Option<i32>, String, String) {
        let key = self.dir.path().join("signing.pk8");
        let store = self.dir.path().join("store");
        let mut args = vec![
            "registry",
            "publish",
            "--signing-key",
            key.to_str().unwrap(),
            "--description",
            DESCRIPTION,
            "--source-commit",
            SOURCE,
            "--package-store",
            store.to_str().unwrap(),
            "--libraries",
            self.libraries.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        self.run(dir, &args)
    }

    fn run(&self, dir: &str, args: &[&str]) -> (Option<i32>, String, String) {
        let root = self.dir.path();
        let args: Vec<_> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let mut command =
            request_command_at(&std::env::current_exe().unwrap(), &args, &root.join(dir));
        for proxy in [
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
            "HTTP_PROXY",
            "http_proxy",
        ] {
            command.env_remove(proxy);
        }
        let output = command
            .env("SSL_CERT_FILE", root.join("ca.pem"))
            .env_remove("SSL_CERT_DIR")
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .output()
            .unwrap();
        let after = |bytes: &[u8]| {
            let text = String::from_utf8_lossy(bytes);
            text.split_once(CHILD_MARK)
                .unwrap_or_else(|| panic!("request child did not start: {output:?}"))
                .1
                .to_owned()
        };
        (
            output.status.code(),
            after(&output.stdout),
            after(&output.stderr),
        )
    }
}

type Committed = BTreeMap<String, (u64, Root)>;

#[derive(Default)]
struct State {
    connections: usize,
    public_key: Vec<u8>,
    posts: Vec<String>,
    committed: Committed,
    intent: BTreeMap<String, Vec<u8>>,
    rows: Vec<Vec<u8>>,
    frames: BTreeMap<String, Vec<u8>>,
    misdirect_hint: bool,
    fail: bool,
}

impl State {
    fn snapshot(&self) -> (Vec<String>, Committed) {
        (self.posts.clone(), self.committed.clone())
    }

    fn post(&mut self, authority: &str, mut body: &[u8]) -> (u16, Vec<u8>) {
        let metadata = item(&mut body);
        let parsed: [Value; 5] = serde_json::from_slice(metadata).unwrap();
        assert_eq!(metadata, serde_json::to_vec(&parsed).unwrap());
        let [intent, subject, proof, frame_roots, children] = parsed;
        let member = intent[6][0]
            .as_str()
            .unwrap()
            .strip_prefix("fixture.test/")
            .unwrap()
            .strip_suffix("@1")
            .unwrap();
        let frame = item(&mut body);
        assert!(
            body.is_empty(),
            "logged dependencies should not be uploaded again"
        );
        let size = zstd_safe::get_frame_content_size(frame).unwrap().unwrap();
        let mut expanded = vec![0; size as usize];
        assert_eq!(
            zstd_safe::decompress(expanded.as_mut_slice(), frame).unwrap(),
            size as usize
        );
        let (root, library) =
            crate::package_content::Library::read_frame(frame, Some(frame.len() as u64)).unwrap();
        assert_eq!(library.encoded.as_ref(), expanded);
        assert_eq!(root, Root::of(&expanded));
        let digest = hirsel_core_ids::hex::encode(&root.0);
        let content = Content::decode(&expanded).unwrap();
        assert_eq!(content.id.to_string(), format!("fixture.test/{member}@1"));
        assert_eq!(content.version.to_string(), "1.2.3");
        assert_eq!(frame_roots, json!([digest]));
        assert_eq!(
            intent,
            json!([
                "hirsel-intent-v1",
                format!("https://{authority}"),
                "publish",
                "fixture.test",
                "1",
                digest,
                [
                    format!("fixture.test/{member}@1"),
                    "1.2.3",
                    DESCRIPTION,
                    SOURCE,
                    null
                ],
            ])
        );
        assert_eq!(
            subject,
            json!(["ed25519", crate::call::base64(&self.public_key)])
        );
        let intent = serde_json::to_vec(&intent).unwrap();
        let signature = crate::registry_verify::decode_base64(proof.as_str().unwrap()).unwrap();
        UnparsedPublicKey::new(&ED25519, &self.public_key)
            .verify(&intent, &signature)
            .unwrap();
        if let Some(prior) = self.intent.insert(member.to_owned(), intent.clone()) {
            assert_eq!(prior, intent, "retry changed authorized bytes");
        }

        let direct = match member {
            "base" => &[][..],
            "left" | "right" => &["base"][..],
            "root" => &["left", "right"][..],
            _ => panic!("unexpected member {member}"),
        };
        let closure = if member == "root" {
            &["base", "left", "right"][..]
        } else {
            direct
        };
        assert_eq!(content.bindings.len(), direct.len());
        for dep in direct {
            let (id, binding) = &content.bindings[*dep];
            assert_eq!(id.to_string(), format!("fixture.test/{dep}@1"));
            assert!(
                matches!(binding, Binding::Floor { tested, minor: 2, patch: 3 }
                if *tested == self.committed[*dep].1),
                "{member} -> {dep}"
            );
        }
        let expected = closure
            .iter()
            .map(|dep| {
                let (index, root) = self.committed[*dep];
                (
                    root,
                    json!([
                        format!("fixture.test/{dep}@1"),
                        "1.2.3",
                        hirsel_core_ids::hex::encode(&root.0),
                        index.to_string()
                    ]),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(children, json!(expected.into_values().collect::<Vec<_>>()));
        self.posts.push(member.to_owned());
        if member == "right" && self.fail {
            self.fail = false;
            return (503, b"[\"unavailable\"]".to_vec());
        }
        let index = if let Some((index, prior)) = self.committed.get(member) {
            assert_eq!(*prior, root);
            *index
        } else {
            let index = self.rows.len() as u64;
            self.committed.insert(member.to_owned(), (index, root));
            self.frames.insert(digest.clone(), frame.to_vec());
            self.rows.push(
                serde_json::to_vec(&json!([
                    "release",
                    content.id.to_string(),
                    "1.2.3",
                    digest,
                    DESCRIPTION,
                    SOURCE,
                    "100",
                    "1",
                    "aa".repeat(32),
                    "bb".repeat(32),
                    [
                        subject,
                        hirsel_core_ids::hex::encode(&Sha256::digest(&intent)),
                        null,
                        proof
                    ],
                ]))
                .unwrap(),
            );
            index
        };
        (
            200,
            serde_json::to_vec(&json!([
                format!("{authority}/log"),
                index.to_string(),
                (index + 1).to_string(),
                digest
            ]))
            .unwrap(),
        )
    }

    fn get(&self, authority: &str, path: &str) -> (u16, Vec<u8>) {
        if path == "/status/checkpoint" {
            return (200, note(authority, Log::Status, 0, merkle(&[]), None));
        }
        if path == "/versions/fixture.test/base" {
            let (index, _) = self.committed["base"];
            let index = index + u64::from(self.misdirect_hint);
            return (
                200,
                serde_json::to_vec(&json!([[
                    "1.2.3",
                    index.to_string(),
                    (index + 1).to_string()
                ]]))
                .unwrap(),
            );
        }
        if let Some(root) = path
            .strip_prefix("/c/")
            .and_then(|p| p.strip_suffix(".zst"))
        {
            return (200, self.frames[root].clone());
        }
        let count = self.rows.len();
        if path == "/log/checkpoint" {
            return (
                200,
                note(
                    authority,
                    Log::Release,
                    count as u64,
                    merkle(&self.rows),
                    None,
                ),
            );
        }
        if let Some(size) = path
            .strip_prefix("/log/heads/")
            .and_then(|size| size.parse::<usize>().ok())
            && size <= count
        {
            return (
                200,
                note(
                    authority,
                    Log::Release,
                    size as u64,
                    merkle(&self.rows[..size]),
                    None,
                ),
            );
        }
        for size in 1..=count {
            if path == format!("/log/tile/entries/000.p/{size}") {
                return (200, bundle(&self.rows[..size]));
            }
            if path == format!("/log/tile/0/000.p/{size}") {
                return (
                    200,
                    self.rows[..size]
                        .iter()
                        .flat_map(|row| merkle(std::slice::from_ref(row)))
                        .collect(),
                );
            }
        }
        panic!("unexpected GET {path}");
    }
}

fn item<'a>(body: &mut &'a [u8]) -> &'a [u8] {
    let (length, tail) = body.split_at(4);
    let (item, tail) = tail.split_at(u32::from_be_bytes(length.try_into().unwrap()) as usize);
    *body = tail;
    item
}

struct Peer {
    authority: String,
    state: Arc<Mutex<State>>,
    stop: Option<mpsc::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Peer {
    fn start() -> Self {
        let cert = include_bytes!("../../../hirsel-core-rt/testdata/tls/sts.pem");
        let key = include_bytes!("../../../hirsel-core-rt/testdata/tls/sts.key.pem");
        let config = Arc::new(
            rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from_pem_slice(cert).unwrap()],
                PrivateKeyDer::from_pem_slice(key).unwrap(),
            )
            .unwrap(),
        );
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let authority = format!("localhost:{}", listener.local_addr().unwrap().port());
        let serve_authority = authority.clone();
        let state = Arc::new(Mutex::new(State {
            fail: true,
            ..State::default()
        }));
        let serve_state = state.clone();
        let (stop, stopped) = mpsc::channel();
        let worker = thread::spawn(move || {
            loop {
                match listener.accept() {
                    Ok((socket, _)) => {
                        let mut state = serve_state.lock().unwrap();
                        state.connections += 1;
                        serve(socket, &config, &serve_authority, &mut state).unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stopped.recv_timeout(Duration::from_millis(5))
                            != Err(mpsc::RecvTimeoutError::Timeout)
                        {
                            break;
                        }
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        });
        Self {
            authority,
            state,
            stop: Some(stop),
            worker: Some(worker),
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.take();
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

fn serve(
    socket: TcpStream,
    config: &Arc<rustls::ServerConfig>,
    authority: &str,
    state: &mut State,
) -> std::io::Result<()> {
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    socket.set_nonblocking(false)?;
    let connection = rustls::ServerConnection::new(config.clone()).unwrap();
    let mut stream = BufReader::new(rustls::StreamOwned::new(connection, socket));
    let mut line = String::new();
    stream.read_line(&mut line)?;
    let request = line.clone();
    let mut size = 0;
    let mut content_type = String::new();
    loop {
        line.clear();
        if stream.read_line(&mut line)? == 0 || line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        match name.to_ascii_lowercase().as_str() {
            "content-length" => size = value.trim().parse().unwrap(),
            "content-type" => content_type = value.trim().to_owned(),
            _ => {}
        }
    }
    let (status, response) = if request == "POST /v1/submit HTTP/1.1\r\n" {
        assert_eq!(content_type, "application/octet-stream");
        let mut body = vec![0; size];
        stream.read_exact(&mut body)?;
        state.post(authority, &body)
    } else {
        let path = request
            .strip_prefix("GET ")
            .unwrap()
            .split_once(' ')
            .unwrap()
            .0;
        state.get(authority, path)
    };
    let stream = stream.get_mut();
    write!(
        stream,
        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.len()
    )?;
    stream.write_all(&response)?;
    stream.flush()
}
