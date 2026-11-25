#define _GNU_SOURCE
#include "hirsel_sqlite_os.h"

#include <limits.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/* The OS layer under SQLITE_OS_OTHER: real mutexes and the memory VFS. */

struct sqlite3_mutex {
    pthread_mutex_t lock;
};

static sqlite3_mutex static_mutexes[SQLITE_MUTEX_STATIC_VFS3 - SQLITE_MUTEX_STATIC_MAIN + 1];

static int mutex_init(void) {
    size_t i = 0;
    for (; i < sizeof static_mutexes / sizeof static_mutexes[0]; ++i) {
        if (pthread_mutex_init(&static_mutexes[i].lock, NULL) != 0) {
            while (i > 0) pthread_mutex_destroy(&static_mutexes[--i].lock);
            return SQLITE_ERROR;
        }
    }
    return SQLITE_OK;
}

static int mutex_end(void) {
    for (size_t i = 0; i < sizeof static_mutexes / sizeof static_mutexes[0]; ++i)
        pthread_mutex_destroy(&static_mutexes[i].lock);
    return SQLITE_OK;
}

static sqlite3_mutex *mutex_alloc(int kind) {
    if (kind >= SQLITE_MUTEX_STATIC_MAIN && kind <= SQLITE_MUTEX_STATIC_VFS3)
        return &static_mutexes[kind - SQLITE_MUTEX_STATIC_MAIN];
    if (kind != SQLITE_MUTEX_FAST && kind != SQLITE_MUTEX_RECURSIVE) return NULL;
    sqlite3_mutex *mutex = malloc(sizeof *mutex);
    if (!mutex) return NULL;
    pthread_mutexattr_t attr;
    if (pthread_mutexattr_init(&attr) != 0) {
        free(mutex);
        return NULL;
    }
    int error = 0;
    if (kind == SQLITE_MUTEX_RECURSIVE)
        error = pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_RECURSIVE);
    if (!error) error = pthread_mutex_init(&mutex->lock, &attr);
    pthread_mutexattr_destroy(&attr);
    if (!error) return mutex;
    free(mutex);
    return NULL;
}

static void mutex_free(sqlite3_mutex *mutex) {
    pthread_mutex_destroy(&mutex->lock);
    free(mutex);
}

static void mutex_enter(sqlite3_mutex *mutex) { pthread_mutex_lock(&mutex->lock); }
static void mutex_leave(sqlite3_mutex *mutex) { pthread_mutex_unlock(&mutex->lock); }
static int mutex_try(sqlite3_mutex *mutex) {
    return pthread_mutex_trylock(&mutex->lock) == 0 ? SQLITE_OK : SQLITE_BUSY;
}

typedef struct {
    sqlite3_file base;
    uint8_t *data;
    size_t size;
} TempFile;

static int temp_close(sqlite3_file *base) {
    free(((TempFile *)base)->data);
    return SQLITE_OK;
}

static int temp_truncate(sqlite3_file *base, sqlite3_int64 length) {
    TempFile *file = (TempFile *)base;
    if (length < 0 || (uint64_t)length > SIZE_MAX) return SQLITE_IOERR_TRUNCATE;
    size_t n = (size_t)length;
    if (!n) {
        free(file->data);
        file->data = NULL;
    } else {
        uint8_t *data = realloc(file->data, n);
        if (!data) return SQLITE_NOMEM;
        file->data = data;
        if (n > file->size) memset(data + file->size, 0, n - file->size);
    }
    file->size = n;
    return SQLITE_OK;
}

static int temp_read(sqlite3_file *base, void *out, int length, sqlite3_int64 offset) {
    TempFile *file = (TempFile *)base;
    if (offset < 0 || length < 0) return SQLITE_IOERR_READ;
    size_t n = (size_t)length;
    size_t available = (uint64_t)offset >= file->size ? 0 : file->size - (size_t)offset;
    size_t copied = available < n ? available : n;
    if (copied) memcpy(out, file->data + (size_t)offset, copied);
    if (copied == n) return SQLITE_OK;
    memset((uint8_t *)out + copied, 0, n - copied);
    return SQLITE_IOERR_SHORT_READ;
}

static int temp_write(sqlite3_file *base, const void *src, int length, sqlite3_int64 offset) {
    TempFile *file = (TempFile *)base;
    if (offset < 0 || length < 0 || offset > INT64_MAX - length) return SQLITE_IOERR_WRITE;
    sqlite3_int64 end = offset + length;
    if ((uint64_t)end > file->size) {
        int rc = temp_truncate(base, end);
        if (rc != SQLITE_OK) return rc;
    }
    if (length) memcpy(file->data + (size_t)offset, src, (size_t)length);
    return SQLITE_OK;
}

static int temp_size(sqlite3_file *base, sqlite3_int64 *size) {
    *size = (sqlite3_int64)((TempFile *)base)->size;
    return SQLITE_OK;
}

static int temp_noop(sqlite3_file *base, int flags) {
    (void)base; (void)flags;
    return SQLITE_OK;
}

static int temp_reserved(sqlite3_file *base, int *held) {
    (void)base;
    *held = 0;
    return SQLITE_OK;
}

HSQ_SHARED int temp_control(sqlite3_file *base, int op, void *arg) {
    (void)base; (void)op; (void)arg;
    return SQLITE_NOTFOUND;
}

HSQ_SHARED int temp_sector(sqlite3_file *base) { (void)base; return 512; }
HSQ_SHARED int temp_characteristics(sqlite3_file *base) { (void)base; return 0; }

static const sqlite3_io_methods temp_methods = {
    .iVersion = 1,
    .xClose = temp_close,
    .xRead = temp_read,
    .xWrite = temp_write,
    .xTruncate = temp_truncate,
    .xSync = temp_noop,
    .xFileSize = temp_size,
    .xLock = temp_noop,
    .xUnlock = temp_noop,
    .xCheckReservedLock = temp_reserved,
    .xFileControl = temp_control,
    .xSectorSize = temp_sector,
    .xDeviceCharacteristics = temp_characteristics,
};

HSQ_SHARED int no_open(sqlite3_vfs *vfs, const char *name, sqlite3_file *file, int flags, int *out) {
    (void)vfs;
    file->pMethods = NULL;
    if (name || (flags & SQLITE_OPEN_SUPER_JOURNAL)) return SQLITE_CANTOPEN;
    TempFile *temp = (TempFile *)file;
    *temp = (TempFile){.base.pMethods = &temp_methods};
    if (out) *out = flags;
    return SQLITE_OK;
}

static int no_delete(sqlite3_vfs *vfs, const char *name, int sync_dir) {
    (void)vfs; (void)name; (void)sync_dir;
    return SQLITE_IOERR_DELETE;
}

static int no_access(sqlite3_vfs *vfs, const char *name, int flags, int *out) {
    (void)vfs; (void)name; (void)flags;
    *out = 0;
    return SQLITE_OK;
}

static int no_path(sqlite3_vfs *vfs, const char *name, int n, char *out) {
    (void)vfs; (void)name; (void)n; (void)out;
    return SQLITE_CANTOPEN;
}

static int zero_randomness(sqlite3_vfs *vfs, int n, char *out) {
    (void)vfs;
    memset(out, 0, (size_t)n);
    return n;
}

static int no_sleep(sqlite3_vfs *vfs, int microseconds) {
    (void)vfs; (void)microseconds;
    return 0;
}

static int no_clock(sqlite3_vfs *vfs, double *out) {
    (void)vfs; (void)out;
    return SQLITE_ERROR;
}

static int no_clock64(sqlite3_vfs *vfs, sqlite3_int64 *out) {
    (void)vfs; (void)out;
    return SQLITE_ERROR;
}

HSQ_SHARED sqlite3_vfs no_files = {
    .iVersion = 2,
    .szOsFile = sizeof(TempFile),
    .mxPathname = 1,
    .zName = "hirsel-none",
    .xOpen = no_open,
    .xDelete = no_delete,
    .xAccess = no_access,
    .xFullPathname = no_path,
    .xRandomness = zero_randomness,
    .xSleep = no_sleep,
    .xCurrentTime = no_clock,
    .xCurrentTimeInt64 = no_clock64,
};

SQLITE_API int sqlite3_os_init(void) { return sqlite3_vfs_register(&no_files, 1); }
SQLITE_API int sqlite3_os_end(void) { return SQLITE_OK; }

HSQ_SHARED int os_initialize(void) {
    sqlite3_mutex_methods methods = {
        .xMutexInit = mutex_init,
        .xMutexEnd = mutex_end,
        .xMutexAlloc = mutex_alloc,
        .xMutexFree = mutex_free,
        .xMutexEnter = mutex_enter,
        .xMutexTry = mutex_try,
        .xMutexLeave = mutex_leave,
    };
    int code = sqlite3_config(SQLITE_CONFIG_MUTEX, &methods);
    return code == SQLITE_OK ? sqlite3_initialize() : code;
}
