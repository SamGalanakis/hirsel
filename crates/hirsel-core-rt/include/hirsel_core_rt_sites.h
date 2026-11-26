#ifndef HIRSEL_CORE_RT_SITES_H
#define HIRSEL_CORE_RT_SITES_H
#include "hirsel_core_rt_host.h"
/* FORM #91: Cause remains32 bytes, site@24 is a linked site word: its file's
   start word plus the byte offset into that file; 0 is none. File rows ascend
   by start. Driver binds current source; runtime never re-sites a
   propagated/joined cause. Callers may supply UTF-8 source. Generated bindings
   carry compact positions. Each view is a byte range relative to the start of
   the files allocation. The file directory and the views it names occupy that
   one allocation. All storage is borrowed immutably through reporting. No startup parse/index,
   source copy or successful-run read. Empty table16B on both64-bit targets. */
typedef struct { size_t offset, len; } HirselCoreSourceView;
typedef struct { HirselCoreSourceView path, source; uint64_t start; } HirselCoreSourceFile;
typedef struct { const HirselCoreSourceFile *files; size_t files_len; } HirselCoreSourceTable;
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
/* Native linker entry for a closed executable with no executable digest reader. */
uint8_t hirsel_core_os_run_without_digest(const HirselCoreOsConfig *, const HirselCoreView *,
                                         const HirselCoreSourceTable *, HirselCoreEntry);
/* The recording runtime's launcher entries. Paths and code identity are
   NUL-terminated generated strings. Replay mode is 1 for replay or 2 for diagnose.
   `build` is the driver's digest of the build without its launch policy. */
uint8_t hirsel_core_os_run_recording(const HirselCoreOsConfig *, const HirselCoreView *,
    const HirselCoreSourceTable *, HirselCoreEntry, const char *identity, const uint8_t revision[32],
    const uint8_t build[32]);
uint8_t hirsel_core_os_replay(const HirselCoreOsConfig *, const HirselCoreView *,
    const HirselCoreSourceTable *, HirselCoreEntry, const char *path, uint64_t mode,
    const char *identity, const uint8_t revision[32], const uint8_t build[32],
    const uint64_t *descriptors, const uint64_t *spawns);
#endif
