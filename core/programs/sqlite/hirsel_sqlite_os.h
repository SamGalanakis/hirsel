#ifndef HIRSEL_SQLITE_OS_H
#define HIRSEL_SQLITE_OS_H
#include "sqlite3.h"

/* Shared by hirsel_sqlite.c and hirsel_sqlite_os.c; objcopy makes them local. */
extern sqlite3_vfs no_files;
int no_open(sqlite3_vfs *vfs, const char *name, sqlite3_file *file, int flags, int *out);
int temp_control(sqlite3_file *base, int op, void *arg);
int temp_sector(sqlite3_file *base);
int temp_characteristics(sqlite3_file *base);
int os_initialize(void);

#endif
