//! A reader pin read by a client that has never seen the store (STORE-8): `store.at_pin`
//! in a fresh process makes a bounded number of provider requests, however many holders
//! the readers table has and however many groups the store holds after the pinned one.
//! The bound is the design's: the newest record's locator from `last`, that record, at
//! most `FOLD_DELTAS` ReaderDelta records along the strictly backward retention links and
//! the Retain fold they lead to, then the replay of the pinned version from its base. A
//! lookup that walked every record, or every holder's record, needs a request per
//! acquire, past the bound from 64 holders on.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `first` commits one key and acquires the pin it prints; `more K` acquires K more
/// pins; `unrelated M` makes M commits that touch no pin; `read HEX` reads the printed
/// pin and nothing else.
const PROGRAM: &str = r#"pub type AppError = Grant(GrantError) | Store(store.StoreError) | Output(OutputError) | Usage

fn put_x(mut tx: store.Tx, info: store.CallInfo, n: u64) -> Result[(), store.TxError] {
  store.tx_put(mut tx, b"x", bytes.from_list([int.truncate(n)]))
}

fn acquire(mut s: store.Store, r: Random) -> Result[store.HeldPin, AppError] {
  let Ok(p) = store.new_reader_pin(r) else {
    return Error(Usage())
  }
  let held: Result[store.HeldPin, store.TxFailure[store.PinError]] = store.transact(mut s, store.acquire_reader_pin, p)
  let Ok(h) = held else {
    return Error(Usage())
  }
  Ok(h)
}

pub fn main(rt: Io, args: Args) -> Result[(), AppError] {
  let output = io.stdout(rt) ? Grant
  let a = io.authority(rt, "main") ? Grant
  let Ok(mode) = arg.text(args, 0) else {
    return Error(Usage())
  }
  let Ok(word) = arg.text(args, 1) else {
    return Error(Usage())
  }
  if mode == "read" {
    let Ok(b) = std@1/hex.decode(word) else {
      return Error(Usage())
    }
    let decoded: Result[store.HeldPin, DecodeError] = codec.decode(b)
    let Ok(held) = decoded else {
      return Error(Usage())
    }
    let snap = store.at_pin(a, held) ? Store
    let Some(v) = store.get(snap, b"x") else {
      return Error(Usage())
    }
    out.print(output, u64.to_string(int.widen(v[0]))) ? Output
    out.print(output, "\n") ? Output
    return Ok(())
  }
  let Ok(n) = u64.parse(word) else {
    return Error(Usage())
  }
  let r = io.random(rt) ? Grant
  let mut s = store.open(a, store.default_config()) ? Store
  if mode == "first" {
    let put: Result[(), store.TxFailure[store.TxError]] = store.transact(mut s, put_x, n)
    if put != Ok(()) {
      return Error(Usage())
    }
    let h = acquire(mut s, r)?
    out.print(output, std@1/hex.encode(codec.encode(h))) ? Output
    out.print(output, "\n") ? Output
  } else if mode == "more" {
    for _ in 0u64..n {
      let _ = acquire(mut s, r)?
    }
  } else if mode == "unrelated" {
    for i in 0u64..n {
      let put: Result[(), store.TxFailure[store.TxError]] = store.transact(mut s, put_x, i + 100)
      if put != Ok(()) {
        return Error(Usage())
      }
    }
  } else {
    return Error(Usage())
  }
  store.close(take s) ? Store
  Ok(())
}
"#;

/// `FOLD_DELTAS` in std@1/store.
const FOLD_DELTAS: usize = 32;

/// `last`; the newest record; at most `FOLD_DELTAS` deltas and their fold, the newest
/// record among them when it is one; and the replay of a version in the store's first
/// records from Genesis, one batch.
const BOUND: usize = 1 + 1 + FOLD_DELTAS + 1 + 1;

struct Fixture(PathBuf);
impl Fixture {
    fn new(name: &str, authority: serde_json::Value) -> Self {
        let root = tempfile::Builder::new()
            .prefix(&format!("hirsel-cold-pin-{name}-"))
            .tempdir()
            .unwrap()
            .keep();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("main.hir"), PROGRAM).unwrap();
        let stdlib = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../stdlib/std@1/hirsel.json")
            .canonicalize()
            .unwrap();
        std::fs::write(
            root.join("main.hir.json"),
            serde_json::json!({"schema":1,"package":{"name":"pinned","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[{"manifest":stdlib,"package":{"name":"std","version":"1"},"expose":[]}],"entry":{"module":[],"name":"main"}}).to_string(),
        )
        .unwrap();
        let authority = serde_json::from_str::<serde_json::Value>(
            &authority
                .to_string()
                .replace("$DATA", &root.join("data").to_string_lossy()),
        )
        .unwrap();
        std::fs::write(
            root.join("grants.json"),
            serde_json::json!({"io":{"stdout":"stdout.bytes@1","random":"random.system@1","dirs":[{"path":"data","host":root.join("data"),"access":"read_write"}],"authorities":{"main":authority}}}).to_string(),
        )
        .unwrap();
        Self(root)
    }

    /// One fresh process, recorded to `tape`.
    fn run(&self, args: &[&str], tape: &Path) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_hirsel"))
            .arg("run")
            .arg(self.0.join("main.hir.json"))
            .arg("--grants")
            .arg(self.0.join("grants.json"))
            .arg("--record")
            .arg(tape)
            .arg("--")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        output
    }

    /// The provider requests a fresh process reading `pin` makes, by operation.
    fn cold_read(&self, pin: &str) -> Vec<String> {
        let tape = self.0.join("read.tape");
        let output = self.run(&["read", pin], &tape);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "1\n",
            "the pinned value"
        );
        let recording = hirsel_rt::tape_binary::parse(&std::fs::read(&tape).unwrap()).unwrap();
        recording
            .events
            .iter()
            .filter_map(|e| match &e.request {
                hirsel_rt::io::Request::Seam(hirsel_rt::authority::SeamRequest::Provider {
                    request,
                    ..
                }) => Some(
                    format!("{request:?}")
                        .split([' ', '{', '('])
                        .next()
                        .unwrap()
                        .to_string(),
                ),
                _ => None,
            })
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Reads the first pin cold at 1, 64 and 1023 holders, and at 1023 after 1 and then 40
/// more unrelated commits: within the bound each time, and the 39 later unrelated
/// commits cost nothing.
fn cold_reads_are_bounded(name: &str, authority: serde_json::Value) {
    let f = Fixture::new(name, authority);
    let scratch = f.0.join("write.tape");
    let printed = f.run(&["first", "1"], &scratch);
    let pin = String::from_utf8_lossy(&printed.stdout).trim().to_string();
    let mut seen = vec![];
    for (more, unrelated) in [(0, 0), (63, 0), (959, 0), (0, 1), (0, 40)] {
        if more > 0 {
            f.run(&["more", &more.to_string()], &scratch);
        }
        if unrelated > 0 {
            f.run(&["unrelated", &unrelated.to_string()], &scratch);
        }
        let reads = f.cold_read(&pin);
        assert!(
            reads.len() <= BOUND,
            "{name}: {} requests after {more} more pins and {unrelated} unrelated commits, past {BOUND}: {reads:?}",
            reads.len()
        );
        seen.push(reads);
    }
    assert_eq!(
        seen[3], seen[4],
        "{name}: unrelated commits after the newest pin changed the cold read"
    );
}

#[test]
fn a_cold_pin_read_on_the_log_is_bounded_however_many_holders() {
    cold_reads_are_bounded(
        "log",
        serde_json::json!({"provider":"authority.log@1","floor":"host","mount":"data","path":"store"}),
    );
}

#[test]
fn a_cold_pin_read_on_objects_is_bounded_however_many_holders() {
    cold_reads_are_bounded(
        "objects",
        serde_json::json!({"provider":"authority.objects@1","floor":"disk","level":"disk","objects":{"provider":"objects.dir@1","path":"$DATA/objects"}}),
    );
}
