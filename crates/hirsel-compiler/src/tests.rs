//! The supervisor's laws, against workers that are shell scripts: each does one thing a
//! hostile compile makes a real worker do. The expected refusals are PACK-3's table.

use super::*;
use hirsel_mod::{Header, Image};
use std::os::unix::fs::PermissionsExt;

const FILES: &[(&str, &[u8])] = &[("main.hirsel", b"pub fn main() {}")];

fn request(target: Target) -> Request<'static> {
    Request {
        files: FILES,
        target,
        backend: Backend::Cranelift,
        policy: 0,
    }
}

fn caps() -> Caps {
    Caps {
        cpu_seconds: 5,
        memory_bytes: 64 << 20,
        wall_ms: 10_000,
        input_bytes: 4096,
        output_bytes: 4096,
    }
}

/// A service whose worker runs `script` with the worker's arguments (`${10}` is the output).
fn service(dir: &Path, script: &str, caps: Caps, workers: usize, queue: usize) -> Service {
    let worker = dir.join("worker");
    std::fs::write(&worker, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
    Service::new(worker, dir.join("scratch"), caps, workers, queue)
}

fn refusal(script: &str, caps: Caps) -> Refusal {
    let dir = tempfile::tempdir().unwrap();
    service(dir.path(), script, caps, 1, 0)
        .compile(&request(Target::X86_64Linux))
        .unwrap_err()
}

fn module(target: Target) -> Vec<u8> {
    Image {
        header: Header {
            target,
            backend: Backend::Cranelift,
            policy: 0,
            abi: hirsel_mod::abi(),
            compiler: ContentHash([2; 32]),
            coreir: ContentHash([3; 32]),
            text_align: 64,
            data_align: 64,
        },
        text: vec![0xc3],
        data: vec![],
        imports: vec![],
        text_fixups: vec![],
        data_fixups: vec![],
        entries: vec![],
        spans: vec![],
        cfi: vec![],
        sites: vec![],
        names: vec![],
    }
    .encode()
}

#[test]
fn a_worker_that_breaks_a_cap_ends_as_that_limit() {
    let spin = Caps {
        cpu_seconds: 1,
        ..caps()
    };
    assert_eq!(
        refusal("while :; do :; done", spin),
        Refusal::Limit(Limit::Cpu)
    );

    // The script's child holds the worker's standard error too: the whole group ends
    // at the deadline, long before the sleep would.
    let started = Instant::now();
    let brief = Caps {
        wall_ms: 200,
        ..caps()
    };
    assert_eq!(refusal("/bin/sleep 60", brief), Refusal::Limit(Limit::Wall));
    assert!(started.elapsed() < Duration::from_secs(20));

    assert_eq!(refusal("exit 71", caps()), Refusal::Limit(Limit::Memory));
    // One byte past the cap is the largest file the worker can write.
    assert_eq!(
        refusal("/usr/bin/head -c 4097 /dev/zero > \"${10}\"", caps()),
        Refusal::Limit(Limit::Output)
    );
    assert_eq!(
        refusal(
            "exec /usr/bin/head -c 1000000 /dev/zero > \"${10}\"",
            caps()
        ),
        Refusal::Limit(Limit::Output)
    );

    let dir = tempfile::tempdir().unwrap();
    let large = [("main.hirsel", &[b' '; 4096][..])];
    let never = Service::new(
        dir.path().join("absent"),
        dir.path().join("scratch"),
        caps(),
        1,
        0,
    );
    let over = Request {
        files: &large,
        ..request(Target::X86_64Linux)
    };
    assert_eq!(never.compile(&over), Err(Refusal::Limit(Limit::Input)));
}

#[test]
fn a_worker_s_end_is_classified_and_its_module_is_checked() {
    assert_eq!(
        refusal(
            "echo 'module: unsupported: the release reaches hirsel_core_exec_spawn' >&2; exit 2",
            caps()
        ),
        Refusal::Unsupported("the release reaches hirsel_core_exec_spawn".into())
    );
    assert_eq!(
        refusal("echo 'main.hirsel:1:1: no' >&2; exit 1", caps()),
        Refusal::Rejected("main.hirsel:1:1: no\n".into())
    );
    // The tool's other failures say nothing about the source, so nothing caches them.
    assert_eq!(
        refusal("echo 'hirsel: no runtime' >&2; exit 2", caps()),
        Refusal::Unavailable("hirsel: no runtime\n".into())
    );
    assert!(matches!(
        refusal("kill -SEGV $$", caps()),
        Refusal::Unavailable(_)
    ));
    assert!(matches!(
        refusal("echo junk > \"${10}\"", caps()),
        Refusal::Unavailable(_)
    ));
    assert!(matches!(refusal("exit 0", caps()), Refusal::Unavailable(_)));

    let dir = tempfile::tempdir().unwrap();
    let bytes = module(Target::X86_64Linux);
    std::fs::write(dir.path().join("module"), &bytes).unwrap();
    let copy = format!("/bin/cp {}/module \"${{10}}\"", dir.path().display());
    let service = service(dir.path(), &copy, caps(), 1, 0);
    let built = service.compile(&request(Target::X86_64Linux)).unwrap();
    assert_eq!(built.bytes, bytes);
    assert_eq!(built.digest, hirsel_mod::digest(&bytes));
    assert_eq!(built.key, Module::parse(&bytes).unwrap().header.key());
    // The same reply to a request for another target is not that request's module.
    let other = service.compile(&request(Target::Aarch64Linux));
    assert!(matches!(other, Err(Refusal::Unavailable(_))));
    // A source path cannot leave the job's package directory.
    for name in ["../escape", "/escape", "a/../../escape", ""] {
        let files = [(name, &b"x"[..])];
        let leaving = Request {
            files: &files,
            ..request(Target::X86_64Linux)
        };
        assert!(
            matches!(service.compile(&leaving), Err(Refusal::Rejected(_))),
            "{name}"
        );
    }
    // Every job's directory is gone once it is answered.
    let left: Vec<_> = std::fs::read_dir(dir.path().join("scratch"))
        .unwrap()
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_full_queue_answers_busy_and_a_queued_request_runs_later() {
    let dir = tempfile::tempdir().unwrap();
    let mark = dir.path().join("started");
    let script = format!(": >> {}; /bin/sleep 1; exit 71", mark.display());
    for (queue, second) in [(0, Refusal::Busy), (1, Refusal::Limit(Limit::Memory))] {
        let _ = std::fs::remove_file(&mark);
        let service = service(dir.path(), &script, caps(), 1, queue);
        std::thread::scope(|scope| {
            let first = scope.spawn(|| service.compile(&request(Target::X86_64Linux)));
            while !mark.exists() {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(service.compile(&request(Target::X86_64Linux)), Err(second));
            assert_eq!(first.join().unwrap(), Err(Refusal::Limit(Limit::Memory)));
        });
    }
}
