#ifndef HIRSEL_SQLITE_OS_H
#define HIRSEL_SQLITE_OS_H
#include "sqlite3.h"

/* Shared by hirsel_sqlite.c and hirsel_sqlite_os.c. objcopy makes them local
   in an ELF row; a Mach-O row compiles both files in one unit, with HSQ_SHARED
   and HSQ_SHARED_DATA static. */
#ifndef HSQ_SHARED
#define HSQ_SHARED
#define HSQ_SHARED_DATA extern
#endif
HSQ_SHARED_DATA sqlite3_vfs no_files;
HSQ_SHARED int no_open(sqlite3_vfs *vfs, const char *name, sqlite3_file *file, int flags, int *out);
HSQ_SHARED int temp_control(sqlite3_file *base, int op, void *arg);
HSQ_SHARED int temp_sector(sqlite3_file *base);
HSQ_SHARED int temp_characteristics(sqlite3_file *base);
HSQ_SHARED int os_initialize(void);

#endif
