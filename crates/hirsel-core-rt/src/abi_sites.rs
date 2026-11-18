//! FORM #91: late source binding for the existing Cause.site word at offset24.
//! The driver supplies linked item ordinals, paths and positions. Nothing
//! reads these borrowed tables before failure reporting; no global registry.
use crate::{causes::Cause, entry::OsProcess};
use hirsel_core_layout::source_positions;

#[unsafe(export_name = "hirsel_core_relative_source_table_v1")]
pub static RELATIVE_SOURCE_TABLE_V1: u8 = source_positions::TAG;

#[repr(C)]
pub struct SourceView {
    pub offset: usize,
    pub len: usize,
}

impl SourceView {
    // The offset is relative to the entire file directory's byte allocation.
    unsafe fn bytes(&self, files: *const SourceFile) -> &[u8] {
        // SAFETY: the source binder put this range inside the file allocation.
        unsafe { std::slice::from_raw_parts(files.cast::<u8>().add(self.offset), self.len) }
    }
}

#[repr(C)]
pub struct SourceFile {
    pub path: SourceView,
    pub source: SourceView,
}
#[repr(C)]
pub struct SourceItem {
    pub file: u32,
    pub file_offset: u32,
}
#[repr(C)]
pub struct SourceTable {
    pub files: *const SourceFile,
    pub files_len: usize,
    pub items: *const SourceItem,
    pub items_len: usize,
}

/// Bind current source storage before running an explicitly constructed process.
/// Null removes the binding. No table storage is copied, read or freed.
/// # Safety
/// The process is exclusively borrowed before run. A nonnull table, its arrays
/// and file bytes remain immutable and pinned through reporting. Each relative
/// range is within the allocation starting at `files`. Every nonzero
/// cause site designates an item and UTF-8 boundary in this linked table.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_os_process_set_sources(
    process: *mut OsProcess,
    sources: *const SourceTable,
) {
    // SAFETY: caller exclusively owns this process before entry.
    unsafe { (*process).sources = sources };
}

pub use crate::entry::hirsel_core_os_run_with_sources;

/// # Safety
/// Same borrowed table contract as set_sources; the cause pins its message.
/// Only the failure-report boundary calls this function.
pub(crate) unsafe fn format_cause(cause: &Cause, sources: *const SourceTable) -> String {
    if sources.is_null() || cause.site == 0 {
        // SAFETY: cause's message remains pinned throughout reporting.
        return unsafe { crate::abi_host::format_cause(cause) };
    }
    // SAFETY: linked item/file indices and source byte boundaries are emitted by
    // our binder. UTF-8 was checked before source positions were assigned.
    unsafe {
        let sources = &*sources;
        let item = &*sources.items.add(((cause.site >> 32) - 1) as usize);
        let file = &*sources.files.add(item.file as usize);
        let offset = item.file_offset as usize + (cause.site as u32) as usize;
        let source = file.source.bytes(sources.files);
        let (line, column) = if let Some(markers) = source.strip_prefix(&[source_positions::TAG]) {
            source_positions::position(markers, offset)
        } else {
            let prefix = &source[..offset];
            let line = prefix.iter().filter(|&&b| b == b'\n').count() + 1;
            let start = prefix
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(0, |i| i + 1);
            let column = std::str::from_utf8_unchecked(&prefix[start..])
                .chars()
                .count()
                + 1;
            (line, column)
        };
        let path = std::str::from_utf8_unchecked(file.path.bytes(sources.files));
        crate::abi_host::format_cause_at(cause, format_args!("{path}:{line}:{column}"))
    }
}
