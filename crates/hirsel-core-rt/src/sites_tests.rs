//! FAIL-2/PROG-3: report linked sites late, retaining origin through cleanup/join.
use crate::{
    Context,
    abi::*,
    abi_sites::*,
    causes::{self, Cause, Causes},
    entry::*,
    process::{Config, Entry, MainResult, View},
    tasks::*,
};
use std::{io::Read, os::fd::AsRawFd, os::unix::net::UnixStream, ptr};

fn view(bytes: &[u8]) -> View {
    View {
        data: bytes.as_ptr(),
        len: bytes.len(),
    }
}
const PRIMARY: u64 = (2 << 32) | 3;
const CLEANUP: u64 = 1 << 32;
fn files() -> [SourceFile; 2] {
    [
        SourceFile {
            path: view(b"lib/final.hirsel"),
            source: view(b"\nfinalize"),
        },
        SourceFile {
            path: view("app/é.hirsel".as_bytes()),
            source: view("// heading\r\nfn é🙂\tfail".as_bytes()),
        },
    ]
}
fn items() -> [SourceItem; 2] {
    [
        SourceItem {
            file: 0,
            file_offset: 1,
        },
        SourceItem {
            file: 1,
            file_offset: 12,
        },
    ]
}
fn table(files: &[SourceFile], items: &[SourceItem]) -> SourceTable {
    SourceTable {
        files: files.as_ptr(),
        files_len: files.len(),
        items: items.as_ptr(),
        items_len: items.len(),
    }
}
fn run(sources: *const SourceTable, entry: Entry, explicit_owner: bool) -> (u8, String) {
    let (mut reader, writer) = UnixStream::pair().unwrap();
    let config = OsConfig {
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
    assert_eq!(size_of::<SourceTable>(), 32);
    assert_eq!(size_of::<SourceFile>(), 32);
    assert_eq!(size_of::<SourceItem>(), 8);
    assert_eq!(std::mem::offset_of!(SourceTable, items), 16);
}
#[test]
fn report_uses_scalar_columns_tab_one_origin_one_and_current_item_position() {
    let f = files();
    let i = items();
    let t = table(&f, &i);
    assert_eq!(
        run(&t, primary, false),
        (101, "Fail(\"bad\") at app/é.hirsel:2:4\n".into())
    );
    // Same word/body, current source moved above the item and multibyte/tab before
    // the raising operation. Coordinates must follow these newly borrowed bytes.
    let moved = [
        SourceFile {
            path: view(b"lib/final.hirsel"),
            source: view(b"\nfinalize"),
        },
        SourceFile {
            path: view("app/é.hirsel".as_bytes()),
            source: view("\n\n// heading\r\nfn é🙂\tfail".as_bytes()),
        },
    ];
    let moved_items = [
        SourceItem {
            file: 0,
            file_offset: 1,
        },
        SourceItem {
            file: 1,
            file_offset: 14,
        },
    ];
    assert_eq!(
        run(&table(&moved, &moved_items), primary, true),
        (101, "Fail(\"bad\") at app/é.hirsel:4:4\n".into())
    );
    // Prefix contains two multibyte scalars and a tab before the next operation.
    let c = Cause::new(causes::OVERFLOW, (2 << 32) | 10);
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
    let empty = table(&[], &[]);
    assert_eq!(run(&empty, no_site, true), (101, "Bounds at 0\n".into()));
    assert_eq!(
        run(ptr::null(), no_site, false),
        (101, "Bounds at 0\n".into())
    );
}
unsafe extern "C" fn inert(_: *mut Context, _: *mut u8) -> u8 {
    0
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
        drop: inert,
    };
    // SAFETY: parent owns the child handle; zero-sized input/result need no storage.
    unsafe {
        let spawned = hirsel_core_spawn(
            ctx,
            64,
            ptr::dangling_mut(),
            child,
            TaskLayout {
                input: value,
                result: value,
            },
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
    let i = items();
    let t = table(&f, &i);
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
    let i = items();
    let t = table(&f, &i);
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
    // SAFETY: one owned inaccessible page stands in for cold source storage.
    // The valid descriptors stay readable; only an eager source read would fault.
    unsafe {
        let bytes = libc::mmap(
            ptr::null_mut(),
            4096,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        assert_ne!(bytes, libc::MAP_FAILED);
        let f = [SourceFile {
            path: view(b"never-read.hirsel"),
            source: View {
                data: bytes.cast(),
                len: 4096,
            },
        }];
        let i = [SourceItem {
            file: 0,
            file_offset: 0,
        }];
        assert_eq!(run(&table(&f, &i), success, false), (0, String::new()));
        assert_eq!(run(ptr::null(), success, true), (0, String::new()));
        assert_eq!(libc::munmap(bytes, 4096), 0);
    }
}
