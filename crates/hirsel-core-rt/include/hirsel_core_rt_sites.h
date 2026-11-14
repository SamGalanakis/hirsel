#ifndef HIRSEL_CORE_RT_SITES_H
#define HIRSEL_CORE_RT_SITES_H
#include "hirsel_core_rt_host.h"
/* FORM #91: Cause remains32 bytes, site@24 is
   ((linked_item_ordinal + 1) << 32) | item_relative_byte_offset; 0 is none.
   Driver binds current source and relocates item IDs; runtime never re-sites a
   propagated/joined cause. UTF-8 paths/bytes and both arrays borrow immutably
   through reporting. The process does not free them. No startup parse/index,
   source copy or successful-run read. Empty table32B on both64-bit targets. */
typedef struct { HirselCoreView path, source; } HirselCoreSourceFile;
typedef struct { uint32_t file, file_offset; } HirselCoreSourceItem;
typedef struct {
    const HirselCoreSourceFile *files; size_t files_len;
    const HirselCoreSourceItem *items; size_t items_len;
} HirselCoreSourceTable;
/* Before run, bind or unbind(NULL) an explicitly constructed OS process.
   Failure101 reports Kind [or Fail("message")] at path:line:column, primary
   first; LANG-1 scalar columns/lines start at1, tab counts one. With no table
   or site0, existing numeric site formatting remains. Linked indices and byte
   boundaries follow the trusted source binder contract. */
void hirsel_core_os_process_set_sources(HirselCoreOsProcess *, const HirselCoreSourceTable *);
/* Same startup/exit contract as os_run, with one borrowed source-table binding.
   Existing entry points and Config/OsConfig layouts are unchanged. */
uint8_t hirsel_core_os_run_with_sources(const HirselCoreOsConfig *, const HirselCoreView *parameter_names,
                                      const HirselCoreSourceTable *, HirselCoreEntry);
#endif
