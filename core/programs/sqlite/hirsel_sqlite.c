#define _POSIX_C_SOURCE 200809L
#include "native.h"
#include "sqlite3.h"

#include <limits.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>

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

typedef struct {
    sqlite3 *db;
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
    free(connection);
    return code;
}

void *hsq_open_memory(uint8_t *e, uint64_t e_len) {
    pthread_once(&init_once, initialize);
    if (init_code != SQLITE_OK) {
        database_error(NULL, e, e_len, init_code);
        return NULL;
    }
    sqlite3 *db = NULL;
    int code = sqlite3_open_v2(":memory:", &db,
        SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_EXRESCODE, "hirsel-none");
    if (code == SQLITE_OK) code = sqlite3_set_authorizer(db, authorize, NULL);
    if (code != SQLITE_OK) {
        database_error(db, e, e_len, code);
        if (db) sqlite3_close(db);
        return NULL;
    }
    Connection *connection = malloc(sizeof *connection);
    if (!connection) {
        database_error(NULL, e, e_len, SQLITE_NOMEM);
        sqlite3_close(db);
        return NULL;
    }
    connection->db = db;
    atomic_init(&connection->refs, 1);
    return connection;
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
