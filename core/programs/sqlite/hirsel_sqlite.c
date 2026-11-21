#define _GNU_SOURCE
#include "native.h"
#include "sqlite3.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <linux/openat2.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

HIRSEL_ABI_TEXT

struct sqlite3_mutex {
    pthread_mutex_t lock;
};

static sqlite3_mutex static_mutexes[SQLITE_MUTEX_STATIC_VFS3 - SQLITE_MUTEX_STATIC_MAIN + 1];
static pthread_once_t init_once = PTHREAD_ONCE_INIT;
static int init_code = SQLITE_ERROR;

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

static int temp_control(sqlite3_file *base, int op, void *arg) {
    (void)base; (void)op; (void)arg;
    return SQLITE_NOTFOUND;
}

static int temp_sector(sqlite3_file *base) { (void)base; return 512; }
static int temp_characteristics(sqlite3_file *base) { (void)base; return 0; }

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

static int no_open(sqlite3_vfs *vfs, const char *name, sqlite3_file *file, int flags, int *out) {
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

static sqlite3_vfs no_files = {
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

int sqlite3_os_init(void) { return sqlite3_vfs_register(&no_files, 1); }
int sqlite3_os_end(void) { return SQLITE_OK; }

static void initialize(void) {
    sqlite3_mutex_methods methods = {
        .xMutexInit = mutex_init,
        .xMutexEnd = mutex_end,
        .xMutexAlloc = mutex_alloc,
        .xMutexFree = mutex_free,
        .xMutexEnter = mutex_enter,
        .xMutexTry = mutex_try,
        .xMutexLeave = mutex_leave,
    };
    init_code = sqlite3_config(SQLITE_CONFIG_MUTEX, &methods);
    if (init_code == SQLITE_OK) init_code = sqlite3_initialize();
}

static int authorize(void *ctx, int op, const char *a, const char *b, const char *schema, const char *trigger) {
    (void)ctx; (void)b; (void)schema; (void)trigger;
    if (op == SQLITE_ATTACH || op == SQLITE_DETACH) return SQLITE_DENY;
    if (op == SQLITE_PRAGMA && a != NULL) {
        const char *blocked[] = {
            "temp_store_directory", "data_store_directory", "soft_heap_limit", "hard_heap_limit",
        };
        for (size_t i = 0; i < sizeof blocked / sizeof blocked[0]; ++i)
            if (strcasecmp(a, blocked[i]) == 0) return SQLITE_DENY;
    }
    return SQLITE_OK;
}

/* SQLite's lock bytes in the database and WAL index. */
#define DB_PENDING ((off_t)0x40000000)
#define DB_RESERVED (DB_PENDING + 1)
#define DB_SHARED (DB_PENDING + 2)
#define DB_SHARED_LEN 510
#define SHM_LOCK_BASE ((22 + SQLITE_SHM_NLOCK) * 4)
#define SHM_DEADMAN 128

typedef struct {
    sqlite3_vfs vfs;
    int root;
    bool writable;
    char leaf[201];
    char name[48];
} DirVfs;

typedef struct {
    void *address;
    size_t size;
} Mapping;

typedef struct {
    sqlite3_file base;
    DirVfs *dir;
    int fd;
    int level;
    int shm;
    Mapping *maps;
    size_t mapped;
} DirFile;

typedef enum { INVALID, DATABASE, JOURNAL, WAL, SHM } FileKind;

static FileKind file_kind(const DirVfs *dir, const char *name) {
    if (name == NULL || name[0] != '/') return INVALID;
    size_t n = strlen(dir->leaf);
    if (strncmp(name + 1, dir->leaf, n) != 0) return INVALID;
    const char *suffix = name + 1 + n;
    if (!*suffix) return DATABASE;
    if (strcmp(suffix, "-journal") == 0) return JOURNAL;
    if (strcmp(suffix, "-wal") == 0) return WAL;
    if (strcmp(suffix, "-shm") == 0) return SHM;
    return INVALID;
}

static int beneath(const DirVfs *dir, const char *leaf, int flags) {
    struct open_how how = {
        .flags = (uint64_t)(flags | O_CLOEXEC),
        .mode = (flags & O_CREAT) ? 0600 : 0,
        .resolve = RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS,
    };
    return (int)syscall(SYS_openat2, dir->root, leaf, &how, sizeof how);
}

static int set_lock(int fd, short type, off_t at, off_t len, int error) {
    struct flock lock = {
        .l_type = type,
        .l_whence = SEEK_SET,
        .l_start = at,
        .l_len = len,
    };
    if (fcntl(fd, F_OFD_SETLK, &lock) == 0) return SQLITE_OK;
    return (errno == EAGAIN || errno == EACCES) ? SQLITE_BUSY : error;
}

static int disk_read(sqlite3_file *base, void *data, int length, sqlite3_int64 offset) {
    const DirFile *file = (const DirFile *)base;
    if (offset < 0 || length < 0 || offset > INT64_MAX - length) return SQLITE_IOERR_READ;
    uint8_t *p = data;
    while (length) {
        ssize_t n = pread(file->fd, p, (size_t)length, offset);
        if (n < 0 && errno == EINTR) continue;
        if (n < 0) return SQLITE_IOERR_READ;
        if (n == 0) {
            memset(p, 0, (size_t)length);
            return SQLITE_IOERR_SHORT_READ;
        }
        p += n;
        offset += n;
        length -= (int)n;
    }
    return SQLITE_OK;
}

static int disk_write(sqlite3_file *base, const void *data, int length, sqlite3_int64 offset) {
    const DirFile *file = (const DirFile *)base;
    if (!file->dir->writable) return SQLITE_READONLY;
    if (offset < 0 || length < 0 || offset > INT64_MAX - length) return SQLITE_IOERR_WRITE;
    const uint8_t *p = data;
    while (length) {
        ssize_t n = pwrite(file->fd, p, (size_t)length, offset);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) return errno == ENOSPC ? SQLITE_FULL : SQLITE_IOERR_WRITE;
        p += n;
        offset += n;
        length -= (int)n;
    }
    return SQLITE_OK;
}

static int disk_truncate(sqlite3_file *base, sqlite3_int64 size) {
    const DirFile *file = (const DirFile *)base;
    if (!file->dir->writable) return SQLITE_READONLY;
    return ftruncate(file->fd, size) == 0 ? SQLITE_OK : SQLITE_IOERR_TRUNCATE;
}

static int disk_sync(sqlite3_file *base, int flags) {
    (void)flags;
    return fsync(((DirFile *)base)->fd) == 0 ? SQLITE_OK : SQLITE_IOERR_FSYNC;
}

static int disk_size(sqlite3_file *base, sqlite3_int64 *size) {
    struct stat stat;
    if (fstat(((DirFile *)base)->fd, &stat) != 0) return SQLITE_IOERR_FSTAT;
    *size = stat.st_size;
    return SQLITE_OK;
}

static int disk_lock(sqlite3_file *base, int target) {
    DirFile *file = (DirFile *)base;
    if (target <= file->level) return SQLITE_OK;
    int fd = file->fd;
    int rc;
    if (target == SQLITE_LOCK_SHARED) {
        rc = set_lock(fd, F_RDLCK, DB_PENDING, 1, SQLITE_IOERR_LOCK);
        if (rc != SQLITE_OK) return rc;
        rc = set_lock(fd, F_RDLCK, DB_SHARED, DB_SHARED_LEN, SQLITE_IOERR_LOCK);
        int unlock = set_lock(fd, F_UNLCK, DB_PENDING, 1, SQLITE_IOERR_UNLOCK);
        if (rc == SQLITE_OK) file->level = SQLITE_LOCK_SHARED;
        return rc == SQLITE_OK ? unlock : rc;
    }
    if (target == SQLITE_LOCK_RESERVED) {
        rc = set_lock(fd, F_WRLCK, DB_RESERVED, 1, SQLITE_IOERR_LOCK);
        if (rc == SQLITE_OK) file->level = SQLITE_LOCK_RESERVED;
        return rc;
    }
    if (file->level < SQLITE_LOCK_PENDING) {
        rc = set_lock(fd, F_WRLCK, DB_PENDING, 1, SQLITE_IOERR_LOCK);
        if (rc != SQLITE_OK) return rc;
        file->level = SQLITE_LOCK_PENDING;
    }
    if (target == SQLITE_LOCK_PENDING) return SQLITE_OK;
    rc = set_lock(fd, F_WRLCK, DB_SHARED, DB_SHARED_LEN, SQLITE_IOERR_LOCK);
    if (rc == SQLITE_OK) file->level = SQLITE_LOCK_EXCLUSIVE;
    return rc;
}

static int disk_unlock(sqlite3_file *base, int target) {
    DirFile *file = (DirFile *)base;
    if (target >= file->level) return SQLITE_OK;
    int fd = file->fd;
    int rc;
    if (target == SQLITE_LOCK_SHARED) {
        rc = set_lock(fd, F_RDLCK, DB_SHARED, DB_SHARED_LEN, SQLITE_IOERR_UNLOCK);
        if (rc != SQLITE_OK) return rc;
        rc = set_lock(fd, F_UNLCK, DB_PENDING, 2, SQLITE_IOERR_UNLOCK);
    } else {
        rc = set_lock(fd, F_UNLCK, DB_PENDING, DB_SHARED_LEN + 2, SQLITE_IOERR_UNLOCK);
    }
    if (rc == SQLITE_OK) file->level = target;
    return rc;
}

static int disk_reserved(sqlite3_file *base, int *held) {
    DirFile *file = (DirFile *)base;
    if (file->level >= SQLITE_LOCK_RESERVED) {
        *held = 1;
        return SQLITE_OK;
    }
    struct flock lock = {
        .l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = DB_RESERVED, .l_len = 1,
    };
    if (fcntl(file->fd, F_OFD_GETLK, &lock) != 0) return SQLITE_IOERR_CHECKRESERVEDLOCK;
    *held = lock.l_type != F_UNLCK;
    return SQLITE_OK;
}

static void shm_name(const DirVfs *dir, char name[205]) {
    snprintf(name, 205, "%s-shm", dir->leaf);
}

static int open_shm(DirFile *file) {
    char name[205];
    shm_name(file->dir, name);
    int fd = beneath(file->dir, name, O_RDWR | O_CREAT | O_NONBLOCK);
    if (fd < 0) return SQLITE_IOERR_SHMOPEN;
    struct stat stat;
    int rc = SQLITE_OK;
    if (fstat(fd, &stat) != 0 || !S_ISREG(stat.st_mode)) {
        rc = SQLITE_IOERR_SHMOPEN;
    } else {
        rc = set_lock(fd, F_WRLCK, SHM_DEADMAN, 1, SQLITE_IOERR_SHMLOCK);
        if (rc == SQLITE_OK && ftruncate(fd, 0) != 0) rc = SQLITE_IOERR_SHMSIZE;
        if (rc == SQLITE_OK || rc == SQLITE_BUSY)
            rc = set_lock(fd, F_RDLCK, SHM_DEADMAN, 1, SQLITE_IOERR_SHMLOCK);
    }
    if (rc != SQLITE_OK) close(fd);
    else file->shm = fd;
    return rc;
}

static int disk_shm_map(sqlite3_file *base, int page, int length, int extend, void volatile **out) {
    DirFile *file = (DirFile *)base;
    *out = NULL;
    if (!file->dir->writable) return SQLITE_IOERR_SHMOPEN;
    if (page < 0 || length <= 0) return SQLITE_IOERR_SHMSIZE;
    if (file->shm < 0) {
        int rc = open_shm(file);
        if (rc != SQLITE_OK) return rc;
    }
    size_t index = (size_t)page;
    if (index < file->mapped && file->maps[index].address) {
        if (file->maps[index].size != (size_t)length) return SQLITE_IOERR_SHMSIZE;
        *out = file->maps[index].address;
        return SQLITE_OK;
    }
    off_t offset = (off_t)page * length;
    off_t end = offset + length;
    struct stat stat;
    if (fstat(file->shm, &stat) != 0) return SQLITE_IOERR_SHMSIZE;
    if (stat.st_size < end) {
        if (!extend) return SQLITE_OK;
        if (ftruncate(file->shm, end) != 0) return SQLITE_IOERR_SHMSIZE;
    }
    if (index >= file->mapped) {
        size_t count = index + 1;
        if (count > SIZE_MAX / sizeof *file->maps) return SQLITE_NOMEM;
        Mapping *maps = realloc(file->maps, count * sizeof *file->maps);
        if (!maps) return SQLITE_NOMEM;
        memset(maps + file->mapped, 0, (count - file->mapped) * sizeof *maps);
        file->maps = maps;
        file->mapped = count;
    }
    void *address = mmap(NULL, (size_t)length, PROT_READ | PROT_WRITE, MAP_SHARED, file->shm, offset);
    if (address == MAP_FAILED) return SQLITE_IOERR_SHMMAP;
    file->maps[index] = (Mapping){address, (size_t)length};
    *out = address;
    return SQLITE_OK;
}

static int disk_shm_lock(sqlite3_file *base, int slot, int length, int flags) {
    DirFile *file = (DirFile *)base;
    if (slot < 0 || length <= 0 || slot > SQLITE_SHM_NLOCK - length || file->shm < 0)
        return SQLITE_IOERR_SHMLOCK;
    short type = flags & SQLITE_SHM_UNLOCK ? F_UNLCK
        : flags & SQLITE_SHM_EXCLUSIVE ? F_WRLCK : F_RDLCK;
    return set_lock(file->shm, type, SHM_LOCK_BASE + slot, length, SQLITE_IOERR_SHMLOCK);
}

static void disk_shm_barrier(sqlite3_file *base) {
    (void)base;
    atomic_thread_fence(memory_order_seq_cst);
}

static int disk_shm_unmap(sqlite3_file *base, int remove) {
    DirFile *file = (DirFile *)base;
    for (size_t i = 0; i < file->mapped; ++i) {
        if (file->maps[i].address) munmap(file->maps[i].address, file->maps[i].size);
    }
    free(file->maps);
    file->maps = NULL;
    file->mapped = 0;
    if (file->shm < 0) return SQLITE_OK;
    int rc = SQLITE_OK;
    if (remove) {
        int lock = set_lock(file->shm, F_WRLCK, SHM_DEADMAN, 1, SQLITE_IOERR_SHMLOCK);
        if (lock == SQLITE_OK) {
            char name[205];
            shm_name(file->dir, name);
            if (unlinkat(file->dir->root, name, 0) != 0 && errno != ENOENT)
                rc = SQLITE_IOERR_DELETE;
        } else if (lock != SQLITE_BUSY) {
            rc = lock;
        }
    }
    close(file->shm);
    file->shm = -1;
    return rc;
}

static int disk_close(sqlite3_file *base) {
    DirFile *file = (DirFile *)base;
    int rc = disk_shm_unmap(base, 0);
    if (close(file->fd) != 0 && rc == SQLITE_OK) rc = SQLITE_IOERR_CLOSE;
    return rc;
}

static const sqlite3_io_methods disk_methods = {
    .iVersion = 2,
    .xClose = disk_close,
    .xRead = disk_read,
    .xWrite = disk_write,
    .xTruncate = disk_truncate,
    .xSync = disk_sync,
    .xFileSize = disk_size,
    .xLock = disk_lock,
    .xUnlock = disk_unlock,
    .xCheckReservedLock = disk_reserved,
    .xFileControl = temp_control,
    .xSectorSize = temp_sector,
    .xDeviceCharacteristics = temp_characteristics,
    .xShmMap = disk_shm_map,
    .xShmLock = disk_shm_lock,
    .xShmBarrier = disk_shm_barrier,
    .xShmUnmap = disk_shm_unmap,
};

static uint32_t be32(const uint8_t *bytes) {
    return (uint32_t)bytes[0] << 24 | (uint32_t)bytes[1] << 16
        | (uint32_t)bytes[2] << 8 | (uint32_t)bytes[3];
}

static int reject_super_journal(const DirFile *file, off_t size) {
    /* SQLite 3.54.1 treats a missing super-journal as a committed transaction.
       Reject that record before recovery so a forbidden reference cannot turn
       into an apparently absent one through xAccess. */
    static const uint8_t magic[] = {0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7};
    if (size < 28 + 20) return SQLITE_OK;
    uint8_t footer[16];
    int rc = disk_read((sqlite3_file *)file, footer, sizeof footer, size - sizeof footer);
    if (rc != SQLITE_OK) return rc;
    uint32_t length = be32(footer);
    if (length == 0 || (off_t)length > size - 20
        || memcmp(footer + 8, magic, sizeof magic) != 0) return SQLITE_OK;
    uint8_t header[28];
    rc = disk_read((sqlite3_file *)file, header, sizeof header, 0);
    if (rc != SQLITE_OK) return rc;
    uint32_t page_size = be32(header + 24);
    if (memcmp(header, magic, sizeof magic) != 0
        || page_size < 512 || page_size > 65536 || (page_size & (page_size - 1)))
        return SQLITE_OK;
    uint8_t marker[4];
    rc = disk_read((sqlite3_file *)file, marker, sizeof marker, size - 20 - length);
    if (rc != SQLITE_OK) return rc;
    return be32(marker) == DB_PENDING / page_size + 1 ? SQLITE_CANTOPEN : SQLITE_OK;
}

static int dir_open(sqlite3_vfs *vfs, const char *name, sqlite3_file *base, int flags, int *out) {
    base->pMethods = NULL;
    const DirVfs *dir = vfs->pAppData;
    if (flags & SQLITE_OPEN_SUPER_JOURNAL) return SQLITE_CANTOPEN;
    if (!dir->writable && (flags & (SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE)))
        return SQLITE_CANTOPEN;
    if (!name) return no_open(vfs, name, base, flags, out);
    FileKind kind = file_kind(dir, name);
    if (!((kind == DATABASE && (flags & SQLITE_OPEN_MAIN_DB))
        || (kind == JOURNAL && (flags & SQLITE_OPEN_MAIN_JOURNAL))
        || (kind == WAL && (flags & SQLITE_OPEN_WAL)))) return SQLITE_CANTOPEN;

    int mode = (flags & SQLITE_OPEN_READWRITE ? O_RDWR : O_RDONLY) | O_NONBLOCK;
    bool created = false;
    int fd;
    if (flags & SQLITE_OPEN_CREATE) {
        fd = beneath(dir, name + 1, mode | O_CREAT | O_EXCL);
        if (fd >= 0) created = true;
        else if (errno == EEXIST && !(flags & SQLITE_OPEN_EXCLUSIVE))
            fd = beneath(dir, name + 1, mode);
    } else {
        fd = beneath(dir, name + 1, mode);
    }
    if (fd < 0) return SQLITE_CANTOPEN;
    struct stat stat;
    int rc = SQLITE_OK;
    if (fstat(fd, &stat) != 0 || !S_ISREG(stat.st_mode) || (kind == DATABASE && stat.st_nlink != 1))
        rc = SQLITE_CANTOPEN;
    if (rc == SQLITE_OK && kind == JOURNAL && !created) {
        DirFile candidate = {.fd = fd};
        rc = reject_super_journal(&candidate, stat.st_size);
    }
    if (rc == SQLITE_OK && created && (kind == JOURNAL || kind == WAL) && fsync(dir->root) != 0)
        rc = SQLITE_IOERR_DIR_FSYNC;
    if (rc != SQLITE_OK) {
        close(fd);
        return rc;
    }
    *(DirFile *)base = (DirFile){.base.pMethods = &disk_methods, .dir = vfs->pAppData, .fd = fd, .shm = -1};
    if (out) *out = flags;
    return SQLITE_OK;
}

static int dir_access(sqlite3_vfs *vfs, const char *name, int flags, int *out) {
    const DirVfs *dir = vfs->pAppData;
    *out = 0;
    if (file_kind(dir, name) == INVALID || (flags == SQLITE_ACCESS_READWRITE && !dir->writable))
        return SQLITE_OK;
    int fd = beneath(dir, name + 1, O_PATH);
    if (fd < 0) return SQLITE_OK;
    struct stat stat;
    *out = fstat(fd, &stat) == 0 && S_ISREG(stat.st_mode);
    close(fd);
    return SQLITE_OK;
}

static int dir_delete(sqlite3_vfs *vfs, const char *name, int sync_dir) {
    const DirVfs *dir = vfs->pAppData;
    FileKind kind = file_kind(dir, name);
    if (!dir->writable || kind == INVALID) return SQLITE_IOERR_DELETE;
    if (unlinkat(dir->root, name + 1, 0) != 0)
        return errno == ENOENT ? SQLITE_IOERR_DELETE_NOENT : SQLITE_IOERR_DELETE;
    if ((sync_dir || kind == JOURNAL || kind == WAL) && fsync(dir->root) != 0)
        return SQLITE_IOERR_DIR_FSYNC;
    return SQLITE_OK;
}

static int dir_path(sqlite3_vfs *vfs, const char *name, int length, char *out) {
    const DirVfs *dir = vfs->pAppData;
    if (!name || (strcmp(name, dir->leaf) != 0 && file_kind(dir, name) != DATABASE))
        return SQLITE_CANTOPEN;
    int n = snprintf(out, (size_t)length, "/%s", dir->leaf);
    return n < 0 || n >= length ? SQLITE_CANTOPEN : SQLITE_OK;
}

static void release_dir(DirVfs *dir) {
    if (!dir) return;
    sqlite3_vfs_unregister(&dir->vfs);
    close(dir->root);
    free(dir);
}

typedef struct {
    sqlite3 *db;
    DirVfs *dir;
    _Atomic size_t refs;
} Connection;

typedef struct {
    Connection *connection;
    sqlite3_stmt *stmt;
    uint32_t columns;
    int row;
    int column_types[];
} Statement;

static int record_error(uint8_t *out, uint64_t length, int code, const char *text) {
    if (length < 260) return code;
    uint32_t bits = (uint32_t)code;
    for (unsigned i = 0; i < 4; ++i) out[i] = (uint8_t)(bits >> (8 * i));
    memset(out + 4, 0, 256);
    size_t n = strnlen(text, 257);
    if (n > 256) {
        n = 256;
        while (n && ((unsigned char)text[n] & 0xc0) == 0x80) --n;
    }
    memcpy(out + 4, text, n);
    return code;
}

static int database_error(sqlite3 *db, uint8_t *out, uint64_t length, int code) {
    if (db && (sqlite3_errcode(db) & 0xff) == (code & 0xff)) {
        code = sqlite3_extended_errcode(db);
        return record_error(out, length, code, sqlite3_errmsg(db));
    }
    return record_error(out, length, code, sqlite3_errstr(code));
}

static int release_connection(Connection *connection) {
    if (atomic_fetch_sub_explicit(&connection->refs, 1, memory_order_acq_rel) != 1)
        return SQLITE_OK;
    int code = sqlite3_close(connection->db);
    release_dir(connection->dir);
    free(connection);
    return code;
}

static Connection *open_connection(const char *name, int flags, DirVfs *dir,
                                   uint8_t *e, uint64_t e_len) {
    sqlite3 *db = NULL;
    int code = sqlite3_open_v2(name, &db, flags | SQLITE_OPEN_EXRESCODE,
                              dir ? dir->name : "hirsel-none");
    if (code == SQLITE_OK) code = sqlite3_set_authorizer(db, authorize, NULL);
    if (code != SQLITE_OK) {
        database_error(db, e, e_len, code);
        if (db) sqlite3_close(db);
        release_dir(dir);
        return NULL;
    }
    Connection *connection = malloc(sizeof *connection);
    if (!connection) {
        database_error(NULL, e, e_len, SQLITE_NOMEM);
        sqlite3_close(db);
        release_dir(dir);
        return NULL;
    }
    connection->db = db;
    connection->dir = dir;
    atomic_init(&connection->refs, 1);
    return connection;
}

void *hsq_open_memory(uint8_t *e, uint64_t e_len) {
    pthread_once(&init_once, initialize);
    if (init_code != SQLITE_OK) {
        database_error(NULL, e, e_len, init_code);
        return NULL;
    }
    return open_connection(":memory:", SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, NULL, e, e_len);
}

static bool valid_leaf(const uint8_t *name, uint64_t n) {
    if (n == 0 || n > 200 || name[0] == '.') return false;
    for (uint64_t i = 0; i < n; ++i) {
        uint8_t byte = name[i];
        if (!((byte >= 'A' && byte <= 'Z') || (byte >= 'a' && byte <= 'z')
            || (byte >= '0' && byte <= '9') || byte == '.' || byte == '_' || byte == '-')) {
            return false;
        }
    }
    return !(n >= 8 && memcmp(name + n - 8, "-journal", 8) == 0)
        && !(n >= 4 && (memcmp(name + n - 4, "-wal", 4) == 0
                     || memcmp(name + n - 4, "-shm", 4) == 0));
}

static _Atomic uint64_t vfs_serial = 0;

void *hsq_open(int d_fd, uint32_t d_flags, const uint8_t *name, uint64_t n,
               uint8_t *e, uint64_t e_len) {
    if (d_fd < 0) {
        database_error(NULL, e, e_len, SQLITE_CANTOPEN);
        return NULL;
    }
    if (!valid_leaf(name, n)) {
        close(d_fd);
        record_error(e, e_len, SQLITE_CANTOPEN, "sqlite: name is not one leaf in the directory");
        return NULL;
    }
    pthread_once(&init_once, initialize);
    if (init_code != SQLITE_OK) {
        close(d_fd);
        database_error(NULL, e, e_len, init_code);
        return NULL;
    }
    DirVfs *dir = calloc(1, sizeof *dir);
    if (!dir) {
        close(d_fd);
        database_error(NULL, e, e_len, SQLITE_NOMEM);
        return NULL;
    }
    dir->root = d_fd;
    memcpy(dir->leaf, name, (size_t)n);
    dir->writable = (d_flags & 1) != 0;
    uint64_t serial = atomic_fetch_add_explicit(&vfs_serial, 1, memory_order_relaxed);
    snprintf(dir->name, sizeof dir->name, "hirsel-dir-%llu", (unsigned long long)serial);
    dir->vfs = no_files;
    dir->vfs.szOsFile = sizeof(DirFile) > sizeof(TempFile) ? sizeof(DirFile) : sizeof(TempFile);
    dir->vfs.mxPathname = 210;
    dir->vfs.zName = dir->name;
    dir->vfs.pAppData = dir;
    dir->vfs.xOpen = dir_open;
    dir->vfs.xDelete = dir_delete;
    dir->vfs.xAccess = dir_access;
    dir->vfs.xFullPathname = dir_path;
    int code = sqlite3_vfs_register(&dir->vfs, 0);
    if (code != SQLITE_OK) {
        release_dir(dir);
        database_error(NULL, e, e_len, code);
        return NULL;
    }
    int flags = dir->writable ? SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE : SQLITE_OPEN_READONLY;
    return open_connection(dir->leaf, flags, dir, e, e_len);
}

static int sql_input(const uint8_t *sql, uint64_t n, uint8_t *e, uint64_t e_len) {
    if (n > INT_MAX) return record_error(e, e_len, SQLITE_TOOBIG, "sqlite: SQL is too long");
    if (memchr(sql, 0, (size_t)n))
        return record_error(e, e_len, SQLITE_ERROR, "sqlite: SQL holds a zero byte");
    return SQLITE_OK;
}

static int next_statement(sqlite3 *db, const char **p, int *len, sqlite3_stmt **stmt) {
    const char *tail = *p;
    int rc = sqlite3_prepare_v3(db, *p, *len, 0, stmt, &tail);
    if (rc != SQLITE_OK) return rc;
    if (tail <= *p || tail - *p > *len) return SQLITE_MISUSE;
    *len -= (int)(tail - *p);
    *p = tail;
    return SQLITE_OK;
}

int32_t hsq_exec(void *handle, const uint8_t *sql, uint64_t n, uint8_t *e, uint64_t e_len) {
    int rc = sql_input(sql, n, e, e_len);
    if (rc != SQLITE_OK) return rc;
    sqlite3 *db = ((Connection *)handle)->db;
    sqlite3_mutex *mutex = sqlite3_db_mutex(db);
    sqlite3_mutex_enter(mutex);
    const char *p = (const char *)sql;
    for (int len = (int)n; len > 0 && rc == SQLITE_OK;) {
        sqlite3_stmt *stmt = NULL;
        rc = next_statement(db, &p, &len, &stmt);
        if (stmt && rc == SQLITE_OK) {
            do { rc = sqlite3_step(stmt); } while (rc == SQLITE_ROW);
            if (rc == SQLITE_DONE) rc = SQLITE_OK;
        }
        if (rc != SQLITE_OK) rc = database_error(db, e, e_len, rc);
        int finalized = sqlite3_finalize(stmt);
        if (rc == SQLITE_OK && finalized != SQLITE_OK)
            rc = database_error(db, e, e_len, finalized);
    }
    sqlite3_mutex_leave(mutex);
    return rc;
}

void *hsq_prepare(void *handle, const uint8_t *sql, uint64_t n, uint8_t *e, uint64_t e_len) {
    if (sql_input(sql, n, e, e_len) != SQLITE_OK) return NULL;
    Connection *connection = handle;
    sqlite3 *db = connection->db;
    sqlite3_mutex *mutex = sqlite3_db_mutex(db);
    sqlite3_mutex_enter(mutex);
    sqlite3_stmt *first = NULL;
    Statement *statement = NULL;
    const char *p = (const char *)sql;
    int len = (int)n;
    while (len > 0) {
        sqlite3_stmt *stmt = NULL;
        int rc = next_statement(db, &p, &len, &stmt);
        if (rc != SQLITE_OK) {
            database_error(db, e, e_len, rc);
            sqlite3_finalize(stmt);
            goto finish;
        }
        if (!stmt) continue;
        if (first) {
            sqlite3_finalize(stmt);
            record_error(e, e_len, SQLITE_ERROR, "sqlite: prepare takes one statement");
            goto finish;
        }
        first = stmt;
    }
    if (!first) {
        record_error(e, e_len, SQLITE_ERROR, "sqlite: no statement");
        goto finish;
    }
    uint32_t columns = (uint32_t)sqlite3_column_count(first);
    statement = malloc(sizeof *statement + columns * sizeof (*statement->column_types));
    if (!statement) {
        database_error(NULL, e, e_len, SQLITE_NOMEM);
        goto finish;
    }
    statement->connection = connection;
    statement->stmt = first;
    statement->columns = columns;
    statement->row = 0;
    atomic_fetch_add_explicit(&connection->refs, 1, memory_order_relaxed);
    first = NULL;
finish:
    sqlite3_finalize(first);
    sqlite3_mutex_leave(mutex);
    return statement;
}

uint32_t hsq_column_count(void *handle) { return ((Statement *)handle)->columns; }

static int bind_range(uint32_t i, uint8_t *e, uint64_t e_len) {
    if (i == 0 || i > INT_MAX)
        return record_error(e, e_len, SQLITE_RANGE, "sqlite: parameter index out of range");
    return SQLITE_OK;
}

#define BIND(NAME, PARAMETERS, OPERATION) \
    int32_t hsq_bind_##NAME PARAMETERS { \
        int code = bind_range(i, e, e_len); \
        if (code != SQLITE_OK) return code; \
        Statement *s = handle; \
        sqlite3 *db = s->connection->db; \
        sqlite3_mutex *mutex = sqlite3_db_mutex(db); \
        sqlite3_mutex_enter(mutex); \
        code = (OPERATION); \
        if (code != SQLITE_OK) code = database_error(db, e, e_len, code); \
        sqlite3_mutex_leave(mutex); \
        return code; \
    }

BIND(i64, (void *handle, uint32_t i, int64_t v, uint8_t *e, uint64_t e_len),
     sqlite3_bind_int64(s->stmt, (int)i, v))
BIND(f64, (void *handle, uint32_t i, double v, uint8_t *e, uint64_t e_len),
     sqlite3_bind_double(s->stmt, (int)i, v))
BIND(blob, (void *handle, uint32_t i, const uint8_t *v, uint64_t n, uint8_t *e, uint64_t e_len),
     sqlite3_bind_blob64(s->stmt, (int)i, v, n, SQLITE_TRANSIENT))
BIND(text, (void *handle, uint32_t i, const uint8_t *v, uint64_t n, uint8_t *e, uint64_t e_len),
     sqlite3_bind_text64(s->stmt, (int)i, (const char *)v, n, SQLITE_TRANSIENT, SQLITE_UTF8))
BIND(null, (void *handle, uint32_t i, uint8_t *e, uint64_t e_len),
     sqlite3_bind_null(s->stmt, (int)i))

int32_t hsq_step(void *handle, uint8_t *e, uint64_t e_len) {
    Statement *s = handle;
    sqlite3 *db = s->connection->db;
    sqlite3_mutex *mutex = sqlite3_db_mutex(db);
    sqlite3_mutex_enter(mutex);
    int rc = sqlite3_step(s->stmt);
    if (rc == SQLITE_ROW && sqlite3_column_count(s->stmt) != (int)s->columns) {
        rc = record_error(e, e_len, SQLITE_SCHEMA, sqlite3_errstr(SQLITE_SCHEMA));
        s->row = 0;
        sqlite3_mutex_leave(mutex);
        return rc;
    }
    s->row = rc == SQLITE_ROW;
    if (s->row) {
        for (uint32_t i = 0; i < s->columns; ++i)
            s->column_types[i] = sqlite3_column_type(s->stmt, (int)i);
    } else if (rc != SQLITE_DONE) {
        rc = database_error(db, e, e_len, rc);
    }
    sqlite3_mutex_leave(mutex);
    return rc;
}

static int valid_column(const Statement *s, uint32_t i) {
    return s->row && i < s->columns;
}

int32_t hsq_column_kind(void *handle, uint32_t i) {
    Statement *s = handle;
    return valid_column(s, i) ? s->column_types[i] : SQLITE_NULL;
}

int64_t hsq_column_i64(void *handle, uint32_t i) {
    Statement *s = handle;
    return valid_column(s, i) ? sqlite3_column_int64(s->stmt, (int)i) : 0;
}

double hsq_column_f64(void *handle, uint32_t i) {
    Statement *s = handle;
    return valid_column(s, i) ? sqlite3_column_double(s->stmt, (int)i) : 0;
}

static int value_bytes(Statement *s, uint32_t i, const void **bytes, int *len) {
    if (!valid_column(s, i)) return SQLITE_MISUSE;
    int kind = s->column_types[i];
    *bytes = kind == SQLITE_TEXT
        ? sqlite3_column_text(s->stmt, (int)i)
        : sqlite3_column_blob(s->stmt, (int)i);
    *len = sqlite3_column_bytes(s->stmt, (int)i);
    if (sqlite3_errcode(s->connection->db) == SQLITE_NOMEM) return SQLITE_NOMEM;
    if (!*bytes && *len != 0) return SQLITE_NOMEM;
    return SQLITE_OK;
}

int64_t hsq_column_len(void *handle, uint32_t i, uint8_t *e, uint64_t e_len) {
    Statement *s = handle;
    sqlite3 *db = s->connection->db;
    sqlite3_mutex *mutex = sqlite3_db_mutex(db);
    sqlite3_mutex_enter(mutex);
    const void *bytes;
    int len;
    int rc = value_bytes(s, i, &bytes, &len);
    if (rc != SQLITE_OK) database_error(db, e, e_len, rc);
    sqlite3_mutex_leave(mutex);
    return rc == SQLITE_OK ? len : -1;
}

int32_t hsq_column_copy(void *handle, uint32_t i, uint8_t *out, uint64_t n,
                        uint64_t start, uint8_t *e, uint64_t e_len) {
    Statement *s = handle;
    sqlite3 *db = s->connection->db;
    sqlite3_mutex *mutex = sqlite3_db_mutex(db);
    sqlite3_mutex_enter(mutex);
    const void *bytes;
    int len;
    int rc = value_bytes(s, i, &bytes, &len);
    if (rc == SQLITE_OK && (start > n || (uint64_t)len != n - start)) rc = SQLITE_MISUSE;
    if (rc != SQLITE_OK) {
        rc = database_error(db, e, e_len, rc);
    } else if (len) {
        memcpy(out + start, bytes, (size_t)len);
    }
    sqlite3_mutex_leave(mutex);
    return rc;
}

int32_t hsq_reset(void *handle, uint8_t *e, uint64_t e_len) {
    Statement *s = handle;
    sqlite3 *db = s->connection->db;
    sqlite3_mutex *mutex = sqlite3_db_mutex(db);
    sqlite3_mutex_enter(mutex);
    int rc = sqlite3_reset(s->stmt);
    s->row = 0;
    if (rc != SQLITE_OK) rc = database_error(db, e, e_len, rc);
    sqlite3_mutex_leave(mutex);
    return rc;
}

int32_t hsq_finalize(void *handle) {
    Statement *s = handle;
    int rc = sqlite3_finalize(s->stmt);
    int closed = release_connection(s->connection);
    free(s);
    return rc == SQLITE_OK ? closed : rc;
}

int32_t hsq_close(void *handle) { return release_connection(handle); }

void hsq_errstr(int32_t code, uint8_t *e, uint64_t e_len) {
    record_error(e, e_len, code, sqlite3_errstr(code));
}
