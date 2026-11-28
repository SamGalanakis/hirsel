//! G2: the package-layer spans of 50 warm `hirsel build --locked` requests.
//! Build this executable and the driver before timing. Set up outside the hold:
//!   package_layer prepare WORK HIRSEL RUNTIME CC
//! For the G3/G4 producer used by bench/wire/registry_fetch.py:
//!   package_layer registry-fixture WORK CORE_STD
//! Then measure the same fixture and compiler on a shielded CPU:
//!   HIRSEL_MEMORY_LIMIT=2G HIRSEL_JOBS=1 python3 core/tools/timing_shield.py run -- \
//!     taskset -c 16 tools/capped package_layer measure WORK HIRSEL RUNTIME CC

use serde_json::Value;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

hirsel_guard::install!();

const PACKAGES: usize = 30;
const SOURCE_BYTES: usize = 10_600_000;
const SAMPLES: usize = 50;
const STAGES: [&str; 4] = ["workspace", "resolution", "lock", "package inputs"];

struct Fixture {
    root: PathBuf,
    driver: PathBuf,
    runtime: PathBuf,
    cc: PathBuf,
}

fn succeeded(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).unwrap()
}

fn manifest(name: &str, deps: &str) -> String {
    format!(
        "pkg_v1.Package(name: {name:?}, version: \"1.0.0\", charge: ChargeV1, sources: [(\"main.hirsel\", \"main\")], dependencies: [{deps}])\n"
    )
}

fn producer(root: &Path, corpus: Option<&[u8]>) {
    fs::create_dir(root.join("producer")).unwrap();
    let names: Vec<_> = (0..PACKAGES)
        .map(|i| match (corpus, i) {
            (Some(_), 0..4) => ["root", "left", "right", "base"][i].into(),
            _ => format!("p{i:02}"),
        })
        .collect();
    fs::write(
        root.join("producer/hirsel.hcfg"),
        format!("pkg_v1.Workspace(members: {names:?})\n"),
    )
    .unwrap();
    for (i, name) in names.iter().enumerate() {
        let dir = root.join("producer").join(name);
        fs::create_dir(&dir).unwrap();
        let deps: Vec<_> = match (corpus, i) {
            (Some(_), 0) => names[1..].iter().filter(|n| *n != "base").collect(),
            (Some(_), 1 | 2) => vec![&names[3]],
            (None, 0) => names[1..].iter().collect(),
            _ => vec![],
        };
        let edges: Vec<_> = deps.iter().map(|dep| format!(
            r#"("{dep}", Path(package: "fixture.test/{dep}", major: 1, path: "../{dep}", authority: []))"#)).collect();
        let config = manifest(&format!("fixture.test/{name}"), &edges.join(", "));
        fs::write(dir.join("hirsel.hcfg"), config).unwrap();
        let mut source = format!("pub fn value() -> u64 {{ {i}u64 }}\n").into_bytes();
        for j in 0..32 {
            writeln!(source, "pub fn value_{j}(x: u64) -> u64 {{ x + {j}u64 }}").unwrap();
        }
        let size = SOURCE_BYTES / PACKAGES + usize::from(i < SOURCE_BYTES % PACKAGES);
        source.extend_from_slice(b"// ");
        if let Some(text) = corpus {
            // Use real standard-library text as comment payload with varied compression.
            let offset = i * (SOURCE_BYTES / PACKAGES);
            source.extend((offset..offset + size - 1 - source.len()).map(|j| text[j % text.len()]));
        } else {
            source.resize(size - 1, b'x');
        }
        source.push(b'\n');
        fs::write(dir.join("main.hirsel"), source).unwrap();
    }
}

fn registry_fixture(root: &Path, std: &Path) {
    let mut files: Vec<_> = fs::read_dir(std)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "hirsel"))
        .collect();
    files.sort();
    let mut text = Vec::new();
    for path in files {
        text.extend(
            fs::read(path)
                .unwrap()
                .into_iter()
                .map(|b| if b.is_ascii_graphic() { b } else { b' ' }),
        );
    }
    assert!(!text.is_empty());
    producer(root, Some(&text));
}

impl Fixture {
    fn hirsel(&self) -> Command {
        let mut command = Command::new(&self.driver);
        command
            .current_dir(&self.root)
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env_remove("PERF_CTL_FIFO");
        command
    }

    fn prepare(&self) {
        producer(&self.root, None);

        fs::create_dir(self.root.join("app")).unwrap();
        let packed = succeeded(
            self.hirsel()
                .args([
                    "pack",
                    "producer/p00",
                    "--output",
                    "app/libs.hpkg",
                    "--package-store",
                    "packages",
                ])
                .output()
                .unwrap(),
        );
        assert_eq!(packed.lines().count(), PACKAGES);
        let root = packed
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .last()
            .unwrap();
        fs::write(
            self.root.join("app/hirsel.hcfg"),
            manifest("local/bench", &format!(r#"("p00", Published(package: "fixture.test/p00", major: 1, root: {root:?}, locator: Archive("libs.hpkg"), authority: []))"#)),
        )
        .unwrap();
        fs::write(
            self.root.join("app/main.hirsel"),
            "pub fn main() -> Result[(), Exit] { let _ = p00.main.value(); Ok(()) }\n",
        )
        .unwrap();
        succeeded(
            self.hirsel()
                .args(["lock", "app", "--package-store", "packages"])
                .output()
                .unwrap(),
        );
        // A timed build must find every canonical content entry in the package store.
        fs::remove_file(self.root.join("app/libs.hpkg")).unwrap();
        self.build();
        let stored: u64 = fs::read_dir(self.root.join("packages"))
            .unwrap()
            .map(|shard| {
                fs::read_dir(shard.unwrap().path())
                    .unwrap()
                    .map(|entry| entry.unwrap().metadata().unwrap().len())
                    .sum::<u64>()
            })
            .sum();
        println!(
            "{PACKAGES} published packages, {SOURCE_BYTES} source bytes, {stored} canonical content bytes"
        );
    }

    fn build(&self) -> Value {
        let stdout = succeeded(
            self.hirsel()
                .args([
                    "build",
                    "app",
                    "--locked",
                    "--package-store",
                    "packages",
                    "--store",
                    "compiler-store",
                    "--code-store",
                    "code-store",
                    "--backend",
                    "cranelift",
                    "--limit",
                    "67108864",
                    "--output",
                    "program",
                    "--metrics",
                    "metrics.json",
                    "--json",
                    "--runtime",
                ])
                .arg(&self.runtime)
                .arg("--cc")
                .arg(&self.cc)
                .output()
                .unwrap(),
        );
        let answer: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(answer["accepted"], true, "{answer}");
        serde_json::from_slice(&fs::read(self.root.join("metrics.json")).unwrap()).unwrap()
    }

    fn measure(&self) {
        let lock = fs::read(self.root.join("app/hirsel.lock")).unwrap();
        assert_eq!(
            std::str::from_utf8(&lock)
                .unwrap()
                .matches("id = \"fixture.test/")
                .count(),
            PACKAGES,
        );
        let mut totals = Vec::with_capacity(SAMPLES);
        let mut stages = STAGES.map(|_| Vec::with_capacity(SAMPLES));
        for _ in 0..SAMPLES {
            let metrics = self.build();
            let work = metrics["package_layer"]
                .as_array()
                .expect("missing package timings");
            assert_eq!(work.len(), STAGES.len());
            let mut total = 0;
            for ((name, values), stage) in STAGES.iter().zip(&mut stages).zip(work) {
                assert_eq!(stage["name"], *name);
                let time = stage["nanoseconds"].as_u64().unwrap();
                total += time;
                values.push(time);
            }
            totals.push(total);
        }
        assert_eq!(fs::read(self.root.join("app/hirsel.lock")).unwrap(), lock);
        print_percentiles("total", &mut totals);
        for (name, values) in STAGES.iter().zip(&mut stages) {
            print_percentiles(name, values);
        }
    }
}

fn print_percentiles(label: &str, samples: &mut [u64]) {
    samples.sort_unstable();
    let percentile = |p: usize| samples[(p * samples.len()).div_ceil(100) - 1] as f64 / 1e6;
    let (p50, p95, p99) = (percentile(50), percentile(95), percentile(99));
    println!("{label:>14}  p50 {p50:9.6} ms  p95 {p95:9.6} ms  p99 {p99:9.6} ms");
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let usage = "usage: package_layer prepare|measure WORK HIRSEL RUNTIME CC";
    if let [action, root, std] = args.as_slice()
        && action == "registry-fixture"
    {
        registry_fixture(Path::new(root), Path::new(std));
        return;
    }
    if args.is_empty() {
        eprintln!("{usage}");
        return;
    }
    let [action, root, driver, runtime, cc] = args.as_slice() else {
        panic!("{usage}");
    };
    let absolute = |path| std::path::absolute(Path::new(path)).unwrap();
    let fixture = Fixture {
        root: absolute(root),
        driver: absolute(driver),
        runtime: absolute(runtime),
        cc: absolute(cc),
    };
    match action.to_str() {
        Some("prepare") => fixture.prepare(),
        Some("measure") => fixture.measure(),
        _ => panic!("expected prepare or measure"),
    }
}
