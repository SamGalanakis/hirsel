use super::PackageLaw;
use crate::launcher::{
    dev::{self, Open, StoreId},
    session::{HostReply, InstallRefusal},
};
use serde_json::Value;
use std::{
    fs::{self, File},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, symlink},
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus},
    time::{Duration, Instant},
};

const ALLOWANCE: u64 = 8 << 20;
const FIXTURE: &str = include_str!("../../testdata/dev_place.hirsel");

struct Fixture {
    law: PackageLaw,
    compiler: PathBuf,
    runtime: PathBuf,
    allowance: String,
    cc: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let law = PackageLaw::new();
        let options = super::native_options(super::Backend::Cranelift, law.dir.path());
        let testdata = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/testdata");
        // The //:dev-fixture-cache action lays out this workspace with the same
        // script and builds it as the dev child does, so the child's first build
        // finds its objects in the seeded content store instead of compiling
        // core, std, runner and store inside the startup wait.
        let laid = Command::new("/bin/sh")
            .arg(testdata.join("dev_fixture.sh"))
            .arg(&law.libraries)
            .arg(&testdata)
            .arg(law.dir.path())
            .output()
            .unwrap();
        assert!(
            laid.status.success(),
            "{}",
            String::from_utf8_lossy(&laid.stderr)
        );
        let seed = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(std::env::var_os("HIRSEL_DEV_TEST_CACHE").unwrap());
        let copy = match crate::host::HOST_TARGET.os() {
            hirsel_core_layout::Os::Linux => "cp -R --reflink=auto",
            hirsel_core_layout::Os::Macos | hirsel_core_layout::Os::Ios => "cp -R",
        };
        let seeded = Command::new("/bin/sh")
            .args([
                "-c",
                &format!("mkdir -p \"$2\" && {copy} \"$1\"/. \"$2\" && chmod -R u+w \"$2\""),
                "seed",
            ])
            .arg(&seed)
            .arg(law.path("cache"))
            .output()
            .unwrap();
        assert!(
            seeded.status.success(),
            "{}",
            String::from_utf8_lossy(&seeded.stderr)
        );
        fs::create_dir(law.path("run")).unwrap();
        let super::Link::Driver(cc) = options.link else {
            unreachable!()
        };
        let fixture = Self {
            law,
            compiler: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(std::env::var_os("HIRSEL_DEV_TEST_DRIVER").unwrap())
                .canonicalize()
                .unwrap(),
            runtime: options.runtime.canonicalize().unwrap(),
            allowance: ALLOWANCE.to_string(),
            cc,
        };
        fixture.release(0);
        fixture
    }

    fn release(&self, revision: u64) {
        self.law.write(
            "runner/dev_place.hirsel",
            FIXTURE.replace(
                "DEV_FIXTURE_RELEASE: u64 = 0u64",
                &format!("DEV_FIXTURE_RELEASE: u64 = {revision}u64"),
            ),
        );
    }

    fn dev(&self, label: &str, args: &[&str]) -> Dev {
        self.launch(label, args, None)
    }

    fn fault(&self, label: &str, mode: &str) -> (Dev, PathBuf) {
        let marker = self.law.path(&format!("{label}.cut"));
        (self.launch(label, &[], Some((mode, &marker))), marker)
    }

    fn prepare_faults(&self) {
        self.law.write(
            "fault.c",
            include_str!("../../testdata/dev_install_fault.c"),
        );
        let output = Command::new(&self.cc)
            .args(["-shared", "-fPIC", "-o"])
            .arg(self.law.path("fault.so"))
            .arg(self.law.path("fault.c"))
            .arg("-ldl")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn launch(&self, label: &str, args: &[&str], fault: Option<(&str, &Path)>) -> Dev {
        let output = self.law.path(&format!("{label}.stdout"));
        let error = self.law.path(&format!("{label}.stderr"));
        let mut command = Command::new(&self.compiler);
        command
            .args([
                "dev",
                "runner",
                "--listen",
                "127.0.0.1:0",
                "--n-place",
                &self.allowance,
                "--backend",
                "cranelift",
                "--source-core",
                "--source-std",
                "--runtime",
            ])
            .arg(&self.runtime)
            .arg("--libraries")
            .arg(self.law.path("libraries"))
            .args(args)
            .current_dir(self.law.dir.path())
            .env("XDG_RUNTIME_DIR", self.law.path("run"))
            .env("XDG_CACHE_HOME", self.law.path("cache"))
            .stdout(File::create(&output).unwrap())
            .stderr(File::create(&error).unwrap());
        if let Some((mode, marker)) = fault {
            let preload = match crate::host::HOST_TARGET.object_format() {
                hirsel_core_layout::ObjectFormat::Elf => "LD_PRELOAD",
                hirsel_core_layout::ObjectFormat::MachO => "DYLD_INSERT_LIBRARIES",
            };
            command
                .env(preload, self.law.path("fault.so"))
                .env("HIRSEL_TEST_DEV_FAULT", mode)
                .env("HIRSEL_TEST_DEV_MARKER", marker);
        }
        Dev {
            child: command.spawn().unwrap(),
            output,
            error,
        }
    }

    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.law.path("runner/.hirsel-dev/state.json")).unwrap())
            .unwrap()
    }

    fn executable(&self, reference: &Value) -> PathBuf {
        let generation = reference["number"].as_u64().unwrap();
        self.law
            .path(&format!("runner/.hirsel-dev/gen/{generation}/place"))
    }

    fn store(&self) -> PathBuf {
        self.law.path("runner/.hirsel-dev/stores/1")
    }

    fn wait_unlocked(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let lock = File::open(self.store().join("LOCK")).unwrap();
        loop {
            // SAFETY: the descriptor belongs to the fixture and is held only for this check.
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "parent death did not release LOCK"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn probe(
        &self,
        executable: &Path,
        expected: Option<StoreId>,
        version: u64,
        value: u64,
    ) -> StoreId {
        let store = File::open(self.store()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let scratch = self.law.path("runner/.hirsel-dev/scratch");
        let Open::Ready(child, proof) = dev::open(
            dev::command(executable, ALLOWANCE, expected, None, &[]),
            self.law.dir.path(),
            &listener,
            &scratch,
            store.as_raw_fd(),
            Some(version),
            || false,
        )
        .unwrap() else {
            panic!("plain reopen failed for {}", executable.display());
        };
        assert_eq!(proof.version, version);
        let get = |path| {
            let mut socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            write!(
                socket,
                "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let mut answer = String::new();
            socket.read_to_string(&mut answer).unwrap();
            assert!(answer.starts_with("HTTP/1.1 200 "), "{path}: {answer}");
            answer.split_once("\r\n\r\n").unwrap().1.to_owned()
        };
        let status: Value = serde_json::from_str(&get("/hirsel/status")).unwrap();
        assert_eq!(status["phase"], "ready");
        assert_eq!(status["n_place"], ALLOWANCE);
        drop(child);
        let read = Command::new(executable)
            .arg("read-fixture")
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&read.stderr).contains(&format!("fixture value {value}\n")),
            "{}",
            String::from_utf8_lossy(&read.stderr)
        );
        (proof.store_hi, proof.store_lo)
    }

    fn install(&self, image: &Path, id: StoreId, predecessor: &Value) -> Option<HostReply> {
        let store = File::open(self.store()).unwrap();
        let scratch = self.law.path("runner/.hirsel-dev/scratch");
        let digest = serde_json::from_value(predecessor["identity"].clone()).unwrap();
        let command = dev::command(image, ALLOWANCE, Some(id), Some(digest), &[]);
        dev::install(
            command,
            self.law.dir.path(),
            &scratch,
            store.as_raw_fd(),
            || false,
        )
        .unwrap()
    }

    fn vectors(&self) {
        let row = PackageLaw::pass(self.law.run(&[
            "test",
            "runner",
            "--source-core",
            "--source-std",
            "--backend",
            "cranelift",
            "--exact",
            "runner@1/runner/dev_identity_vectors",
            "--json",
            "--rerun",
        ]));
        let report: Value = serde_json::from_str(&row).unwrap();
        let vectors = report["tests"][0]["stdout"].as_str().unwrap();
        let digests: Vec<_> = vectors
            .lines()
            .map(|line| {
                let (description, expected) = line.split_once(' ').unwrap();
                let raw = hirsel_core_ids::hex::decode(description).unwrap();
                let (_, digest) = crate::sdk::Describe::read_with_identity(&raw).unwrap();
                assert_eq!(hirsel_core_ids::hex::encode(&digest), expected);
                digest
            })
            .collect();
        assert_eq!(digests.len(), 5);
        for digest in &digests[1..4] {
            assert_ne!(*digest, digests[0]);
        }
        assert_eq!(digests[0], digests[4]);
    }
}

struct Dev {
    child: Child,
    output: PathBuf,
    error: PathBuf,
}

impl Dev {
    fn log(&self) -> String {
        fs::read_to_string(&self.error).unwrap()
    }

    fn wait_for(&mut self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            if self.log().contains(needle)
                || fs::read_to_string(&self.output).unwrap().contains(needle)
            {
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "waiting for {needle:?}\n{}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    fn place_pid(&self) -> u32 {
        let pid = self.child.id();
        let children = crate::host::child_pids(pid as libc::pid_t).unwrap();
        let [child] = children[..] else {
            panic!("one serving child, not {children:?}")
        };
        child as u32
    }

    fn interrupt_at(mut self, marker: &Path) {
        let deadline = Instant::now() + Duration::from_secs(300);
        while !marker.exists() {
            assert!(
                self.child.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "waiting for {}: {}",
                marker.display(),
                self.log()
            );
            std::thread::sleep(Duration::from_millis(30));
        }
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }

    fn stop(mut self) -> String {
        // SAFETY: the unreaped subprocess is owned by this test.
        assert_eq!(
            unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) },
            0
        );
        assert!(self.child.wait().unwrap().success(), "{}", self.log());
        self.log()
    }

    fn status(&mut self) -> ExitStatus {
        self.child.wait().unwrap()
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn logged_version(log: &str) -> u64 {
    log.lines()
        .find(|line| line.starts_with("hirsel dev: serving generation"))
        .unwrap()
        .split_once(", activation ")
        .unwrap()
        .1
        .split_once(',')
        .unwrap()
        .0
        .parse()
        .unwrap()
}

#[test]
fn package_dev_s1_persistence_and_fencing() {
    let fixture = Fixture::new();
    fixture.prepare_faults();
    let (before_state, cut) = fixture.fault("before-state", "hold-state");
    before_state.interrupt_at(&cut);
    fixture.wait_unlocked();
    assert!(!fixture.law.path("runner/.hirsel-dev/state.json").exists());

    let mut old = fixture.dev("old", &[]);
    old.wait_for("hirsel dev listening on ");
    let old_ref = fixture.state()[2]["active"].clone();
    let old_image = fixture.executable(&old_ref);
    let first = old.stop();
    assert!(first.contains("activation 1,"));
    let seeded = Command::new(&old_image)
        .arg("seed-fixture")
        .output()
        .unwrap();
    assert!(
        seeded.status.success(),
        "{}",
        String::from_utf8_lossy(&seeded.stderr)
    );
    let id = fixture.probe(&old_image, None, 1, 271828);

    let mut live = fixture.dev("live", &[]);
    live.wait_for("hirsel dev listening on ");
    let live_pid = live.place_pid();
    fixture.law.write(
        "runner/dev_place.hirsel",
        format!("{FIXTURE}\n// same program\n"),
    );
    live.wait_for("hirsel dev: unchanged");
    assert_eq!(live.place_pid(), live_pid);
    fixture.release(1);
    live.wait_for("OLD reopened");
    assert!(live.log().contains("migration refused: Migration"));
    let live_ref = fixture.state()[2]["active"].clone();
    assert_eq!(old_ref["identity"], live_ref["identity"]);
    live.stop();
    assert_eq!(fixture.probe(&old_image, Some(id), 1, 271828), id);

    fixture.release(2);
    let (before_cut, cut) = fixture.fault("before-cut", "hold-before");
    before_cut.interrupt_at(&cut);
    fixture.wait_unlocked();
    let mut recovered = fixture.dev("before-recovered", &[]);
    recovered.wait_for("OLD reopened");
    assert!(!recovered.stop().contains("installed activation"));
    assert_eq!(fixture.probe(&old_image, Some(id), 1, 271828), id);

    let (mut lost_before, cut) = fixture.fault("lost-before", "exit-before");
    lost_before.wait_for("OLD reopened");
    assert!(cut.exists());
    assert!(!lost_before.stop().contains("installed activation"));
    assert_eq!(fixture.probe(&old_image, Some(id), 1, 271828), id);

    let mut changed = fixture.dev("changed", &[]);
    changed.wait_for("hirsel dev listening on ");
    let new_ref = fixture.state()[2]["active"].clone();
    let new_image = fixture.executable(&new_ref);
    assert_ne!(old_ref["identity"], new_ref["identity"]);
    let changed = changed.stop();
    let version: u64 = changed
        .split_once("installed activation ")
        .unwrap()
        .1
        .lines()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(version > 1);
    assert!(changed.contains(&format!("activation {version},")));

    let mut same = fixture.dev("same", &[]);
    same.wait_for("hirsel dev listening on ");
    let same = same.stop();
    assert!(same.contains(&format!("activation {version},")));
    assert!(!same.contains("installed activation"));
    assert_eq!(fixture.probe(&new_image, Some(id), version, 271830), id);

    let mut backend = fixture.dev("backend", &["--backend", "llvm"]);
    backend.wait_for("hirsel dev listening on ");
    let backend_ref = fixture.state()[2]["active"].clone();
    let backend_image = fixture.executable(&backend_ref);
    assert_eq!(backend_ref["identity"], new_ref["identity"]);
    assert_ne!(
        fs::read(backend_image).unwrap(),
        fs::read(&new_image).unwrap()
    );
    let backend = backend.stop();
    assert_eq!(logged_version(&backend), version);
    assert!(!backend.contains("installed activation"));

    // A non-dev writer can install after a dev supervisor stops its predecessor.
    let old_version = match fixture.install(&old_image, id, &new_ref) {
        Some(HostReply::Installed(version, false)) => version,
        reply => panic!("outside writer did not install: {reply:?}"),
    };
    let binding: Vec<u8> = serde_json::from_value(old_ref["identity"].clone()).unwrap();
    assert_eq!(
        fixture.install(&new_image, id, &new_ref),
        Some(HostReply::InstallRefused(InstallRefusal::Predecessor(
            binding
        )))
    );
    assert_eq!(fixture.probe(&old_image, Some(id), old_version, 271828), id);

    let lock = File::open(fixture.store().join("LOCK")).unwrap();
    // SAFETY: acquire a writer lock on the test's own store for the held case.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let mut held = fixture.dev("held", &[]);
    assert_eq!(held.status().code(), Some(2));
    assert!(held.log().contains("Held"));
    drop(lock);

    let (after_cut, cut) = fixture.fault("after-cut", "hold-after");
    after_cut.interrupt_at(&cut);
    fixture.wait_unlocked();
    let pending = fixture.state()[2]["pending"].clone();
    assert!(pending["installed"].is_null());
    let mut recovered = fixture.dev("after-recovered", &[]);
    recovered.wait_for("hirsel dev listening on ");
    let after_ref = fixture.state()[2]["active"].clone();
    assert_eq!(after_ref["identity"], new_ref["identity"]);
    let after = recovered.stop();
    assert!(!after.contains("installed activation"));
    let after_version = logged_version(&after);
    assert_eq!(
        fixture.probe(
            &fixture.executable(&after_ref),
            Some(id),
            after_version,
            271830
        ),
        id
    );

    fixture.release(3);
    let (mut lost_after, cut) = fixture.fault("lost-after", "exit-after");
    lost_after.wait_for("hirsel dev listening on ");
    assert!(cut.exists());
    let lost_ref = fixture.state()[2]["active"].clone();
    assert_ne!(lost_ref["identity"], after_ref["identity"]);
    let lost = lost_after.stop();
    assert!(!lost.contains("installed activation"));
    assert_eq!(
        fixture.probe(
            &fixture.executable(&lost_ref),
            Some(id),
            logged_version(&lost),
            271831
        ),
        id
    );

    fixture.vectors();
}

#[test]
fn package_dev_s1_store_ownership() {
    let fixture = Fixture::new();
    let supplied = fixture.law.path("supplied");
    fs::create_dir(&supplied).unwrap();
    fs::write(supplied.join("original"), b"user file").unwrap();
    let supplied = supplied.to_str().unwrap();
    let mut rejected = fixture.dev("fresh-supplied", &["--fresh", "--store", supplied]);
    assert_eq!(rejected.status().code(), Some(2));
    assert!(rejected.log().contains("cannot be combined"));
    let mut external = fixture.dev("supplied", &["--store", supplied]);
    external.wait_for("hirsel dev listening on ");
    let alias = fixture.law.path("other-server");
    fs::create_dir(&alias).unwrap();
    let runtime = fixture.law.path("other-runtime");
    fs::create_dir(&runtime).unwrap();
    let conflict = Command::new(&fixture.compiler)
        .args(["dev", alias.to_str().unwrap(), "--store", supplied])
        .env("XDG_RUNTIME_DIR", runtime)
        .output()
        .unwrap();
    assert_eq!(conflict.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("already supervising store"));
    external.stop();

    let mut original = fixture.dev("original", &[]);
    original.wait_for("hirsel dev listening on ");
    original.stop();
    let store = fixture.store();
    let lock = File::open(store.join("LOCK")).unwrap();
    let inode = lock.metadata().unwrap().ino();
    // SAFETY: this descriptor points to this test's store lock.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let mut fresh = fixture.dev("fresh", &["--fresh"]);
    fresh.wait_for("hirsel dev listening on ");
    fresh.stop();
    let mut second_fresh = fixture.dev("fresh-again", &["--fresh"]);
    second_fresh.wait_for("hirsel dev listening on ");
    second_fresh.stop();

    let stores = store.parent().unwrap();
    let new_store = stores.join("2");
    fs::rename(&new_store, stores.join("4")).unwrap();
    fs::rename(stores.join("3"), stores.join("5")).unwrap();
    fixture.law.write(
        "runner/.hirsel-dev/stores/4/keep.hirsel",
        "fn keep() -> u64 { 45u64 }\n",
    );
    // Source paths cannot name `.hirsel-dev`, so the overlap arrives through a link.
    let manifest = fs::read_to_string(fixture.law.path("runner/hirsel.toml")).unwrap();
    fixture.law.write(
        "runner/hirsel.toml",
        manifest.replace("[sources]\n", "[sources]\n\"keep.hirsel\" = \"keep\"\n"),
    );
    symlink(
        ".hirsel-dev/stores/4/keep.hirsel",
        fixture.law.path("runner/keep.hirsel"),
    )
    .unwrap();
    // Keep the marker and its inode, but put two ineligible generations below it.
    symlink(supplied, &new_store).unwrap();
    fs::create_dir(stores.join("3")).unwrap();
    fs::write(stores.join("3/keep"), b"unowned").unwrap();
    fs::write(stores.join("current"), b"5\n").unwrap();
    let mut later = fixture.dev("later", &[]);
    later.wait_for("hirsel dev listening on ");
    later.stop();

    assert_eq!(fs::metadata(store.join("LOCK")).unwrap().ino(), inode);
    assert!(fs::symlink_metadata(new_store).unwrap().is_symlink());
    assert_eq!(fs::read(stores.join("3/keep")).unwrap(), b"unowned");
    assert!(stores.join("4/keep.hirsel").is_file());
    assert_eq!(
        fs::read(Path::new(supplied).join("original")).unwrap(),
        b"user file"
    );
}

// RUNNER-SPEC §14.6 "Who supplies a fate": `hirsel dev` passes `Release` on every
// install it runs into its store. Release leaves a held row `Waiting` with NEW's
// identity digest as its stamp, `cancel` false, and its attempt and due time
// unchanged; NEW's head then runs attempt n + 1 on NEW's code, once.
#[test]
fn package_dev_releases_held_runs() {
    let fixture = Fixture::new();
    fixture.prepare_faults();
    let mut old = fixture.dev("old", &[]);
    old.wait_for("hirsel dev listening on ");
    let old_image = fixture.executable(&fixture.state()[2]["active"]);
    old.stop();
    let seeded = Command::new(&old_image).arg("seed-held").output().unwrap();
    assert!(
        seeded.status.success(),
        "{}",
        String::from_utf8_lossy(&seeded.stderr)
    );

    // A hot redeploy: the loop serves OLD, then the source changes under it.
    let (mut live, cut) = fixture.fault("held-live", "hold-after");
    live.wait_for("hirsel dev listening on ");
    fixture.release(2);
    let deadline = Instant::now() + Duration::from_secs(300);
    while !cut.exists() {
        let log = live.log();
        assert!(
            !log.contains("migration refused") && Instant::now() < deadline,
            "the held run's install did not commit:\n{log}"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    live.interrupt_at(&cut);
    fixture.wait_unlocked();

    // The install committed and NEW's head has not started: read the settled row.
    let new_ref = fixture.state()[2]["pending"]["new"].clone();
    let new_image = fixture.executable(&new_ref);
    let identity: Vec<u8> = serde_json::from_value(new_ref["identity"].clone()).unwrap();
    let identity = hirsel_core_ids::hex::encode(&identity);
    let held = |image: &Path| {
        let read = Command::new(image)
            .args(["read-held", &identity])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&read.stderr).into_owned();
        assert!(read.status.success(), "{stderr}");
        stderr
            .lines()
            .find_map(|line| line.strip_prefix("held "))
            .unwrap_or_else(|| panic!("no held line: {stderr}"))
            .to_owned()
    };
    assert_eq!(
        held(&new_image),
        "waiting=true parked=false cancel=false stamped=true attempt=1 due=7"
    );

    // Dev recovers through a plain reopen of NEW, which serves, and installs nothing again.
    let mut recovered = fixture.dev("held-recovered", &[]);
    recovered.wait_for("hirsel dev listening on ");
    assert_eq!(
        fixture.state()[2]["active"]["identity"],
        new_ref["identity"]
    );
    assert!(!recovered.stop().contains("installed activation"));
}

// packages.md `[program]`: a package that declares its run configuration is the
// build's only source of `--limit`, `--dir` and `--grant`, and dev's build is one
// such build. Its first `dirs` path is the Dir slot dev's launch fills with the
// store, so the place opens the store and serves through dev's listener.
#[test]
fn package_dev_takes_grants_from_program() {
    let fixture = Fixture::new();
    let manifest = fs::read_to_string(fixture.law.path("runner/hirsel.toml")).unwrap();
    fixture.law.write(
        "runner/hirsel.toml",
        manifest
            + "limit = 1610612736\n\
               grants = [\"Clock\", \"Random\", \"Tasks\", \"Process\", \"Stderr\", \"Net=Listen 127.0.0.1/32 0-65535\"]\n\
               dirs = [\".\"]\n",
    );
    let mut dev = fixture.dev("program", &[]);
    dev.wait_for("hirsel dev listening on ");
    let stdout = fs::read_to_string(&dev.output).unwrap();
    let address = stdout
        .lines()
        .find_map(|line| line.strip_prefix("hirsel dev listening on "))
        .unwrap();
    let mut socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(
        socket,
        "GET /hirsel/status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut answer = String::new();
    socket.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 200 "), "{answer}");
    let status: Value = serde_json::from_str(answer.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(status["phase"], "ready");
    assert!(dev.stop().contains("activation 1,"));
}

#[test]
fn package_dev_wait_reports_failed_edit_and_serving_predecessor() {
    let fixture = Fixture::new();
    let mut dev = fixture.dev("dev-edit-receipts", &["--json"]);
    dev.wait_for("\"Activated\"");
    let cli = |args: &[&str]| {
        let answer = Command::new(&fixture.compiler)
            .args(args)
            .arg("runner")
            .arg("--json")
            .current_dir(fixture.law.dir.path())
            .env("XDG_RUNTIME_DIR", fixture.law.path("run"))
            .output()
            .unwrap();
        let json = serde_json::from_slice::<serde_json::Value>(&answer.stdout)
            .unwrap_or_else(|e| panic!("{e}: {answer:?}"));
        (answer.status.code(), json)
    };
    let (code, ready) = cli(&["status", "--context", "dev", "--ready"]);
    assert_eq!(code, Some(0), "{ready}");
    let active = &ready["result"]["active"];
    assert!(!active.is_null(), "{ready}");
    fixture
        .law
        .write("runner/dev_place.hirsel", "pub fn broken(");
    let (code, rejected) = cli(&["dev", "wait"]);
    assert_eq!(code, Some(1), "{rejected}");
    assert!(!rejected["result"]["attempted"]["edit"].is_null());
    assert_eq!(&rejected["result"]["active"], active);
    assert!(rejected["result"]["superseded"].is_null());
    assert_eq!(cli(&["status", "--context", "dev", "--ready"]).0, Some(0));
    assert_eq!(cli(&["dev", "stop"]).0, Some(0));
    assert!(dev.status().success(), "{}", dev.log());
    let output = fs::read_to_string(&dev.output).unwrap();
    let records = output
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.first().unwrap()["kind"], "snapshot");
    assert_eq!(records.last().unwrap()["kind"], "end");
    assert_eq!(records.iter().filter(|r| r["kind"] == "end").count(), 1);
}
