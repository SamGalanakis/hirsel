use super::*;
use crate::native::tests::{native_options, process, start_resident, stop_resident};
use crate::package_content::{Binding, Content, Id, Library, Root, Source};
use crate::package_manifest::name::Version;
use crate::package_store::Transport;
use crate::registry_fetch::Client;
use crate::registry_verify::status::Disposition;
use ring::signature::{Ed25519KeyPair, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use serde_json::json;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

const ALPHA: &str = "alpha.test";
const BETA: &str = "beta.test";
const TARGET: &str = "fixture.test/tenant/wrap";
const NOW: u64 = 400_000;
const TLS_CA: &[u8] = include_bytes!("../../../hirsel-core-rt/testdata/tls/ca.pem");

type Key<'a> = (&'a str, u8, u8);

fn signer(seed: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap()
}

fn b64(bytes: &[u8]) -> String {
    crate::call::base64(bytes)
}

fn c2sp_key((name, seed, tag): Key<'_>) -> (String, [u8; 4]) {
    let mut bytes = vec![tag];
    bytes.extend_from_slice(signer(seed).public_key().as_ref());
    let digest = Sha256::new()
        .chain_update(name)
        .chain_update(b"\n")
        .chain_update(&bytes)
        .finalize();
    let id = digest[..4].try_into().unwrap();
    (format!("{name}+{}+{}", hex(&digest[..4]), b64(&bytes)), id)
}

fn witnesses() -> [Key<'static>; 2] {
    [("first.test/witness", 7, 4), ("second.test/witness", 8, 4)]
}

pub(crate) fn route(prefix: &str, authority: &str, preview: bool, legal: u8) -> String {
    let keys: Vec<_> = if preview {
        Vec::new()
    } else {
        witnesses().into_iter().map(|key| c2sp_key(key).0).collect()
    };
    format!(
        "[[registry.route]]\nprefix = {prefix:?}\nurl = \"https://{authority}\"\nrelease-key = {:?}\nstatus-key = {:?}\nlegal-key = {:?}\nwitness-keys = {keys:?}\nquorum = {}\n",
        c2sp_key((&format!("{authority}/log"), 1, 1)).0,
        c2sp_key((&format!("{authority}/status"), 2, 1)).0,
        b64(signer(legal).public_key().as_ref()),
        keys.len(),
    )
}

pub(crate) fn note(authority: &str, log: Log, size: u64, root: Hash, time: Option<u64>) -> Vec<u8> {
    let origin = format!("{authority}/{}", log.prefix());
    let seed = if log == Log::Release { 1 } else { 2 };
    let text = format!("{origin}\n{size}\n{}\n", b64(&root));
    let mut note = format!("{text}\n");
    for (name, seed, tag) in std::iter::once((origin.as_str(), seed, 1))
        .chain(time.into_iter().flat_map(|_| witnesses()))
    {
        let (_, id) = c2sp_key((name, seed, tag));
        let mut bytes = id.to_vec();
        let input = if tag == 4 {
            let time = time.unwrap();
            bytes.extend(time.to_be_bytes());
            format!("cosignature/v1\ntime {time}\n{text}")
        } else {
            text.clone()
        };
        bytes.extend_from_slice(signer(seed).sign(input.as_bytes()).as_ref());
        note.push_str(&format!("— {name} {}\n", b64(&bytes)));
    }
    note.into_bytes()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    hirsel_core_ids::hex::encode(bytes)
}

pub(crate) fn auth() -> serde_json::Value {
    json!([
        ["ed25519", b64(signer(5).public_key().as_ref())],
        "aa".repeat(32),
        null,
        b64(&[5; 64])
    ])
}

pub(crate) fn signed_takedown(
    authority: &str,
    scope: &str,
    root: Root,
    previous: serde_json::Value,
    message: &str,
    seed: u8,
) -> (serde_json::Value, Vec<u8>) {
    let key = signer(seed);
    let intent = serde_json::to_vec(&json!([
        "hirsel-intent-v1",
        format!("https://{authority}"),
        "takedown",
        scope,
        "0",
        hex(&root.0),
        [previous, ["legal", message]]
    ]))
    .unwrap();
    let auth = json!([
        ["ed25519", b64(key.public_key().as_ref())],
        hex(&Sha256::digest(&intent)),
        null,
        b64(key.sign(&intent).as_ref())
    ]);
    (
        json!([
            "takedown",
            hex(&root.0),
            previous,
            "legal",
            message,
            "30",
            auth
        ]),
        intent,
    )
}

#[test]
fn legal_takedown_requires_pinned_signature_and_exact_intent() {
    let routes = Routes::parse(&route("*", ALPHA, true, 3)).unwrap();
    let server = Server::start();
    let transport = server.transport();
    serve_target_release(&server, Root([3; 32]));
    for case in [
        "wrong key",
        "invalid signature",
        "missing intent",
        "wrong digest",
        "different event",
        "valid",
    ] {
        let cache = tempfile::tempdir().unwrap();
        let client = Client::new(
            &routes,
            routes.for_name(TARGET).unwrap(),
            &transport,
            cache.path(),
        );
        let (mut row, mut intent) = signed_takedown(
            ALPHA,
            "fixture.test/tenant",
            Root([3; 32]),
            json!(null),
            "removed",
            if case == "wrong key" { 4 } else { 3 },
        );
        if case == "invalid signature" {
            row[6][3] = json!(b64(&[0; 64]));
        }
        if case == "different event" {
            row[4] = json!("other");
        }
        if case == "wrong digest" {
            intent.push(b' ');
        }
        let path = format!("/e/{}", row[6][1].as_str().unwrap());
        if case == "missing intent" {
            server.missing(ALPHA, &path);
        } else {
            server.serve(ALPHA, &path, intent);
        }
        let rows = [serde_json::to_vec(&row).unwrap()];
        server.serve(
            ALPHA,
            "/status/checkpoint",
            note(ALPHA, Log::Status, 1, merkle(&rows), None),
        );
        server.serve(ALPHA, "/status/tile/entries/000.p/1", bundle(&rows));
        let result = client.status_replay(NOW, None);
        assert_eq!(result.is_ok(), case == "valid", "{case}: {result:?}");
    }
}

fn release_row(root: Root) -> Vec<u8> {
    serde_json::to_vec(&json!([
        "release",
        format!("{TARGET}@1"),
        "1.2.3",
        hex(&root.0),
        "fixture",
        null,
        "100",
        "1",
        "aa".repeat(32),
        "bb".repeat(32),
        auth()
    ]))
    .unwrap()
}

fn serve_target_release(server: &Server, root: Root) {
    let rows = [release_row(root)];
    server.serve(
        ALPHA,
        "/log/checkpoint",
        note(ALPHA, Log::Release, 1, merkle(&rows), None),
    );
    server.serve(ALPHA, "/log/tile/entries/000.p/1", bundle(&rows));
}

fn retire(root: Root) -> Vec<u8> {
    serde_json::to_vec(&json!([
        "retire",
        hex(&root.0),
        null,
        "security",
        "fixture retirement",
        "200",
        auth()
    ]))
    .unwrap()
}

pub(crate) fn bundle(rows: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for row in rows {
        bytes.extend((row.len() as u16).to_be_bytes());
        bytes.extend(row);
    }
    bytes
}

pub(crate) fn merkle(rows: &[Vec<u8>]) -> Hash {
    match rows {
        [] => Sha256::digest([]).into(),
        [row] => Sha256::new()
            .chain_update([0])
            .chain_update(row)
            .finalize()
            .into(),
        _ => {
            let (left, right) = rows.split_at(1 << (rows.len() - 1).ilog2());
            Sha256::new()
                .chain_update([1])
                .chain_update(merkle(left))
                .chain_update(merkle(right))
                .finalize()
                .into()
        }
    }
}

fn route_checks() {
    let missing = Routes::default().for_name(TARGET).err().unwrap();
    assert!(missing.contains("https://pkg.hirsel.ai") && missing.contains("[[registry.route]]"));
    assert!(Routes::parse(&route("*", "PKG.HIRSEL.AI:443", true, 3)).is_err());

    let text = route("*", "default.test", false, 3)
        + &route("fixture.test/*", BETA, false, 3)
        + &route("fixture.test/tenant/*", ALPHA, true, 3);
    let routes = Routes::parse(&text).unwrap();
    for (name, origin) in [
        (TARGET, ALPHA),
        ("fixture.test/tenant", BETA),
        ("fixture.test/tenant-team/wrap", BETA),
        ("another.test/wrap", "default.test"),
    ] {
        assert_eq!(
            routes.for_name(name).unwrap().origin().to_string(),
            format!("https://{origin}")
        );
    }
    assert!(routes.for_name("fixture.test/tenant/wrap/extra").is_err());
    assert!(
        routes
            .for_pin(TARGET, &Origin::parse("https://beta.test").unwrap())
            .is_err()
    );
    assert!(Routes::parse(&(text.clone() + &route("*", ALPHA, true, 3))).is_err());
    assert!(Routes::parse(&(text.clone() + &route("another.test/*", ALPHA, false, 3))).is_err());
    assert!(Routes::parse(&text.replace("quorum = 2", "quorum = 0")).is_err());
    assert!(Routes::parse(&text.replace("quorum = 0", "quorum = 1")).is_err());
    assert!(Routes::parse(&text.replace("https://alpha.test", "https://alpha.test/path")).is_err());
    let witness = c2sp_key(("another.test/witness", 7, 4)).0;
    assert!(Routes::parse(&text.replace(&c2sp_key(witnesses()[1]).0, &witness)).is_err());
}

fn proofs(server: &Server, cache: &Path, target: Root) {
    let witnessed = Routes::parse(&route("*", ALPHA, false, 3)).unwrap();
    let preview = Routes::parse(&route("*", ALPHA, true, 3)).unwrap();
    let changed = Routes::parse(&route("*", ALPHA, false, 4)).unwrap();
    let other = Routes::parse(&route("*", BETA, false, 3)).unwrap();
    let online = server.transport();
    let offline = Transport::offline();
    fn client<'a>(routes: &'a Routes, transport: &'a Transport, cache: &'a Path) -> Client<'a> {
        Client::new(routes, routes.for_name(TARGET).unwrap(), transport, cache)
    }

    let entry = release_row(target);
    let mut log = vec![entry.clone(); 257];
    let root = merkle(&log);
    let current = note(ALPHA, Log::Release, 257, root, Some(NOW));
    server.serve(
        ALPHA,
        "/log/checkpoint",
        note(ALPHA, Log::Release, 257, root, None),
    );
    server.serve(
        ALPHA,
        "/log/heads/257",
        note(ALPHA, Log::Release, 257, root, Some(101)),
    );
    server.serve(
        ALPHA,
        "/versions/fixture.test/tenant/wrap",
        br#"[["1.2.3","256","257"]]"#.to_vec(),
    );
    let mut full_tile = vec![0; 256 * 32];
    full_tile[..32].copy_from_slice(&merkle(&log[..256]));
    server.serve(ALPHA, "/log/tile/1/000", full_tile);
    log.resize(512, entry);
    server.serve(ALPHA, "/log/tile/entries/001", bundle(&log[256..]));

    let preview_client = client(&preview, &online, cache);
    let preview_head = preview_client.release_checkpoint(NOW).unwrap();
    let [(hint, found)]: [_; 1] = preview_client
        .verified_hints(&preview_head, TARGET)
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(found.root(), target);
    assert_eq!(
        preview_client
            .publication_time(&found, Some(257), NOW)
            .unwrap(),
        100
    );
    assert!(preview_head.is_preview());
    assert!(
        client(&witnessed, &offline, cache)
            .release_checkpoint(NOW)
            .is_err()
    );
    let (requests, connections) = server.take();
    assert_eq!(
        requests,
        [
            "alpha.test/log/checkpoint",
            "alpha.test/versions/fixture.test/tenant/wrap",
            "alpha.test/log/tile/entries/001.p/1",
            "alpha.test/log/tile/entries/001",
            "alpha.test/log/tile/1/000.p/1",
            "alpha.test/log/tile/1/000",
        ]
    );
    assert_eq!(
        connections, 1,
        "sequential requests share one HTTP/1.1 connection"
    );

    server.serve(ALPHA, "/log/checkpoint", current.clone());
    let witnessed_client = client(&witnessed, &online, cache);
    let head = witnessed_client.release_checkpoint(NOW).unwrap();
    assert!(
        client(&witnessed, &offline, cache)
            .release_from_hint(&head, TARGET, &hint)
            .is_err()
    );
    let found = witnessed_client
        .release_from_hint(&head, TARGET, &hint)
        .unwrap();
    assert_eq!(
        witnessed_client
            .publication_time(&found, Some(257), NOW)
            .unwrap(),
        101
    );
    let requests = server.take().0;
    assert!(requests.contains(&"alpha.test/log/tile/entries/001".into()));
    assert!(requests.contains(&"alpha.test/log/heads/257".into()));

    let local = client(&witnessed, &offline, cache);
    let reloaded = local.release_checkpoint(NOW).unwrap();
    assert_eq!(reloaded.note(), current);
    assert_eq!(
        local
            .release_from_hint(&reloaded, TARGET, &hint)
            .unwrap()
            .root(),
        target
    );
    assert!(
        client(&changed, &offline, cache)
            .release_checkpoint(NOW)
            .is_err()
    );
    assert!(
        client(&changed, &offline, cache)
            .release_from_hint(&head, TARGET, &hint)
            .is_err()
    );
    server.serve(
        BETA,
        "/log/checkpoint",
        note(BETA, Log::Release, 257, root, Some(NOW)),
    );
    let beta = client(&other, &online, cache);
    let beta_head = beta.release_checkpoint(NOW).unwrap();
    assert!(beta.release_from_hint(&beta_head, TARGET, &hint).is_err());
    assert_eq!(
        server.take().0,
        [
            "beta.test/log/checkpoint",
            "beta.test/log/tile/entries/001.p/1",
            "beta.test/log/tile/entries/001",
        ]
    );

    let second = Root([91; 32]);
    let mut rows = vec![retire(target), retire(second)];
    let first = note(ALPHA, Log::Status, 1, merkle(&rows[..1]), Some(NOW));
    let next = note(ALPHA, Log::Status, 2, merkle(&rows), Some(NOW));
    rows.resize(256, retire(Root([92; 32])));
    server.serve(ALPHA, "/status/tile/entries/000", bundle(&rows));
    server.serve(
        ALPHA,
        "/status/tile/0/000",
        rows.iter()
            .flat_map(|row| merkle(std::slice::from_ref(row)))
            .collect(),
    );
    server.serve(ALPHA, "/status/checkpoint", first);
    let replay = witnessed_client.status_replay(NOW, None).unwrap();
    assert!(matches!(
        replay.state(target).unwrap().disposition,
        Disposition::Retired { .. }
    ));
    assert!(replay.state(second).is_none());
    drop(replay);
    server.serve(ALPHA, "/status/checkpoint", next.clone());
    let replay = witnessed_client.status_replay(NOW, None).unwrap();
    assert!(matches!(
        replay.state(second).unwrap().disposition,
        Disposition::Retired { .. }
    ));
    let receipts = server.take().0;
    assert_eq!(
        receipts
            .iter()
            .filter(|path| path.ends_with("/tile/entries/000"))
            .count(),
        1
    );
    let conflict = note(ALPHA, Log::Status, 2, [0x88; 32], Some(NOW));
    server.serve(ALPHA, "/status/checkpoint", conflict.clone());
    assert!(
        witnessed_client
            .status_replay(NOW, None)
            .unwrap_err()
            .to_string()
            .contains("does not extend")
    );
    let state = cache
        .join("registry")
        .join(hex(&Sha256::digest(b"https://alpha.test")))
        .join("status")
        .join(hex(&witnessed_client.route().trust_digest()));
    let evidence = fs::read(
        fs::read_dir(state.join("conflicts"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path(),
    )
    .unwrap();
    assert_eq!(
        &evidence[36..],
        [next.as_slice(), conflict.as_slice()].concat()
    );
    assert_eq!(local.status_replay(NOW, None).unwrap(), replay);
    assert!(
        client(&changed, &offline, cache)
            .status_replay(NOW, None)
            .is_err()
    );
}

fn content(name: &str, bindings: BTreeMap<String, (Id, Binding)>) -> (Id, Root, Vec<u8>) {
    let id = Id {
        name: name.into(),
        major: 1,
    };
    let file = Source {
        module: vec!["library".into()],
        bytes: "pub fn value() -> u64 { 7u64 }\n".into(),
    };
    let value = Content {
        id: id.clone(),
        version: Version::parse("1.2.3").unwrap(),
        bindings,
        files: [("lib.hirsel".into(), file)].into(),
        foreign: BTreeMap::new(),
    };
    let (root, bytes) = value.inventory().unwrap().encode().unwrap();
    (id, root, bytes)
}

pub(crate) fn compress(bytes: &[u8], level: i32, checksum: bool) -> Vec<u8> {
    let mut context = zstd_safe::CCtx::create();
    context
        .set_parameter(zstd_safe::CParameter::CompressionLevel(level))
        .unwrap();
    context
        .set_parameter(zstd_safe::CParameter::ChecksumFlag(checksum))
        .unwrap();
    let mut frame = Vec::with_capacity(zstd_safe::compress_bound(bytes.len()));
    context.compress2(&mut frame, bytes).unwrap();
    frame
}

pub(crate) fn frame_path(root: Root) -> String {
    format!("/c/{}.zst", hex(&root.0))
}

fn cached_path(path: &Path, root: Root) -> PathBuf {
    let name = hex(&root.0);
    path.join(&name[..2]).join(&name[2..])
}

fn builds(server: &Server, dir: &Path, target: Root, child: Root, target_c: &[u8], child_c: &[u8]) {
    let project = dir.join("app");
    let store = dir.join("packages");
    let empty = dir.join("empty");
    let config = dir.join("config");
    fs::create_dir_all(config.join("hirsel")).unwrap();
    fs::write(
        config.join("hirsel/config.toml"),
        route("*", "default.test", false, 3)
            + &route("fixture.test/*", BETA, false, 3)
            + &route("fixture.test/tenant/*", ALPHA, true, 3),
    )
    .unwrap();
    fs::create_dir(&project).unwrap();
    fs::write(project.join("hirsel.toml"), format!(
        "[package]\nname = \"local/app\"\nversion = \"1.0.0\"\ncharge = \"charge-v1\"\n[sources]\n\"main.hirsel\" = \"main\"\n[dependencies]\nwrap = {{ package = {TARGET:?}, major = 1, root = \"{target}\", registry = \"https://alpha.test\" }}\n"
    )).unwrap();
    fs::write(
        project.join("main.hirsel"),
        "pub fn main() -> Result[(), Exit] { Ok(()) }\n",
    )
    .unwrap();
    let libraries = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../core")
        .canonicalize()
        .unwrap();
    let runtime_archive = native_options(crate::Backend::Cranelift, dir)
        .runtime
        .canonicalize()
        .unwrap();
    let runtime = tempfile::Builder::new()
        .prefix("registry-")
        .tempdir_in("/tmp")
        .unwrap();
    let environment = |cmd: &mut std::process::Command| {
        server.route(cmd, dir);
        cmd.env("XDG_CONFIG_HOME", &config)
            .env("XDG_CACHE_HOME", dir.join("cache"))
            .env("XDG_RUNTIME_DIR", runtime.path());
    };
    let run = |args: &[&str], packages: &Path| {
        let mut args = args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        args.extend(["--package-store".into(), packages.display().to_string()]);
        if args[0] == "build" {
            for (flag, path) in [
                ("--libraries", libraries.clone()),
                ("--runtime", runtime_archive.clone()),
                ("--store", dir.join("compiler-store")),
                ("--code-store", dir.join("code-store")),
            ] {
                args.extend([flag.into(), path.display().to_string()]);
            }
        }
        let mut cmd = process::request_command_at(&std::env::current_exe().unwrap(), &args, dir);
        environment(&mut cmd);
        cmd.env("HIRSEL_TRACE_REQUEST_PHASES", "1");
        process::tape_request_output(cmd)
    };
    let first = compress(target_c, 1, false);
    let second = compress(target_c, 9, true);
    assert_ne!(first, second);
    assert_eq!(
        Library::read_frame(first.as_slice(), Some(first.len() as u64))
            .unwrap()
            .0,
        target
    );
    assert_eq!(
        Library::read_frame(second.as_slice(), None).unwrap().0,
        target
    );
    let parent_path = frame_path(target);
    let child_path = frame_path(child);
    server.take();
    serve_target_release(server, target);
    let (row, intent) = signed_takedown(
        ALPHA,
        "fixture.test/tenant",
        target,
        json!(null),
        "removed",
        3,
    );
    let rows = [serde_json::to_vec(&row).unwrap()];
    server.serve(
        ALPHA,
        &format!("/e/{}", hex(&Sha256::digest(&intent))),
        intent,
    );
    server.serve(
        ALPHA,
        "/status/checkpoint",
        note(ALPHA, Log::Status, 1, merkle(&rows), None),
    );
    server.serve(ALPHA, "/status/tile/entries/000.p/1", bundle(&rows));
    server.serve(ALPHA, &parent_path, first.clone());
    let (status, _, error) = run(&["fetch", "app"], &empty);
    assert_eq!(status, Some(2), "{error}");
    assert!(error.contains("taken down"), "{error}");
    assert!(!cached_path(&empty, target).exists());
    assert!(!server.take().0.contains(&format!("{ALPHA}{parent_path}")));
    server.serve(
        ALPHA,
        "/status/checkpoint",
        note(ALPHA, Log::Status, 0, merkle(&[]), None),
    );
    server.redirect(
        ALPHA,
        &parent_path,
        format!("https://beta.test{parent_path}"),
    );
    server.serve(BETA, &parent_path, second.clone());
    let (status, _, error) = run(&["fetch", "app"], &store);
    assert_eq!(status, Some(2), "{error}");
    assert!(error.contains("HTTP 302"), "{error}");
    assert_eq!(
        server.take().0,
        [
            format!("{ALPHA}/status/checkpoint"),
            format!("{ALPHA}{parent_path}")
        ]
    );
    assert!(!project.join("hirsel.lock").exists());

    server.serve(ALPHA, &parent_path, first);
    server.serve(ALPHA, &child_path, compress(child_c, 3, true));
    let (status, _, error) = run(&["fetch", "app"], &store);
    assert_eq!(status, Some(0), "{error}");
    let (requests, connections) = server.take();
    assert_eq!(
        requests,
        [
            format!("{ALPHA}/status/checkpoint"),
            format!("{ALPHA}{parent_path}"),
            format!("{ALPHA}{child_path}")
        ]
    );
    assert_eq!(
        connections, 1,
        "exact children reuse the carrier connection"
    );
    assert_eq!(fs::read(cached_path(&store, target)).unwrap(), target_c);
    assert_eq!(fs::read(cached_path(&store, child)).unwrap(), child_c);
    let lock = fs::read_to_string(project.join("hirsel.lock")).unwrap();
    assert!(lock.contains("source = \"registry:https://alpha.test\""));
    assert!(lock.contains(&format!("source = \"via:{TARGET}@1\"")));
    fs::remove_file(cached_path(&store, child)).unwrap();
    let (status, _, error) = run(&["fetch", "app", "--locked"], &store);
    assert_eq!(status, Some(0), "{error}");
    assert_eq!(
        server.take().0,
        [
            format!("{ALPHA}/status/checkpoint"),
            format!("{ALPHA}{child_path}")
        ]
    );

    let frozen = |resident: bool| {
        let output = if resident {
            "resident-output"
        } else {
            "oneshot-output"
        };
        for (cache, code) in [(&store, 0), (&empty, 2)] {
            let (status, _, error) = run(&["build", "app", "--frozen", "-o", output], cache);
            assert_eq!(status, Some(code), "{error}");
            assert_eq!(error.contains("hirsel-request-phases"), resident, "{error}");
            if code != 0 {
                assert!(error.contains("--frozen does not fetch"), "{error}");
            }
            assert_eq!(
                server.take(),
                (Vec::new(), 0),
                "frozen build touched registry transport"
            );
        }
    };
    frozen(false);
    let child = start_resident(runtime.path(), dir, environment);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| frozen(true)));
    stop_resident(runtime.path(), dir, child);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn route_and_fetch_contract() {
    route_checks();
    let work = tempfile::tempdir().unwrap();
    let server = Server::start();
    let (child_id, child, child_c) = content("unlogged.test/json", BTreeMap::new());
    let (_, target, target_c) = content(
        TARGET,
        [("json".into(), (child_id, Binding::Exact(child)))].into(),
    );
    proofs(&server, &work.path().join("proofs"), target);
    builds(&server, work.path(), target, child, &target_c, &child_c);
}

#[derive(Clone)]
enum Response {
    Ok(Vec<u8>),
    Redirect(String),
    Missing,
}

#[derive(Default)]
struct Wire {
    objects: BTreeMap<String, Response>,
    requests: Vec<String>,
    connections: usize,
}

pub(crate) struct Server {
    state: Arc<Mutex<Wire>>,
    proxy: String,
    stop: Option<std::sync::mpsc::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    pub(crate) fn start() -> Self {
        let configs = Arc::new(BTreeMap::from([
            (
                ALPHA,
                tls_config(
                    include_bytes!("../../../hirsel-core-rt/testdata/tls/alpha.pem"),
                    include_bytes!("../../../hirsel-core-rt/testdata/tls/alpha.key.pem"),
                ),
            ),
            (
                BETA,
                tls_config(
                    include_bytes!("../../../hirsel-core-rt/testdata/tls/beta.pem"),
                    include_bytes!("../../../hirsel-core-rt/testdata/tls/beta.key.pem"),
                ),
            ),
        ]));
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        let (stop, stopped) = std::sync::mpsc::channel();
        let state = Arc::new(Mutex::new(Wire::default()));
        let wire = state.clone();
        let worker = thread::spawn(move || {
            let mut clients = Vec::new();
            loop {
                match listener.accept() {
                    Ok((socket, _)) => {
                        wire.lock().unwrap().connections += 1;
                        let wire = wire.clone();
                        let configs = configs.clone();
                        clients.push(thread::spawn(move || handle(socket, &configs, &wire)));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stopped.recv_timeout(Duration::from_millis(2))
                            != Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                        {
                            break;
                        }
                    }
                    Err(error) => panic!("{error}"),
                }
            }
            for client in clients {
                client.join().unwrap();
            }
        });
        Self {
            state,
            proxy,
            stop: Some(stop),
            worker: Some(worker),
        }
    }

    pub(crate) fn serve(&self, host: &str, path: &str, bytes: Vec<u8>) {
        self.state
            .lock()
            .unwrap()
            .objects
            .insert(format!("{host}{path}"), Response::Ok(bytes));
    }

    fn missing(&self, host: &str, path: &str) {
        self.state
            .lock()
            .unwrap()
            .objects
            .remove(&format!("{host}{path}"));
    }

    fn redirect(&self, host: &str, path: &str, location: String) {
        self.state
            .lock()
            .unwrap()
            .objects
            .insert(format!("{host}{path}"), Response::Redirect(location));
    }

    pub(crate) fn take(&self) -> (Vec<String>, usize) {
        let mut wire = self.state.lock().unwrap();
        (
            std::mem::take(&mut wire.requests),
            std::mem::take(&mut wire.connections),
        )
    }

    pub(crate) fn route(&self, command: &mut std::process::Command, dir: &Path) {
        let ca = dir.join("ca.pem");
        if !ca.exists() {
            fs::write(&ca, TLS_CA).unwrap();
        }
        command
            .env("ALL_PROXY", &self.proxy)
            .env("NO_PROXY", "")
            .env_remove("no_proxy")
            .env_remove("HTTPS_PROXY")
            .env_remove("https_proxy")
            .env_remove("all_proxy")
            .env("SSL_CERT_FILE", ca)
            .env_remove("SSL_CERT_DIR");
    }

    fn transport(&self) -> Transport {
        use ureq::tls::{Certificate, RootCerts, TlsConfig};
        let tls = TlsConfig::builder()
            .root_certs(RootCerts::from([Certificate::from_pem(TLS_CA).unwrap()]))
            .unversioned_rustls_crypto_provider(rustls::crypto::ring::default_provider().into())
            .build();
        let agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .proxy(Some(ureq::Proxy::new(&self.proxy).unwrap()))
            .build()
            .new_agent();
        Transport::with_agent(agent)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(self.stop.take());
        self.worker.take().unwrap().join().unwrap();
    }
}

fn tls_config(cert: &[u8], key: &[u8]) -> Arc<rustls::ServerConfig> {
    Arc::new(
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
    )
}

fn request_line(reader: &mut impl BufRead) -> std::io::Result<String> {
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let mut line = String::new();
    while !first.is_empty() && reader.read_line(&mut line)? != 0 && line != "\r\n" {
        line.clear();
    }
    Ok(first)
}

fn handle(
    socket: TcpStream,
    configs: &BTreeMap<&str, Arc<rustls::ServerConfig>>,
    state: &Mutex<Wire>,
) {
    socket.set_nonblocking(false).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut proxy = BufReader::new(socket);
    let Ok(connect) = request_line(&mut proxy) else {
        return;
    };
    let Some(host) = connect
        .strip_prefix("CONNECT ")
        .and_then(|line| line.split_once(":443 "))
        .map(|(host, _)| host)
    else {
        return;
    };
    let Some(config) = configs.get(host) else {
        return;
    };
    let mut socket = proxy.into_inner();
    if socket
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .is_err()
    {
        return;
    }
    let connection = rustls::ServerConnection::new(config.clone()).unwrap();
    let mut tls = BufReader::new(rustls::StreamOwned::new(connection, socket));
    while let Ok(line) = request_line(&mut tls) {
        let Some(path) = line
            .strip_prefix("GET ")
            .and_then(|line| line.split_once(' '))
            .map(|(path, _)| path)
        else {
            break;
        };
        let key = format!("{host}{path}");
        let response = {
            let mut state = state.lock().unwrap();
            state.requests.push(key.clone());
            state
                .objects
                .get(&key)
                .cloned()
                .unwrap_or(Response::Missing)
        };
        let tls = tls.get_mut();
        let sent = match response {
            Response::Ok(bytes) => write!(
                tls,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                bytes.len()
            )
            .and_then(|_| tls.write_all(&bytes)),
            Response::Redirect(location) => write!(
                tls,
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
            ),
            Response::Missing => {
                tls.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
            }
        };
        if sent.and_then(|()| tls.flush()).is_err() {
            break;
        }
    }
}
