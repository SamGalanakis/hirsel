//! FAIL-2/PROG-3: report linked sites late, retaining origin through cleanup/join.
use crate::{
    Context,
    abi::*,
    abi_sites::*,
    causes::{self, Cause, Causes},
    entry::*,
    process::{Config, Entry, MainResult},
    tasks::*,
};
use std::{io::Read, os::fd::AsRawFd, os::unix::net::UnixStream, ptr};

// Linked site words: file 0 ("\nfinalize") starts at site 1, file 1 one past
// its end at 11. PRIMARY is byte 15 of file 1, CLEANUP byte 1 of file 0.
const PRIMARY: u64 = 11 + 15;
const CLEANUP: u64 = 1 + 1;

#[repr(C)]
struct Files {
    rows: [SourceFile; 2],
    text: [u8; 128],
}

impl Files {
    fn new(main: &str) -> Self {
        let mut text = [0; 128];
        let mut cursor = 0;
        let mut view = |bytes: &str| {
            let range = SourceView {
                offset: std::mem::offset_of!(Files, text) + cursor,
                len: bytes.len(),
            };
            text[cursor..cursor + bytes.len()].copy_from_slice(bytes.as_bytes());
            cursor += bytes.len();
            range
        };
        let rows = [
            SourceFile {
                path: view("lib/final.hirsel"),
                source: view("\nfinalize"),
                start: 1,
            },
            SourceFile {
                path: view("app/é.hirsel"),
                source: view(main),
                start: 11,
            },
        ];
        Self { rows, text }
    }
}

fn files() -> Files {
    Files::new("// heading\r\nfn é🙂\tfail")
}

fn table(files: &Files) -> SourceTable {
    SourceTable {
        files: ptr::from_ref(files).cast(),
        files_len: files.rows.len(),
    }
}
fn run(sources: *const SourceTable, entry: Entry, explicit_owner: bool) -> (u8, String) {
    let (mut reader, writer) = UnixStream::pair().unwrap();
    let config = OsConfig {
        directory_ops: std::ptr::null(),
        process_ops: std::ptr::null(),
        socket_activation: std::ptr::null(),
        secret_grants: core::ptr::null(),
        secret_grants_len: 0,
        env_patterns: crate::process::View {
            data: std::ptr::null(),
            len: 0,
        },
        secret_ops: std::ptr::null(),
        env_ops: std::ptr::null(),
        target: crate::process::View {
            data: std::ptr::null(),
            len: 0,
        },
        root_limit: 1 << 20,
        args: ptr::null(),
        args_len: 0,
        directories: ptr::null(),
        directories_len: 0,
        grant_mask: 0,
        stdout_fd: writer.as_raw_fd(),
        stderr_fd: writer.as_raw_fd(),
        parameters: ptr::null(),
        parameters_len: 0,
        net_rules: std::ptr::null(),
        net_rules_len: 0,
    };
    // SAFETY: config and borrowed source storage remain live through reporting.
    let status = unsafe {
        if explicit_owner {
            let owner = hirsel_core_os_process_new(&config);
            assert_eq!(owner.error, crate::provider::IoError::OK);
            assert!(!owner.process.is_null());
            hirsel_core_os_process_set_sources(owner.process, sources);
            let status = crate::process::hirsel_core_run(
                hirsel_core_os_process_config(owner.process),
                entry,
            );
            hirsel_core_os_process_free(owner.process);
            status
        } else {
            hirsel_core_os_run_with_sources(&config, ptr::null(), sources, entry)
        }
    };
    drop(writer);
    let mut text = String::new();
    reader.read_to_string(&mut text).unwrap();
    (status, text)
}
unsafe extern "C" fn primary(ctx: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
    // SAFETY: this task owns its unused primary cause cell; literal storage is static.
    unsafe { hirsel_core_raise_string(ctx, PRIMARY, b"bad".as_ptr().cast_mut(), 3, 0) };
    1
}
unsafe extern "C" fn no_site(ctx: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
    // SAFETY: this task owns its unused primary cause cell.
    unsafe { hirsel_core_raise(ctx, Cause::new(causes::BOUNDS, 0)) };
    1
}
#[test]
fn cause_and_source_table_abi_sizes_keep_the_existing_site_word() {
    assert_eq!(size_of::<Cause>(), 32);
    assert_eq!(std::mem::offset_of!(Cause, site), 24);
    assert_eq!(size_of::<SourceTable>(), 16);
    assert_eq!(size_of::<SourceFile>(), 40);
    assert_eq!(std::mem::offset_of!(SourceFile, start), 32);
}
#[test]
fn report_uses_scalar_columns_tab_one_origin_one_and_current_item_position() {
    let f = files();
    let t = table(&f);
    assert_eq!(
        run(&t, primary, false),
        (101, "Fail(\"bad\") at app/é.hirsel:2:4\n".into())
    );
    assert_eq!(
        run(&t, primary, true),
        (101, "Fail(\"bad\") at app/é.hirsel:2:4\n".into())
    );
    // Prefix contains two multibyte scalars and a tab before the next operation.
    let c = Cause::new(causes::OVERFLOW, 11 + 22);
    // SAFETY: all linked table entries and byte positions are valid UTF-8 boundaries.
    assert_eq!(
        unsafe { crate::abi_sites::format_cause(&c, &t) },
        "Overflow at app/é.hirsel:2:7"
    );
    let c = Cause::new(causes::UNWRAP, CLEANUP);
    assert_eq!(
        unsafe { crate::abi_sites::format_cause(&c, &t) },
        "Unwrap at lib/final.hirsel:2:1"
    );
}
#[test]
fn no_source_or_none_site_keeps_numeric_fallback() {
    assert_eq!(
        run(ptr::null(), primary, false),
        (101, format!("Fail(\"bad\") at {PRIMARY}\n"))
    );
    let empty = SourceTable {
        files: ptr::null(),
        files_len: 0,
    };
    assert_eq!(run(&empty, no_site, true), (101, "Bounds at 0\n".into()));
    assert_eq!(
        run(ptr::null(), no_site, false),
        (101, "Bounds at 0\n".into())
    );
}
unsafe extern "C" fn child(ctx: *mut Context, _: *mut u8, _: *mut u8) -> u8 {
    // SAFETY: emitter-style prepaid resource finalization after the primary raise.
    unsafe {
        let resource = causes::resource_new(&*ctx, 8, 0, 8, 8).unwrap();
        hirsel_core_raise(ctx, Cause::new(causes::OVERFLOW, PRIMARY));
        causes::finalizer_begin(&mut *ctx, resource);
        hirsel_core_raise(ctx, Cause::new(causes::DIVISION, CLEANUP));
        causes::finalizer_end(&mut *ctx, resource);
        causes::resource_free(resource, 8);
    }
    1
}
unsafe extern "C" fn joined(ctx: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
    let value = ValueLayout {
        bytes: 0,
        align: 1,
        logical: 0,
        clean: None,
    };
    // SAFETY: parent owns the child handle; zero-sized input/result need no storage.
    unsafe {
        let spawned = hirsel_core_spawn(
            ctx,
            0,
            64,
            ptr::dangling_mut(),
            child,
            TaskLayout {
                input: value,
                result: value,
            },
            0,
        );
        assert!(!spawned.task.is_null());
        let result = hirsel_core_join(ctx, spawned.task, ptr::dangling_mut(), 123);
        assert_eq!((result.status, result.kind), (0, 1));
        let chain = Causes::from_raw(result.causes);
        assert_eq!(
            chain.iter().map(|c| c.site).collect::<Vec<_>>(),
            [PRIMARY, CLEANUP]
        );
        (*ctx).causes = chain;
    }
    1
}
#[test]
fn joined_child_keeps_primary_and_finalizer_sites_and_report_order() {
    let f = files();
    let t = table(&f);
    assert_eq!(
        run(&t, joined, false),
        (
            101,
            "Overflow at app/é.hirsel:2:4\nDivision at lib/final.hirsel:2:1\n".into()
        )
    );
}
unsafe extern "C" fn cancelled(ctx: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
    // SAFETY: isolated task owns its cancellation flag and primary cell.
    unsafe {
        (*ctx)
            .cancel_requested
            .store(true, std::sync::atomic::Ordering::Release);
        crate::tasks::hirsel_core_check_cancel(ctx, PRIMARY)
    }
}
#[test]
fn runtime_cancellation_keeps_the_cancellation_point_site() {
    let f = files();
    let t = table(&f);
    assert_eq!(
        run(&t, cancelled, false),
        (101, "Cancelled at app/é.hirsel:2:4\n".into())
    );
}
unsafe extern "C" fn success(_: *mut Context, _: *const Config, out: *mut MainResult) -> u8 {
    // SAFETY: entry owns this initialized MainResult destination.
    unsafe { *out = MainResult::default() };
    0
}
#[test]
fn successful_run_never_reads_borrowed_source_storage() {
    // SAFETY: a live mapping holds the file directory and the ranges it
    // names. Only an eager read of that protected storage would fault.
    unsafe {
        let bytes = libc::mmap(
            ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        assert_ne!(bytes, libc::MAP_FAILED);
        let files = bytes.cast::<Files>();
        files.write(self::files());
        assert_eq!(libc::mprotect(bytes, 4096, libc::PROT_NONE), 0);
        let sources = SourceTable {
            files: files.cast(),
            files_len: 2,
        };
        assert_eq!(run(&sources, success, false), (0, String::new()));
        assert_eq!(run(ptr::null(), success, true), (0, String::new()));
        assert_eq!(libc::munmap(bytes, 4096), 0);
    }
}
