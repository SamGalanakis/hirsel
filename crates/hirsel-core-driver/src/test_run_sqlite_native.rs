fn sqlite_hot_journal(db: &[u8], super_name: Option<&[u8]>) -> Vec<u8> {
    const MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];
    // SQLite's rollback journal begins with a sector header, then one page
    // number, page image and checksum per record. Preserve page 1 verbatim.
    let page_size = u32::from(u16::from_be_bytes(db[16..18].try_into().unwrap()));
    assert!(page_size >= 512);
    let mut bytes = MAGIC.to_vec();
    for word in [1u32, 0, db.len() as u32 / page_size, 512, page_size] {
        bytes.extend_from_slice(&word.to_be_bytes());
    }
    bytes.resize(512, 0);
    bytes.extend_from_slice(&1u32.to_be_bytes());
    let page = &db[..page_size as usize];
    bytes.extend_from_slice(page);
    let checksum: u32 = (1..page.len()).rev()
        .skip(199).step_by(200).map(|i| u32::from(page[i])).sum();
    bytes.extend_from_slice(&checksum.to_be_bytes());
    if let Some(name) = super_name {
        bytes.extend_from_slice(&(0x40000000u32 / page_size + 1).to_be_bytes());
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(&(name.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&name.iter().map(|&b| u32::from(b)).sum::<u32>().to_be_bytes());
        bytes.extend_from_slice(&MAGIC);
    }
    bytes
}

#[test]
fn sqlite_directory_recovery_locking_and_sync() {
    use std::fs;
    use std::io::{BufRead, BufReader, Read};
    use std::os::fd::AsRawFd;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;

    crate::openat2_required::require();
    let law = PackageLaw::new();
    let fixture = law.libraries.join("programs/sqlite");
    let manifest = fs::read_to_string(fixture.join("hirsel.hcfg")).unwrap()
        .replace(r#"("native.hirsel", "native")"#, r#"("native.hirsel", "native"), ("boundary.hirsel", "boundary")"#);
    law.write("core/programs/sqlite/hirsel.hcfg", manifest);
    for file in ["sqlite.hirsel", "native.hirsel", "libhirsel_sqlite.a", "libhirsel_sqlite-aarch64-unknown-linux-gnu.a", "libhirsel_sqlite-aarch64-apple-darwin.a", "tests.hirsel", "error_buffer_test.hirsel", "example.hirsel"] {
        law.write(&format!("core/programs/sqlite/{file}"), fs::read(fixture.join(file)).unwrap());
    }
    law.write("core/programs/sqlite/boundary.hirsel", r#"
fn expect_code(take r: Result[(), sqlite.SqliteError], expected: i32) {
    match r {
        Error(e) => if e.code != expected { fail("unexpected SQLite error code") },
        Ok(()) => fail("expected a SQLite error"),
    }
}

fn read_row(mut db: sqlite.Db) waits {
    let mut statement = result.unwrap(sqlite.prepare(mut db, "select x from t"))
    match result.unwrap(sqlite.step(mut statement)) {
        Row => if sqlite.column_i64(mut statement, 0) != 19 { fail("database row changed") },
        _ => fail("database row is missing"),
    }
    result.unwrap(sqlite.finalize(take statement))
}

pub fn main(f: Foreign[sqlite.Sqlite], d: Dir, args_: Args, input: Stdin, output: Stdout) -> Result[(), Exit] waits {
    let mode = option.unwrap(args.get(args_, 0))
    if mode == "hard-link" {
        match sqlite.open(f, d, "a.db") {
            Error(e) => if e.code != 14 { fail("expected SQLITE_CANTOPEN") },
            Ok(db) => { result.unwrap(sqlite.close(take db)); fail("hard-linked database opened") },
        }
        return Ok(())
    }
    let dir = if mode == "read-only" { host.read_only(d) } else { d }
    let mut db = result.unwrap(sqlite.open(f, dir, "a.db"))
    if mode == "init" {
        result.unwrap(sqlite.exec(mut db, "create table t(x); insert into t values(19)"))
    } else if mode == "holder" || mode == "wal-holder" {
        if mode == "wal-holder" {
            result.unwrap(sqlite.exec(mut db, "create table wal_witness(x); insert into wal_witness values(1)"))
        }
        result.unwrap(sqlite.exec(mut db, "begin immediate"))
        result.unwrap(stdout.write(output, "locked\n"))
        let mut scratch: Bytes = list.with_capacity(1)
        let _ = result.unwrap(stdin.read(input, mut scratch, Never))
        result.unwrap(sqlite.exec(mut db, "rollback"))
    } else if mode == "compete" {
        expect_code(sqlite.exec(mut db, "begin immediate"), 5)
    } else if mode == "read-only" {
        read_row(mut db)
        expect_code(sqlite.exec(mut db, "insert into t values(23)"), 8)
    } else if mode == "hot-journal" {
        expect_code(sqlite.exec(mut db, "select x from t"), 14)
    } else if mode == "wal" {
        result.unwrap(sqlite.exec(mut db, "pragma journal_mode=wal"))
    } else if mode == "sync" {
        expect_code(sqlite.exec(mut db, "insert into t values(23)"), 1290)
    } else {
        read_row(mut db)
    }
    result.unwrap(sqlite.close(take db))
    Ok(())
}
"#);
    // Fail just the durability call on a directory, leaving file data sync intact.
    let (fault, preload) = match crate::host::HOST_TARGET.object_format() {
        hirsel_core_layout::ObjectFormat::Elf => (r#"
#define _GNU_SOURCE
#include <errno.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>
int fsync(int fd) {
    struct stat info;
    if (fstat(fd, &info) == 0 && S_ISDIR(info.st_mode)) {
        errno = EIO;
        return -1;
    }
    return syscall(SYS_fsync, fd);
}
"#, "LD_PRELOAD"),
        hirsel_core_layout::ObjectFormat::MachO => (include_str!("sqlite_sync_fault_darwin.c"), "DYLD_INSERT_LIBRARIES"),
    };
    law.write("sync-fault.c", fault);
    let options = native_options(Backend::Cranelift, law.dir.path());
    command(Command::new(options.link.driver()).current_dir(law.dir.path())
        .args(["-shared", "-fPIC", "sync-fault.c", "-o", "sync-fault.so"])).unwrap();
    let db = law.path("data/a.db");
    let journal = law.path("data/a.db-journal");
    let shm = law.path("data/a.db-shm");
    let alias = law.path("data/b.db");
    for backend in ["cranelift", "llvm"] {
        fs::create_dir(law.path("data")).unwrap();
        let exe = law.path(if backend == "llvm" { "sqlite-llvm" } else { "sqlite-cl" });
        PackageLaw::pass(law.run(&[
            "build", "core/programs/sqlite", "--entry", "hirsel.ai/sqlite@1/boundary/main",
            "--backend", backend, "--source-core", "--source-std", "--limit", "67108864",
            "--grant", "Foreign=hirsel.ai/sqlite@1/sqlite/Sqlite", "--grant", "Stdin", "--grant", "Stdout",
            "--dir", law.path("data").to_str().unwrap(), "-o", exe.to_str().unwrap(),
        ]));
        let child = |mode: &str| {
            let mut cmd = Command::new(&exe);
            cmd.arg(mode).current_dir(law.dir.path());
            cmd
        };
        let success = |out: std::process::Output| {
            assert!(out.status.success(), "{backend}: {}", String::from_utf8_lossy(&out.stderr));
        };
        let bounded = |cmd: &mut Command| {
            let (out, expired) = crate::test_run::capture(cmd, Some(std::time::Duration::from_secs(15))).unwrap();
            assert!(!expired, "{cmd:?} timed out");
            success(out);
        };
        let run = |mode| bounded(&mut child(mode));
        run("init");
        let baseline = fs::read(&db).unwrap();
        let start_holder = |mode: &str| {
            let mut holder = Command::new(&exe)
                .arg(mode)
                .current_dir(law.dir.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn().unwrap();
            let output = holder.stdout.take().unwrap();
            let mut poll = libc::pollfd { fd: output.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: the child output remains open and poll borrows one initialized row.
            assert_eq!(unsafe { libc::poll(&raw mut poll, 1, 15_000) }, 1);
            let mut signal = String::new();
            BufReader::new(output).read_line(&mut signal).unwrap();
            assert_eq!(signal, "locked\n");
            holder
        };
        let mut holder = start_holder("holder");
        run("compete");
        drop(holder.stdin.take());
        assert!(holder.wait().unwrap().success());
        run("read-only");
        assert_eq!(fs::read(&db).unwrap(), baseline);
        assert_eq!(fs::read_dir(law.path("data")).unwrap().count(), 1);

        fs::write(&journal, sqlite_hot_journal(&baseline, None)).unwrap();
        run("read");
        assert!(!journal.exists(), "control journal must be recovered");
        let refused = sqlite_hot_journal(&baseline, Some(b"/tmp/evil"));
        fs::write(&journal, &refused).unwrap();
        run("hot-journal");
        assert_eq!(fs::read(&journal).unwrap(), refused);
        assert_eq!(fs::read(&db).unwrap(), baseline);
        fs::remove_file(&journal).unwrap();

        fs::hard_link(&db, &alias).unwrap();
        run("hard-link");
        fs::remove_file(&alias).unwrap();
        bounded(child("sync").env(preload, law.path("sync-fault.so")));
        assert_eq!(fs::read(&db).unwrap(), baseline);
        if journal.exists() {
            fs::remove_file(&journal).unwrap();
        }
        run("wal");
        // Commit a WAL frame before killing the writer so recovery must open the index.
        let mut dead = start_holder("wal-holder");
        assert!(shm.is_file());
        let identity = Command::new("ps")
            .args(["-o", "comm=", "-p", &dead.id().to_string()])
            .output().unwrap();
        assert!(identity.status.success());
        assert_eq!(Path::new(String::from_utf8_lossy(&identity.stdout).trim()).file_name(), exe.file_name());
        dead.kill().unwrap();
        assert_eq!(dead.wait().unwrap().signal(), Some(libc::SIGKILL));
        assert!(shm.is_file(), "the killed connection must leave its shared index behind");
        fs::write(&shm, [0xff; 32768]).unwrap();
        let mut stale = fs::File::open(&shm).unwrap();
        run("read");
        // Retain the old inode even if SQLite unlinks the index on close.
        let mut index = Vec::new();
        stale.read_to_end(&mut index).unwrap();
        assert!(index.is_empty() || index.last() == Some(&0), "{backend}: stale WAL index survived recovery");
        fs::remove_dir_all(law.path("data")).unwrap();
    }
}
