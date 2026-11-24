#define _GNU_SOURCE
#include "native.h"
#include "sqlite3.h"

#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/wait.h>
#include <unistd.h>

enum { WAL_REGION = 32768, DEADMAN = 128 };
static const uint8_t leaf[] = "law.db";
static const char sidecar[] = "law.db-shm";
static uint8_t error[260];
static sqlite3 *opened;

static int capture_db(sqlite3 *db, char **message, const sqlite3_api_routines *api) {
    (void)message; (void)api;
    opened = db;
    return SQLITE_OK;
}

static void *open_directory(int dir) {
    int fd = dup(dir);
    assert(fd >= 0);
    void *handle = hsq_open(fd, 1, leaf, sizeof leaf - 1, error, sizeof error);
    assert(handle && opened);
    return handle;
}

static sqlite3_file *database_file(void) {
    sqlite3_file *file = NULL;
    assert(sqlite3_file_control(opened, "main", SQLITE_FCNTL_FILE_POINTER, &file) == SQLITE_OK);
    return file;
}

static struct flock deadman(short type) {
    return (struct flock){.l_type = type, .l_whence = SEEK_SET, .l_start = DEADMAN, .l_len = 1};
}

static void signal_peer(int fd) {
    assert(write(fd, "!", 1) == 1);
}

static void wait_for_peer(int fd) {
    char c;
    assert(read(fd, &c, 1) == 1);
}

static int release_peer = -1;
static int peer_released = -1;
extern int __real_fcntl(int fd, int command, ...);

int __wrap_fcntl(int fd, int command, ...) {
    assert(command == F_OFD_GETLK || command == F_OFD_SETLK);
    va_list args;
    va_start(args, command);
    struct flock *lock = va_arg(args, struct flock *);
    va_end(args);
    short requested = lock->l_type;
    int rc = __real_fcntl(fd, command, lock);
    int saved_errno = errno;
    if (release_peer >= 0 && lock->l_start == DEADMAN && requested == F_WRLCK &&
        ((command == F_OFD_GETLK && rc == 0 && lock->l_type == F_WRLCK) ||
         (command == F_OFD_SETLK && rc == -1 && saved_errno == EAGAIN))) {
        // Release the peer after the kernel reports exclusive contention.
        signal_peer(release_peer);
        wait_for_peer(peer_released);
        release_peer = -1;
    }
    errno = saved_errno;
    return rc;
}

static void lock_case(int dir, sqlite3_file *file, short type) {
    int fd = openat(dir, sidecar, O_CREAT | O_RDWR | O_TRUNC, 0600);
    assert(fd >= 0);
    if (type != F_WRLCK) assert(write(fd, "index", 5) == 5);
    assert(close(fd) == 0);

    int to_peer[2], from_peer[2];
    assert(pipe(to_peer) == 0 && pipe(from_peer) == 0);
    pid_t peer = fork();
    assert(peer >= 0);
    if (peer == 0) {
        close(to_peer[1]);
        close(from_peer[0]);
        fd = openat(dir, sidecar, O_RDWR);
        assert(fd >= 0);
        struct flock lock = deadman(type);
        assert(__real_fcntl(fd, F_OFD_SETLK, &lock) == 0);
        signal_peer(from_peer[1]);
        wait_for_peer(to_peer[0]);
        assert(close(fd) == 0);
        signal_peer(from_peer[1]);
        _exit(0);
    }
    close(to_peer[0]);
    close(from_peer[1]);
    wait_for_peer(from_peer[0]);
    if (type == F_WRLCK) {
        release_peer = to_peer[1];
        peer_released = from_peer[0];
    }
    void volatile *page = NULL;
    int rc = file->pMethods->xShmMap(file, 0, WAL_REGION, 0, &page);
    if (type != F_WRLCK || release_peer >= 0) {
        signal_peer(to_peer[1]);
        wait_for_peer(from_peer[0]);
    }
    release_peer = -1;
    close(to_peer[1]);
    close(from_peer[0]);
    int status;
    assert(waitpid(peer, &status, 0) == peer && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(page == NULL);
    if (type == F_WRLCK) {
        assert(rc == SQLITE_BUSY);
        assert(file->pMethods->xShmMap(file, 0, WAL_REGION, 0, &page) == SQLITE_OK);
    } else {
        assert(rc == SQLITE_OK);
    }
    fd = openat(dir, sidecar, O_RDWR);
    assert(fd >= 0);
    struct flock lock = deadman(F_WRLCK);
    assert(__real_fcntl(fd, F_OFD_GETLK, &lock) == 0 && lock.l_type == F_RDLCK);
    struct stat st;
    assert(fstat(fd, &st) == 0 && st.st_size == (type == F_RDLCK ? 5 : 0));
    assert(close(fd) == 0);
    assert(file->pMethods->xShmUnmap(file, 1) == SQLITE_OK);
}

static void deadman_openers(int dir) {
    void *handle = open_directory(dir);
    sqlite3_file *file = database_file();
    lock_case(dir, file, F_UNLCK);
    lock_case(dir, file, F_RDLCK);
    lock_case(dir, file, F_WRLCK);
    assert(hsq_close(handle) == SQLITE_OK);
}

static void allocation(int dir) {
    void *handle = open_directory(dir);
    sqlite3_file *file = database_file();
    struct statvfs fs;
    assert(fstatvfs(dir, &fs) == 0);
    assert(fs.f_bavail * fs.f_frsize >= WAL_REGION && fs.f_bavail * fs.f_frsize <= 65536);
    int filler = openat(dir, "filler", O_CREAT | O_EXCL | O_WRONLY, 0600);
    assert(filler >= 0);
    assert(fallocate(filler, 0, 0, fs.f_bavail * fs.f_frsize - WAL_REGION / 2) == 0);

    void volatile *page = NULL;
    assert(file->pMethods->xShmMap(file, 0, WAL_REGION, 1, &page) == SQLITE_IOERR_SHMSIZE);
    assert(page == NULL);
    struct stat st;
    assert(fstatat(dir, sidecar, &st, 0) == 0);
    assert(st.st_size > 0 && st.st_size < WAL_REGION && st.st_size == st.st_blocks * 512);

    assert(close(filler) == 0 && unlinkat(dir, "filler", 0) == 0);
    assert(file->pMethods->xShmMap(file, 0, WAL_REGION, 1, &page) == SQLITE_OK && page);
    for (int offset = 0; offset < WAL_REGION; offset += 4096)
        ((volatile unsigned char *)page)[offset] = 1;
    assert(file->pMethods->xShmUnmap(file, 1) == SQLITE_OK);
    assert(hsq_close(handle) == SQLITE_OK);
}

static void check_trusted_schema(void *handle) {
    assert(handle && opened);
    int trusted = -1;
    assert(sqlite3_db_config(opened, SQLITE_DBCONFIG_TRUSTED_SCHEMA, -1, &trusted) == SQLITE_OK);
    assert(trusted == 0);
    assert(hsq_close(handle) == SQLITE_OK);
}

static void trusted_schema_memory(int dir) {
    (void)dir;
    check_trusted_schema(hsq_open_memory(error, sizeof error));
}

static void trusted_schema_directory(int dir) {
    check_trusted_schema(open_directory(dir));
}

static void persistent_statements(int dir) {
    (void)dir;
    void *handle = hsq_open_memory(error, sizeof error);
    assert(handle && opened);
    static const uint8_t sql[] = "select 1";
    int hits, high;
    assert(sqlite3_db_status(opened, SQLITE_DBSTATUS_LOOKASIDE_HIT, &hits, &high, 1) == SQLITE_OK);
    void *statement = hsq_prepare(handle, sql, sizeof sql - 1, error, sizeof error);
    assert(statement);
    assert(sqlite3_db_status(opened, SQLITE_DBSTATUS_LOOKASIDE_HIT, &hits, &high, 1) == SQLITE_OK);
    assert(high == 0);
    assert(hsq_finalize(statement) == SQLITE_OK);
    assert(hsq_exec(handle, sql, sizeof sql - 1, error, sizeof error) == SQLITE_OK);
    assert(sqlite3_db_status(opened, SQLITE_DBSTATUS_LOOKASIDE_HIT, &hits, &high, 0) == SQLITE_OK);
    assert(high > 0);
    assert(hsq_close(handle) == SQLITE_OK);
}

static void powersafe_overwrite(int dir) {
    void *handle = open_directory(dir);
    sqlite3_file *file = database_file();
    assert((file->pMethods->xDeviceCharacteristics(file) & SQLITE_IOCAP_POWERSAFE_OVERWRITE) == 0);
    assert(hsq_close(handle) == SQLITE_OK);
}

static void wal_sleep(int dir) {
    void *handle = open_directory(dir);
    sqlite3_vfs *vfs = NULL;
    assert(sqlite3_file_control(opened, "main", SQLITE_FCNTL_VFS_POINTER, &vfs) == SQLITE_OK);
    assert(vfs->xSleep(vfs, 1000) == 1000);
    sqlite3_vfs *memory = sqlite3_vfs_find("hirsel-none");
    assert(memory && memory->xSleep(memory, 1000) == 0);
    static const uint8_t sql[] = "pragma busy_timeout = 1000";
    assert(hsq_exec(handle, sql, sizeof sql - 1, error, sizeof error) == SQLITE_AUTH);
    static const uint8_t query[] = "pragma busy_timeout";
    assert(hsq_exec(handle, query, sizeof query - 1, error, sizeof error) == SQLITE_OK);
    assert(hsq_close(handle) == SQLITE_OK);
}

int main(void) {
    static const struct {
        const char *name;
        void (*check)(int);
    } cases[] = {
        {"allocation", allocation},
        {"deadman_openers", deadman_openers},
        {"trusted_schema_memory", trusted_schema_memory},
        {"trusted_schema_directory", trusted_schema_directory},
        {"persistent_statements", persistent_statements},
        {"powersafe_overwrite", powersafe_overwrite},
        {"wal_sleep", wal_sleep},
    };
    void *memory = hsq_open_memory(error, sizeof error);
    assert(memory && hsq_close(memory) == SQLITE_OK);
    assert(sqlite3_auto_extension((void (*)(void))capture_db) == SQLITE_OK);
    int dir = open(".", O_DIRECTORY | O_RDONLY);
    assert(dir >= 0);
    int failures = 0;
    for (size_t i = 0; i < sizeof cases / sizeof cases[0]; ++i) {
        pid_t child = fork();
        assert(child >= 0);
        if (child == 0) {
            cases[i].check(dir);
            _exit(0);
        }
        int status;
        assert(waitpid(child, &status, 0) == child);
        int passed = WIFEXITED(status) && WEXITSTATUS(status) == 0;
        printf("%s %s\n", cases[i].name, passed ? "PASS" : "FAIL");
        fflush(stdout);
        failures += !passed;
        assert(unlinkat(dir, (const char *)leaf, 0) == 0 || errno == ENOENT);
        assert(unlinkat(dir, sidecar, 0) == 0 || errno == ENOENT);
        assert(unlinkat(dir, "filler", 0) == 0 || errno == ENOENT);
    }
    assert(close(dir) == 0);
    return failures ? 1 : 0;
}
