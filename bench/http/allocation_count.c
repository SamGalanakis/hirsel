// Linux/glibc process-local allocation call counter for the soak only.
#include <errno.h>
#include <fcntl.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <unistd.h>

extern void *__libc_malloc(size_t);
extern void *__libc_calloc(size_t, size_t);
extern void *__libc_realloc(void *, size_t);
extern void *__libc_memalign(size_t, size_t);

static _Atomic uint64_t *calls;

__attribute__((constructor)) static void start_counter(void) {
    const char *path = getenv("HTTP_BENCH_C_ALLOC");
    if (!path) return;
    int fd = open(path, O_RDWR | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
    if (fd < 0 || ftruncate(fd, sizeof(*calls)) < 0) _exit(81);
    void *mapping = mmap(NULL, sizeof(*calls), PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (mapping == MAP_FAILED) _exit(82);
    calls = mapping;
}

static void *observe(void *ptr) {
    if (ptr && calls) atomic_fetch_add_explicit(calls, 1, memory_order_relaxed);
    return ptr;
}

void *malloc(size_t size) {
    return observe(__libc_malloc(size));
}

void *calloc(size_t count, size_t size) {
    return observe(__libc_calloc(count, size));
}

void *realloc(void *ptr, size_t size) {
    return observe(__libc_realloc(ptr, size));
}

void *aligned_alloc(size_t alignment, size_t size) {
    return observe(__libc_memalign(alignment, size));
}

int posix_memalign(void **ptr, size_t alignment, size_t size) {
    if (alignment < sizeof(void *) || alignment & (alignment - 1)) return EINVAL;
    int saved_errno = errno;
    void *resource = observe(__libc_memalign(alignment, size));
    errno = saved_errno;
    if (!resource) return ENOMEM;
    *ptr = resource;
    return 0;
}
