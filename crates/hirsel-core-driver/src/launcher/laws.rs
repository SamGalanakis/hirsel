//! The launcher's laws (docs/spec/launch.md "Laws"; RUNNER-SPEC §14.3-§14.5), run on the
//! launcher's one state loop with stand-in children: shell scripts that take the
//! descriptors of launch.md "Child modes" and behave as the law needs. The real-place
//! halves (a NEW head opening the store only after OLD closed it) are the platform host's.
use super::session::tests::serve_fixture as serve;
use super::session::*;
use super::{Image, Images, Launcher, Lifecycle::*, bootstrap::Bootstrap, hex};
use std::collections::BTreeSet;
use std::fs;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Stand-in images: an assembly name's first byte picks a script. Objects are files named
/// by the hex of a 32-byte name.
pub(super) struct Scripts {
    scripts: Vec<PathBuf>,
    objects: PathBuf,
}

impl Images for Scripts {
    fn resolve(&self, assembly: [u8; 32]) -> Result<Image, String> {
        let path = self
            .scripts
            .get(assembly[0] as usize)
            .ok_or("no such image")?
            .clone();
        Ok(Image {
            path,
            executable: super::ImageFile::Script,
            digest: [assembly[0]; 32],
            n_place: 1 << 16,
            prefix: Vec::new(),
        })
    }

    fn collect(
        &self,
        keep: &BTreeSet<[u8; 32]>,
        _: &dyn Fn(crate::code_admission::ObjectKey) -> bool,
    ) -> std::io::Result<u64> {
        let kept: BTreeSet<_> = keep.iter().map(|k| hex::encode(k)).collect();
        let mut removed = 0;
        for entry in fs::read_dir(&self.objects)? {
            let entry = entry?;
            if !kept.contains(entry.file_name().to_str().unwrap()) {
                fs::remove_file(entry.path())?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

fn images(dir: &Path, scripts: &[&str]) -> Scripts {
    let objects = dir.join("objects");
    fs::create_dir_all(&objects).unwrap();
    let scripts = scripts
        .iter()
        .enumerate()
        .map(|(i, body)| {
            let path = dir.join(format!("image-{i}"));
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        })
        .collect();
    Scripts { scripts, objects }
}

pub(super) struct Rig<I = Scripts> {
    pub(super) dir: tempfile::TempDir,
    pub(super) launcher: Launcher<I>,
}

/// A rig whose image `i` is the shell script body `scripts[i]`.
fn rig(scripts: &[&str]) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let state = super::private_dir(dir.path().join("state")).unwrap();
    let launcher = Launcher::new(&state, Leader, || Ok(images(dir.path(), scripts))).unwrap();
    Rig { dir, launcher }
}

/// A serving stand-in that runs until SIGTERM and then exits 0 with no report.
const SLEEP: &str = "trap 'exit 0' TERM\nwhile :; do sleep 0.02; done";

/// A stand-in that is one process with no descendant, so that once it is killed nothing
/// still holds the group's listener (possible for descendants without delegation).
const SINGLE: &str = "exec sleep 30";

pub(super) fn group() -> Group {
    Group {
        name: "g".into(),
        incarnation: 1,
    }
}

pub(super) fn attempt(n: u64) -> Attempt {
    Attempt { hi: n, lo: n }
}

fn launch(image: u8, workdir: &Path, mode: Mode) -> Request {
    let mut assembly = vec![0; 32];
    assembly[0] = image;
    Request::Start(Launch {
        group: group(),
        assembly,
        workdir: workdir.to_str().unwrap().into(),
        args: vec!["app".into()],
        mode,
        recording: None,
    })
}

fn serving(g: u64) -> Child {
    Child {
        group: group(),
        role: Role::Serving(g),
    }
}

fn role(role: Role) -> Child {
    Child {
        group: group(),
        role,
    }
}

fn ended(events: &[Event], child: &Child) -> bool {
    events
        .iter()
        .any(|e| matches!(e, Event::Ended(c, _) if c == child))
}

impl<I: Images> Rig<I> {
    fn work(&self) -> PathBuf {
        self.dir.path().to_owned()
    }

    pub(super) fn ask(&mut self, request: Request) -> Vec<Event> {
        self.launcher.ask_request(request)
    }

    pub(super) fn bind(&mut self) -> std::net::SocketAddr {
        let bind = Request::Bind(group(), "127.0.0.1:0".into());
        assert_eq!(self.ask(bind), [Event::Bound(group())]);
        self.launcher.listener_addr(&group()).unwrap()
    }

    /// Run the loop until `done` holds for the events seen so far, and return them.
    fn until(&mut self, done: impl Fn(&[Event]) -> bool) -> Vec<Event> {
        let end = Instant::now() + Duration::from_secs(10);
        let mut seen = self.launcher.take_events();
        while !done(&seen) {
            assert!(Instant::now() < end, "timed out with {seen:?}");
            self.launcher.turn(Duration::from_millis(20)).unwrap();
            seen.extend(self.launcher.take_events());
        }
        seen
    }

    /// Run the loop for `period` and return every event.
    fn idle(&mut self, period: Duration) -> Vec<Event> {
        let end = Instant::now() + period;
        let mut seen = self.launcher.take_events();
        while Instant::now() < end {
            self.launcher.turn(Duration::from_millis(10)).unwrap();
            seen.extend(self.launcher.take_events());
        }
        seen
    }

    fn inventory(&mut self) -> (Vec<ChildState>, Vec<Group>) {
        match self.ask(Request::Hello).as_slice() {
            [Event::Inventory(children, cold, _)] => (children.clone(), cold.clone()),
            other => panic!("Hello answered {other:?}"),
        }
    }
}

#[test]
fn launch_identity_is_the_verified_image() {
    use super::objects::Records;
    use super::stores::Stores;
    use crate::code_store::{CodeStore, ObjectKind};
    use hirsel_core_ids::{ContentHash, DefPath};
    use hirsel_core_ir::{Identity, release_recipe};
    use rustls::crypto::ring::cipher_suite::TLS13_AES_128_GCM_SHA256;
    use std::io::Read;
    use std::process::Command;

    // The replacement happens after the real store has checked the image and
    // before the launcher execs it. The next resolve sees the replacement.
    struct SwapImage {
        stores: Stores,
        replacement: PathBuf,
    }
    impl Images for SwapImage {
        fn resolve(&self, assembly: [u8; 32]) -> Result<Image, String> {
            let image = self.stores.resolve(assembly)?;
            fs::rename(&self.replacement, &image.path).unwrap();
            Ok(image)
        }

        fn collect(
            &self,
            keep: &BTreeSet<[u8; 32]>,
            reserved: &dyn Fn(crate::code_admission::ObjectKey) -> bool,
        ) -> std::io::Result<u64> {
            self.stores.collect(keep, reserved)
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let work = dir.path();
    let options = crate::native::tests::native_options(crate::Backend::Cranelift, work);
    let source = work.join("launch-identity.c");
    fs::write(
        &source,
        r#"
#include "hirsel_core_rt_host.h"
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static uint8_t entry(HirselCoreContext *ctx, const HirselCoreProcessConfig *cfg,
                     HirselCoreMainResult *result) {
    uint8_t digest[32];
    if (hirsel_core_launch_word(ctx, 1, 0) != sizeof(digest)) return 1;
    for (size_t i = 0; i < sizeof(digest); ++i)
        digest[i] = hirsel_core_launch_word(ctx, 2, i);
    int client = hirsel_core_launch_word(ctx, 12, 0) ? accept(3, NULL, NULL) : 1;
    if (client < 0 || write(client, digest, sizeof(digest)) != sizeof(digest)) return 1;
    result->kind = 0;
    return 0;
}

int main(int argc, char **argv) {
    HirselCoreView args[argc];
    for (int i = 1; i < argc; ++i)
        args[i - 1] = (HirselCoreView){(const uint8_t *)argv[i], strlen(argv[i])};
    const HirselCoreOsConfig config = {
        .root_limit = 1048576, .args = args, .args_len = argc - 1,
        .stdout_fd = 1, .stderr_fd = 2,
    };
    return hirsel_core_os_run(&config, NULL, entry);
}
"#,
    )
    .unwrap();
    let include = Path::new(env!("CARGO_MANIFEST_DIR")).join("../hirsel-core-rt/include");
    crate::native::command(
        Command::new(options.link.driver())
            .arg("-I")
            .arg(include)
            .arg(source)
            .arg(&options.runtime)
            .args(crate::host::RUNTIME_LIBRARIES)
            .arg("-o")
            .arg(&options.output),
    )
    .unwrap();
    let bytes = fs::read(&options.output).unwrap();
    let rustls::SupportedCipherSuite::Tls13(suite) = TLS13_AES_128_GCM_SHA256 else {
        unreachable!();
    };
    let expected = suite.common.hash_provider.hash(&bytes);
    let digest = ContentHash(expected.as_ref().try_into().unwrap());
    let standalone = Command::new(&options.output).output().unwrap();
    assert!(standalone.status.success(), "{standalone:?}");
    assert_eq!(standalone.stdout, digest.0);

    let state = work.join("state");
    let objects = state.join("objects");
    fs::create_dir_all(&objects).unwrap();
    let records = Records::open(&objects).unwrap();
    let recipe = release_recipe::Recipe {
        entry: Identity::plain(DefPath::from_full("app@1/main/main").unwrap()),
        address: ContentHash([0; 32]),
        groups: vec![],
        bodies: vec![],
        foreign_modules: vec![],
        foreign_archives: vec![],
        toolchain: vec![],
        values: vec![],
        launch: release_recipe::Launch {
            backend: release_recipe::Backend::Cranelift,
            target: options.target,
            root_limit: 1 << 20,
            n_place: 1 << 16,
            exit_status: true,
            grants: vec![],
        },
    };
    let recipe = CodeStore::new(state.join("code"))
        .put(ObjectKind::Recipe, &recipe.encode())
        .unwrap()
        .hash;
    let toolchain = ContentHash([0; 32]);
    let frame = [recipe.0, toolchain.0, digest.0].concat();
    let identity = ContentHash::of_parts("hirsel.assembly-record.v1", [frame.as_slice()]);
    records
        .put_assembly(
            &crate::AssemblyRecord {
                identity,
                recipe,
                toolchain,
                digest,
            },
            &bytes,
        )
        .unwrap();
    drop(records);
    let replacement = work.join("replacement");
    fs::write(&replacement, b"#!/bin/sh\ntouch incorrectly-executed\n").unwrap();
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o500)).unwrap();
    let launcher = Launcher::new(&state, Leader, || {
        Ok(SwapImage {
            stores: Stores::open(&state)?,
            replacement,
        })
    })
    .unwrap();
    let mut rig = Rig { dir, launcher };
    let address = rig.bind();
    let start = |generation| {
        Request::Start(Launch {
            group: group(),
            assembly: identity.0.to_vec(),
            workdir: rig.work().to_str().unwrap().into(),
            args: vec![],
            mode: Mode::Serving(generation, serve(0, None)),
            recording: None,
        })
    };
    let (first, second) = (start(1), start(2));
    let mut peer = TcpStream::connect(address).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let child = serving(1);
    assert_eq!(
        rig.ask(first),
        [Event::Launched(child.clone(), Phase::Started)]
    );
    let mut observed = [0; 32];
    peer.read_exact(&mut observed).unwrap();
    assert_eq!(observed, digest.0);
    rig.until(|events| ended(events, &child));
    assert_eq!(
        rig.ask(second),
        [Event::Refused(
            serving(2),
            Refusal::Image("image: image digest".into())
        )],
    );
    assert!(!rig.work().join("incorrectly-executed").exists());
}

/// `group_one_head`, launcher half (RUNNER-SPEC §14.3): one store-holding launch per group,
/// except the one candidate that names the live serving child's gen; a repeated key starts
/// nothing; a child that opens no store is not counted, and an ended one holds nothing.
#[test]
fn group_one_head() {
    let mut rig = rig(&[SLEEP, "exit 0"]);
    let work = rig.work();
    rig.bind();
    let started = |c| vec![Event::Launched(c, Phase::Started)];
    let held = |c| vec![Event::Refused(c, Refusal::Held)];
    let first = launch(0, &work, Mode::Serving(1, serve(1, None)));
    assert_eq!(rig.ask(first.clone()), started(serving(1)));
    let second = |candidate| launch(0, &work, Mode::Serving(2, serve(2, candidate)));
    assert_eq!(rig.ask(second(None)), held(serving(2)));
    assert_eq!(rig.ask(second(Some(7))), held(serving(2)));
    assert_eq!(rig.ask(second(Some(1))), started(serving(2)));
    // A cutover has two heads at most: a second candidate is refused.
    assert_eq!(
        rig.ask(launch(0, &work, Mode::Serving(3, serve(2, Some(1))))),
        held(serving(3))
    );
    let preparing = role(Role::Prepare(attempt(9)));
    let prepare = Mode::Prepare(attempt(9), "x".into(), Vec::new());
    assert_eq!(
        rig.ask(launch(1, &work, prepare)),
        started(preparing.clone())
    );
    // The same identity again answers its phase and starts no second process.
    assert_eq!(rig.ask(first), started(serving(1)));
    assert_eq!(rig.inventory().0.len(), 3);
    rig.until(|e| ended(e, &preparing));
    rig.ask(Request::Kill(serving(1)));
    rig.ask(Request::Kill(serving(2)));
    rig.until(|e| ended(e, &serving(1)) && ended(e, &serving(2)));
    assert_eq!(
        rig.ask(launch(0, &work, Mode::Serving(4, serve(2, None)))),
        started(serving(4))
    );
    rig.ask(Request::Kill(serving(4)));
    rig.until(|e| ended(e, &serving(4)));
}

/// L46 as amended (RUNNER-SPEC §14.3 "Event order"): a serving child's end carries the
/// listener sample taken after it ended, and an `Arrived` can only follow that end. The
/// watch is level-triggered and reports a readable listener once; each inventory repeats it.
#[test]
fn end_samples_listener_before_watch() {
    // At SIGTERM, write the clean report Stopped(Some(2000)) on descriptor 5 and exit 0;
    // `armed` says the trap is set (a first exec on macOS takes over 100 ms).
    let clean = "trap 'printf \"\\001\\000\\000\\000\\320\\007\\000\\000\\000\\000\\000\\000\" >&5; exit 0' TERM\n: > armed\nwhile :; do sleep 0.02; done";

    // An arrival during the drain is in the end's sample, so no Arrived follows it.
    let mut rig = rig(&[clean]);
    let work = rig.work();
    let address = rig.bind();
    rig.ask(launch(0, &work, Mode::Serving(1, serve(1, None))));
    let waiting = TcpStream::connect(address).unwrap();
    let armed = Instant::now() + Duration::from_secs(10);
    while !work.join("armed").exists() {
        assert!(Instant::now() < armed && rig.idle(Duration::from_millis(20)).is_empty());
    }
    rig.ask(Request::Stop(serving(1)));
    let end = End {
        status: Status::Code(0),
        report: Some(Some(2000)),
        readable: true,
    };
    assert_eq!(
        rig.until(|e| ended(e, &serving(1))),
        [Event::Ended(serving(1), end)]
    );
    assert!(rig.idle(Duration::from_millis(100)).is_empty());
    assert_eq!(rig.inventory().1, [group()]);
    drop(waiting);

    // With nothing waiting at the end, the sample is false and the arrival follows the end.
    let mut rig = self::rig(&[clean]);
    let work = rig.work();
    let address = rig.bind();
    rig.ask(launch(0, &work, Mode::Serving(1, serve(1, None))));
    rig.ask(Request::Stop(serving(1)));
    let events = rig.until(|e| ended(e, &serving(1)));
    assert!(matches!(
        &events[..],
        [Event::Ended(
            _,
            End {
                readable: false,
                ..
            }
        )]
    ));
    let _client = TcpStream::connect(address).unwrap();
    assert_eq!(rig.until(|e| !e.is_empty()), [Event::Arrived(group())]);
    assert!(rig.idle(Duration::from_millis(100)).is_empty());
    assert_eq!(rig.inventory().1, [group()]);
}

/// Send `request` on the host's end and run the launcher until the next event arrives.
fn exchange(launcher: &mut Launcher<Scripts>, host: &mut UnixStream, request: Request) -> Event {
    write_frame(host, &request).unwrap();
    host.set_nonblocking(true).unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    let mut bytes = Vec::new();
    loop {
        launcher.turn(Duration::from_millis(20)).unwrap();
        let mut chunk = [0; 4096];
        if let Ok(n) = std::io::Read::read(host, &mut chunk) {
            bytes.extend_from_slice(&chunk[..n]);
        }
        if bytes.len() >= 4 {
            let size = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
            if bytes.len() >= 4 + size {
                return Event::decode(&bytes[4..4 + size]).unwrap();
            }
        }
        assert!(Instant::now() < end, "no answer to {request:?}");
    }
}

/// `launcher_restart_stateless`, launcher half (RUNNER-SPEC §14.5): a host restart is a new
/// session whose `Hello` inventory names the launcher's children, live or ended and not yet
/// acknowledged; a lost launcher is fenced by the launcher lock, and its successor answers
/// `Hello` with no child and binds the address again.
#[test]
fn launcher_restart_stateless() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().to_owned();
    let state = work.join("state");
    fs::create_dir_all(&state).unwrap();
    let socket = work.join("control");
    let mut launcher = Launcher::new(&state, Leader, || Ok(images(&work, &[SINGLE]))).unwrap();
    launcher
        .listen(UnixListener::bind(&socket).unwrap())
        .unwrap();
    let connect = |launcher: &mut Launcher<Scripts>| {
        let host = UnixStream::connect(&socket).unwrap();
        launcher.turn(Duration::from_millis(20)).unwrap();
        host
    };
    let mut host = connect(&mut launcher);
    assert_eq!(
        exchange(&mut launcher, &mut host, Request::Hello),
        Event::Inventory(vec![], vec![], vec![])
    );
    let bind = Request::Bind(group(), "127.0.0.1:0".into());
    assert_eq!(
        exchange(&mut launcher, &mut host, bind),
        Event::Bound(group())
    );
    let address = launcher.listener_addr(&group()).unwrap();
    let start = launch(0, &work, Mode::Serving(1, serve(1, None)));
    assert_eq!(
        exchange(&mut launcher, &mut host, start),
        Event::Launched(serving(1), Phase::Started)
    );

    // The host dies: the child keeps running and the next host adopts it.
    drop(host);
    for _ in 0..5 {
        launcher.turn(Duration::from_millis(20)).unwrap();
    }
    let mut host = connect(&mut launcher);
    let live = ChildState {
        child: serving(1),
        phase: Phase::Started,
        selection: Some(Selection::new(1, vec![0; 32])),
        answer: None,
    };
    assert_eq!(
        exchange(&mut launcher, &mut host, Request::Hello),
        Event::Inventory(vec![live], vec![], vec![])
    );

    // An end no host saw stays in the inventory until a host acknowledges it.
    write_frame(&mut host, &Request::Kill(serving(1))).unwrap();
    drop(host);
    let end = Instant::now() + Duration::from_secs(10);
    while launcher.is_child_live(&serving(1)) {
        assert!(Instant::now() < end);
        launcher.turn(Duration::from_millis(20)).unwrap();
    }
    let mut host = connect(&mut launcher);
    let Event::Inventory(children, _, _) = exchange(&mut launcher, &mut host, Request::Hello)
    else {
        panic!("Hello answered with no inventory");
    };
    assert!(matches!(
        &children[..],
        [ChildState {
            child,
            phase: Phase::Exited(End { status: Status::Signal(9), report: None, .. }),
            ..
        }] if *child == serving(1)
    ));
    write_frame(&mut host, &Request::Acknowledge(serving(1))).unwrap();
    assert_eq!(
        exchange(&mut launcher, &mut host, Request::Hello),
        Event::Inventory(vec![], vec![], vec![])
    );

    // A second launcher on the same state is fenced by the lock while the first lives.
    assert!(Launcher::new(&state, Leader, || Ok(images(&work, &[SINGLE]))).is_err());
    drop(host);
    drop(launcher);
    let mut successor = Launcher::new(&state, Leader, || Ok(images(&work, &[SINGLE]))).unwrap();
    successor.handle(Request::Hello);
    successor.handle(Request::Bind(group(), address.to_string()));
    assert_eq!(
        successor.take_events(),
        [
            Event::Inventory(vec![], vec![], vec![]),
            Event::Bound(group())
        ]
    );
}

/// `collect_reservation_handoff` (launch.md "Collect"): a reservation released between
/// `CollectBegin` and the root read keeps its objects through that collection and ends
/// after it; a new session drops a fence without removing anything.
#[test]
fn collect_reservation_handoff() {
    let mut rig = rig(&[]);
    let objects = rig.launcher.objects.images.objects.clone();
    let name = |n: u8| [n; 32];
    let exists = |n: u8| objects.join(hex::encode(&name(n))).exists();
    for n in [1, 2, 3] {
        fs::write(objects.join(hex::encode(&name(n))), b"object").unwrap();
    }
    // 1 is a root, 2 is reserved by the deploy attempt, 3 is garbage.
    rig.launcher.reserve(attempt(5), BTreeSet::from([name(2)]));
    assert_eq!(rig.ask(Request::CollectBegin), [Event::Fenced]);
    assert!(rig.ask(Request::Release(attempt(5))).is_empty());
    let roots = Request::Collect(vec![name(1).to_vec()]);
    assert_eq!(rig.ask(roots.clone()), [Event::Collected(1)]);
    assert!(exists(1) && exists(2) && !exists(3));

    // The release took effect when the fence ended: the next collection removes 2.
    rig.ask(Request::CollectBegin);
    assert_eq!(rig.ask(roots), [Event::Collected(1)]);
    assert!(exists(1) && !exists(2));

    // Hello drops a fence; a Collect outside a fence removes nothing.
    rig.ask(Request::CollectBegin);
    assert!(matches!(
        &rig.ask(Request::Hello)[..],
        [Event::Inventory(..)]
    ));
    assert_eq!(rig.ask(Request::Collect(vec![])), [Event::Collected(0)]);
    assert!(exists(1));

    // A host that cannot read its roots aborts: nothing is removed, the fence ends at
    // once, and the release it deferred applies, so the next collection removes 4.
    fs::write(objects.join(hex::encode(&name(4))), b"object").unwrap();
    rig.launcher.reserve(attempt(6), BTreeSet::from([name(4)]));
    rig.ask(Request::CollectBegin);
    assert!(rig.ask(Request::Release(attempt(6))).is_empty());
    assert_eq!(rig.ask(Request::CollectAbort), [Event::Collected(0)]);
    assert!(exists(1) && exists(4));
    rig.ask(Request::CollectBegin);
    assert_eq!(
        rig.ask(Request::Collect(vec![name(1).to_vec()])),
        [Event::Collected(1)]
    );
    assert!(exists(1) && !exists(4));
}

/// `child_mode_authority`, launcher half (launch.md "Child modes"): the serving child holds
/// the listener on 3, its endpoint on 4 and its report on 5; no other mode holds a
/// descriptor past 2; no child holds the control socket; a prepare child is pointed at its
/// export directory in the group's scratch; describe and preview answer on standard output.
#[test]
fn child_mode_authority() {
    // Each stand-in writes its inherited descriptors 3-9 with what they are, then its
    // arguments, to fds-<name>.
    let report = |name: &str| {
        format!(
            "out=\"$PWD/fds-{name}\"; : > \"$out\"\nfor n in 3 4 5 6 7 8 9; do {}; done\necho \"args $*\" >> \"$out\"",
            crate::host::FD_REPORT
        )
    };
    let scripts = [
        format!("{}\n{SLEEP}", report("serving")),
        format!("{}\nprintf described", report("describe")),
        format!("{}\ncat", report("preview")),
        report("prepare"),
        report("install"),
    ];
    let scripts: Vec<&str> = scripts.iter().map(String::as_str).collect();
    let mut rig = rig(&scripts);
    let work = rig.work();
    rig.bind();
    rig.launcher
        .listen(UnixListener::bind(work.join("control")).unwrap())
        .unwrap();
    let control = crate::host::fd_report(rig.launcher.control_fd().unwrap());
    let held = |name: &str| -> Vec<String> {
        fs::read_to_string(work.join(format!("fds-{name}")))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    };

    let answer_before_end = |events: &[Event], child: Child, answer: Vec<u8>| {
        let end = events
            .iter()
            .position(|event| matches!(event, Event::Ended(id, _) if id == &child))
            .unwrap();
        assert!(
            events[..end].contains(&Event::Answered(child, answer)),
            "{events:?}"
        );
    };
    let describing = role(Role::Describe(attempt(1)));
    rig.ask(launch(1, &work, Mode::Describe(attempt(1))));
    let events = rig.until(|e| ended(e, &describing));
    answer_before_end(&events, describing, b"described".to_vec());
    let previewing = role(Role::Preview(attempt(2)));
    let old = b"old entries".to_vec();
    rig.ask(launch(2, &work, Mode::Preview(attempt(2), old.clone())));
    let events = rig.until(|e| ended(e, &previewing));
    answer_before_end(&events, previewing, old);
    let preparing = role(Role::Prepare(attempt(3)));
    rig.ask(launch(
        3,
        &work,
        Mode::Prepare(attempt(3), "export-1".into(), vec![0xf0]),
    ));
    rig.until(|e| ended(e, &preparing));
    for name in ["describe", "preview", "prepare"] {
        let lines = held(name);
        assert!(
            lines.len() == 1 && lines[0].starts_with("args "),
            "{name}: {lines:?}"
        );
    }
    let export = rig.launcher.scratch_of(&group()).join("export-1");
    let attempt_hex = "03".to_owned() + &"00".repeat(7) + "03" + &"00".repeat(7);
    let image_hex = "03".repeat(32);
    assert!(held("prepare")[0].ends_with(&format!(
        "--hirsel-prepare {} {attempt_hex} {image_hex} f0",
        export.display()
    )));

    let install = Mode::Install(1, InstallKind::Genesis, Vec::new());
    rig.ask(launch(4, &work, install));
    rig.until(|events| ended(events, &role(Role::Install(1))));
    assert_eq!(
        held("install"),
        ["args --hirsel-n-place=65536 --hirsel-install-child=0 app"]
    );

    rig.ask(launch(0, &work, Mode::Serving(1, serve(5, None))));
    let end = Instant::now() + Duration::from_secs(10);
    while !fs::read_to_string(work.join("fds-serving")).is_ok_and(|s| s.contains("args")) {
        assert!(Instant::now() < end);
        rig.launcher.turn(Duration::from_millis(20)).unwrap();
    }
    let lines = held("serving");
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(lines[0].starts_with("3 socket:[") && lines[1].starts_with("4 socket:["));
    assert!(lines[2].starts_with("5 pipe:["));
    assert!(lines.iter().all(|l| !l.contains(&control)));
    assert!(lines[3].starts_with("args --hirsel-wake --hirsel-idle=250 "));
    assert!(lines[3].ends_with("--hirsel-activation=5 app"));
    rig.ask(Request::Kill(serving(1)));
    rig.until(|e| ended(e, &serving(1)));
}

include!("../host/launcher_cgroup_law.rs");

/// Admission over the session (launch.md "The launcher", Requests 12-16): the launcher
/// relays the request and objects to the verifier, answers the verifier's Need as
/// `Need` events, and reports the verified root; an attested root the recipe does not
/// hash to refuses before any Need; a child's end and a collection are answered while an
/// upload is half sent.
#[test]
fn session_admission() {
    use crate::code_admission::ObjectKind;
    use crate::code_admission::code_store::{Fixture, limits};
    use std::sync::Arc;
    let mut rig = rig(&[SLEEP]);
    rig.bind();
    let state = rig.work().join("state");
    let objects = rig.work().join("objects");
    fs::create_dir_all(&objects).unwrap();
    let records = Arc::new(super::objects::Records::open(&objects).unwrap());
    let admission = super::Admission::open(&state, records, limits(), 1 << 20).unwrap();
    rig.launcher.admit_with(admission).unwrap();
    let fixture = Fixture::leaf();
    let (root, frame) = fixture.request();
    let admit = |a: u64, root: &[u8]| {
        Request::Admit(
            group(),
            attempt(a),
            Input::Attested(root.to_vec()),
            Approvals::default(),
            super::admit::fixture_deadline_ms(),
        )
    };
    rig.ask(admit(1, &[7; 32]));
    rig.ask(Request::AdmitData(attempt(1), frame.clone()));
    rig.ask(Request::AdmitEnd(attempt(1)));
    let seen = rig.until(|seen| seen.iter().any(|e| matches!(e, Event::AdmissionFailed(..))));
    assert!(
        seen.iter()
            .all(|e| matches!(e, Event::AdmissionFailed(a, _) if *a == attempt(1))),
        "a recipe that is not the attested root refuses, with no Need: {seen:?}"
    );

    rig.ask(admit(2, &root.0));
    rig.ask(Request::AdmitData(attempt(2), frame));
    let seen = rig.until(|seen| seen.iter().any(|e| matches!(e, Event::Need(_, _, true))));
    let mut need = Vec::new();
    for event in seen {
        match event {
            Event::Need(a, keys, _) if a == attempt(2) => need.extend(keys),
            other => panic!("before the last Need: {other:?}"),
        }
    }
    let mut wanted: Vec<_> = fixture
        .recipe
        .groups
        .iter()
        .map(|g| (ObjectKind::Group, g.hash))
        .collect();
    for body in &fixture.recipe.bodies {
        wanted.push((ObjectKind::Body, body.body.hash));
        wanted.push((ObjectKind::Annex, body.annex.hash));
    }
    let mut asked: Vec<_> = need.iter().map(|k| (k.kind, k.hash)).collect();
    asked.sort();
    wanted.sort();
    assert_eq!(
        asked, wanted,
        "an empty store needs every object of the closure"
    );
    // A slow upload: half the objects, then a child's end and a collection go through
    // while the attempt waits for the rest.
    let objects = fixture.objects(&need);
    let (head, tail) = objects.split_at(objects.len() / 2);
    rig.ask(Request::AdmitData(attempt(2), head.to_vec()));
    let child = serving(1);
    rig.ask(launch(0, &rig.work(), Mode::Serving(1, serve(1, None))));
    rig.ask(Request::Kill(child.clone()));
    let seen = rig.until(|seen| ended(seen, &child));
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, Event::Admitted(..) | Event::AdmissionFailed(..)))
    );
    assert_eq!(rig.ask(Request::CollectBegin), [Event::Fenced]);
    assert!(matches!(
        rig.ask(Request::Collect(vec![])).as_slice(),
        [Event::Collected(_)]
    ));
    rig.ask(Request::AdmitData(attempt(2), tail.to_vec()));
    rig.ask(Request::AdmitEnd(attempt(2)));
    let seen = rig.until(|seen| !seen.is_empty());
    match seen.as_slice() {
        [Event::Admitted(a, verified, summary, _)] => {
            assert_eq!(*a, attempt(2));
            assert_eq!(
                verified[..],
                root.0,
                "the verified root is the attested one"
            );
            assert_eq!(summary.root_limit, fixture.recipe.launch.root_limit);
        }
        other => panic!("after AdmitEnd: {other:?}"),
    }
}

/// Cutover retention (RUNNER-SPEC §14.5): OLD's first answer to a cutover is kept by
/// (OLD, attempt) until `CutoverAck`, listed in the inventory, and returned to an identical
/// re-send without forwarding it again; a different command for the key is refused; an OLD
/// that ends unanswered leaves `Uncertain`, which a re-send after its end returns.
#[test]
fn cutover_reply_retained() {
    let cutover = |a: u64, digest: u8| {
        let mut id = a.to_le_bytes().to_vec();
        id.extend_from_slice(&a.to_le_bytes());
        HostCommand::Cutover(id, vec![digest], 2000, false)
    };
    let mut frame = Vec::new();
    super::session::write_frame(&mut frame, &cutover(3, 0xbb)).unwrap();
    let mut reply = Vec::new();
    super::session::write_frame(&mut reply, &HostReply::Installed(4, true)).unwrap();
    let octal: String = reply.iter().map(|b| format!("\\{b:03o}")).collect();
    let n = frame.len();
    // OLD 0 answers its first command and records every byte forwarded to it; OLD 1
    // reads its command and ends without answering.
    let answers = format!("head -c {n} <&4 > got-0\nprintf '{octal}' >&4\nexec cat <&4 >> got-0");
    let silent = format!("head -c {n} <&4 > got-1");
    let mut rig = rig(&[&answers, &silent]);
    let work = rig.work();
    rig.bind();
    let old = serving(1);
    assert_eq!(
        rig.ask(launch(0, &work, Mode::Serving(1, serve(1, None)))),
        [Event::Launched(old.clone(), Phase::Started)]
    );
    let send = |c: &Child, a: u64, d: u8| Request::Command(c.clone(), cutover(a, d));
    let decided = Event::CutoverResult(old.clone(), attempt(3), HostReply::Installed(4, true));
    rig.ask(send(&old, 3, 0xbb));
    let seen = rig.until(|seen| !seen.is_empty());
    assert_eq!(seen, [decided.clone()]);

    let held = |rig: &mut Rig| match rig.ask(Request::Hello).as_slice() {
        [Event::Inventory(_, _, cutovers)] => cutovers.clone(),
        other => panic!("Hello answered {other:?}"),
    };
    assert_eq!(
        held(&mut rig),
        [(old.clone(), attempt(3))],
        "a new session still holds it"
    );
    assert_eq!(
        rig.ask(send(&old, 3, 0xbb)),
        [decided],
        "a re-send returns the kept reply"
    );
    assert!(matches!(
        rig.ask(send(&old, 3, 0xcc)).as_slice(),
        [Event::Placed(c, HostReply::Refused(_))] if *c == old
    ));
    rig.idle(Duration::from_millis(200));
    assert_eq!(
        fs::read(work.join("got-0")).unwrap(),
        frame,
        "OLD saw the cutover exactly once"
    );
    assert!(
        rig.ask(Request::CutoverAck(old.clone(), attempt(3)))
            .is_empty()
    );
    assert!(held(&mut rig).is_empty(), "the acknowledgement drops it");

    rig.ask(Request::Kill(old.clone()));
    rig.until(|seen| ended(seen, &old));
    rig.ask(Request::Acknowledge(old.clone()));
    let silent_old = serving(2);
    rig.ask(launch(1, &work, Mode::Serving(2, serve(2, None))));
    rig.ask(send(&silent_old, 4, 0xbb));
    let seen = rig.until(|seen| ended(seen, &silent_old));
    let uncertain = Event::CutoverResult(silent_old.clone(), attempt(4), HostReply::Uncertain);
    assert!(seen.contains(&uncertain), "{seen:?}");
    assert_eq!(rig.ask(send(&silent_old, 4, 0xbb)), [uncertain]);
}

/// Run `script` as the platform host with an operator listener and published bounds.
fn hosted(rig: &mut Rig, script: &str) -> PathBuf {
    let work = rig.work();
    let program = work.join("host");
    fs::write(&program, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let state = work.join("state");
    super::admit::write_envelope(&state, &super::limits()).unwrap();
    let operator = super::bind_control(&work.join("operator.sock")).unwrap();
    rig.launcher.bootstrap = Some(Bootstrap::new(program, vec![], state, operator));
    work
}

/// Only the host's own process holds the control session: another process of the same
/// user that connects to launcher.sock is closed, so it can neither take over the session
/// nor end the host's (launch.md "The operator socket").
#[test]
fn control_peer_is_the_host() {
    let mut rig = rig(&[]);
    let socket = rig.work().join("launcher.sock");
    rig.launcher
        .listen(super::bind_control(&socket).unwrap())
        .unwrap();
    hosted(&mut rig, "exec sleep 30");
    rig.idle(Duration::from_millis(50));
    assert!(rig.launcher.bootstrap.as_ref().unwrap().pid().is_some());
    let _foreign = UnixStream::connect(&socket).unwrap();
    rig.idle(Duration::from_millis(50));
    assert!(
        !rig.launcher.has_session(),
        "a process that is not the host was admitted to the control session"
    );
}

/// The host is started again after exit 75 at once, then after 1 s, 2 s ...; never after
/// 74 (the catalog is unreadable) or 64 (usage), whose status the launcher ends with.
#[test]
fn host_restart_wired() {
    let mut rig = rig(&[]);
    let work = hosted(&mut rig, "echo x >> ../starts\nexit 75");
    rig.idle(Duration::from_millis(2500));
    let starts = fs::read_to_string(work.join("starts"))
        .unwrap()
        .lines()
        .count();
    assert_eq!(starts, 3, "starts at 0 s, 0 s and 1 s within 2.5 s");

    let mut rig = self::rig(&[]);
    let work = hosted(&mut rig, "echo x >> ../starts\nexit 74");
    rig.idle(Duration::from_secs(2));
    let starts = fs::read_to_string(work.join("starts"))
        .unwrap()
        .lines()
        .count();
    assert_eq!(starts, 1, "a host that ended 74 was started again");
    assert_eq!(rig.launcher.bootstrap.as_ref().unwrap().stopped(), Some(74));
}

/// `launcher_code_collection` (launch.md "The launcher"): a collection keeps each code
/// object a kept assembly's recipe lists and each object an admission in flight reserved,
/// and removes old, unrooted objects. A fresh publication still admits after collection.
#[test]
fn code_collection_keeps_reached_reserved_and_freshly_published_objects() {
    use super::stores::Stores;
    use crate::code_admission::ObjectKey;
    use crate::code_admission::code_store::{Fixture, limits};
    use crate::code_store::{CodeStore, ObjectKind};
    use hirsel_core_ids::{ContentHash, DefPath};
    use hirsel_core_ir::Identity;
    use hirsel_core_ir::release_recipe::{Backend, BodyRef, Launch, Recipe};

    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    let stores = Stores::open(state).unwrap();
    let code = CodeStore::new(state.join("code"));
    let put = |kind, bytes: &[u8]| code.put(kind, bytes).unwrap();
    let kept_group = put(ObjectKind::Group, b"listed group");
    let body = put(ObjectKind::Body, b"listed body");
    let annex = put(ObjectKind::Annex, b"listed annex");
    let reserved = put(ObjectKind::Body, b"reserved body");
    let garbage = put(ObjectKind::Body, b"unlisted body");
    let fixture = Fixture::leaf();
    let fresh_group = fixture.recipe.groups[0];
    let fresh_body = fixture.recipe.bodies[0].body;
    let fresh_annex = fixture.recipe.bodies[0].annex;
    fixture.publish_object(&code, ObjectKind::Body, fresh_body);
    let recipe = |groups, bodies| Recipe {
        entry: Identity::plain(DefPath::from_full("app@1/main/main").unwrap()),
        address: ContentHash([0; 32]),
        groups,
        bodies,
        foreign_modules: vec![],
        foreign_archives: vec![],
        toolchain: vec![],
        values: vec![],
        launch: Launch {
            backend: Backend::Cranelift,
            target: "x86_64-unknown-linux-gnu".into(),
            root_limit: 1 << 20,
            n_place: 1 << 16,
            exit_status: true,
            grants: vec![],
        },
    };
    let kept = put(
        ObjectKind::Recipe,
        &recipe(vec![kept_group], vec![BodyRef { body, annex }]).encode(),
    );
    let dropped = put(ObjectKind::Recipe, &recipe(vec![], vec![]).encode());
    let assemble = |recipe: ContentHash, image: &[u8]| {
        let record = crate::AssemblyRecord::new(recipe, ContentHash([0; 32]), image);
        stores.records().put_assembly(&record, image).unwrap();
        record
    };
    let live = assemble(kept.hash, b"kept image");
    let old = assemble(dropped.hash, b"dropped image");
    // Everything so far was published two days ago.
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
    for kind in std::fs::read_dir(state.join("code")).unwrap() {
        for object in std::fs::read_dir(kind.unwrap().path()).unwrap() {
            let file = std::fs::File::open(object.unwrap().path()).unwrap();
            file.set_modified(long_ago).unwrap();
        }
    }
    // An operator publishes a recipe now and admits it after the collection below: no
    // root names it yet, and it shares a body an old publication already stored.
    fixture.publish_object(&code, ObjectKind::Group, fresh_group);
    fixture.publish_object(&code, ObjectKind::Body, fresh_body);
    fixture.publish_object(&code, ObjectKind::Annex, fresh_annex);
    let published = put(ObjectKind::Recipe, &fixture.recipe.encode());

    let reserved_key = ObjectKey {
        kind: ObjectKind::Body,
        hash: reserved.hash,
    };
    let admission = super::Admission::open(state, stores.records(), limits(), 1 << 20).unwrap();
    let removed = admission
        .under_collection(|held| {
            stores.collect(&BTreeSet::from([live.identity.0]), &|key| {
                key == reserved_key || held(key)
            })
        })
        .unwrap();
    let held = |kind, hash| code.path(kind, hash).exists();
    for (kind, object) in [
        (ObjectKind::Recipe, kept),
        (ObjectKind::Group, kept_group),
        (ObjectKind::Body, body),
        (ObjectKind::Annex, annex),
        (ObjectKind::Body, reserved),
        (ObjectKind::Recipe, published),
        (ObjectKind::Group, fresh_group),
        (ObjectKind::Body, fresh_body),
        (ObjectKind::Annex, fresh_annex),
    ] {
        assert!(held(kind, object.hash), "{kind:?} removed");
    }
    assert!(!held(ObjectKind::Body, garbage.hash) && !held(ObjectKind::Recipe, dropped.hash));
    assert!(stores.records().assembly(live.identity).is_ok());
    assert!(stores.records().assembly(old.identity).is_err());
    assert_eq!(removed, 4);

    let launcher = Launcher::new(state, Leader, || Ok(stores)).unwrap();
    let mut rig = Rig { dir, launcher };
    rig.launcher.admit_with(admission).unwrap();
    rig.bind();
    let a = attempt(1);
    rig.ask(Request::Admit(
        group(),
        a,
        Input::Publication(published.hash.0.to_vec()),
        Approvals::default(),
        super::admit::fixture_deadline_ms(),
    ));
    let answered = rig.until(|events| {
        events
            .iter()
            .any(|e| matches!(e, Event::Admitted(..) | Event::AdmissionFailed(..)))
    });
    assert!(
        matches!(answered.as_slice(), [Event::Admitted(candidate, root, ..)]
            if *candidate == a && root == &published.hash.0),
        "fresh publication failed admission after collection: {answered:?}"
    );
    rig.ask(Request::Release(a));
}
