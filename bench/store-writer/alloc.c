/* glibc malloc interposition using its exported forwarding entry points.
 * Based on glibc malloc/malloc.c __libc_malloc, __libc_calloc, __libc_realloc
 * (04e750e7). Unlike a replacement allocator, this preserves glibc policy.
 * Counts successful underlying allocator requests, including reallocations.
 * Native small-object slab reuse does not issue an underlying request. */
#include <stddef.h>
#include <stdint.h>
#include <stdatomic.h>
extern void *__libc_malloc(size_t);
extern void *__libc_calloc(size_t, size_t);
extern void *__libc_realloc(void *, size_t);
_Atomic uint64_t store_bench_allocations;
void *malloc(size_t n) {
    void *p = __libc_malloc(n);
    if (p) atomic_fetch_add_explicit(&store_bench_allocations, 1, memory_order_relaxed);
    return p;
}
void *calloc(size_t n, size_t size) {
    void *p = __libc_calloc(n, size);
    if (p) atomic_fetch_add_explicit(&store_bench_allocations, 1, memory_order_relaxed);
    return p;
}
void *realloc(void *old, size_t n) {
    void *p = __libc_realloc(old, n);
    if (p) atomic_fetch_add_explicit(&store_bench_allocations, 1, memory_order_relaxed);
    return p;
}
#include <unistd.h>
#include <sys/syscall.h>
_Atomic uint64_t store_bench_fsyncs;
int fsync(int fd) {
    int result = (int)syscall(SYS_fsync, fd);
    if (!result) atomic_fetch_add_explicit(&store_bench_fsyncs, 1, memory_order_relaxed);
    return result;
}
int fdatasync(int fd) {
    int result = (int)syscall(SYS_fdatasync, fd);
    if (!result) atomic_fetch_add_explicit(&store_bench_fsyncs, 1, memory_order_relaxed);
    return result;
}
#include <fcntl.h>
_Atomic uint64_t store_bench_puts;
int linkat(int olddir, const char *old, int newdir, const char *name, int flags) {
    int result = (int)syscall(SYS_linkat, olddir, old, newdir, name, flags);
    if (!result) atomic_fetch_add_explicit(&store_bench_puts, 1, memory_order_relaxed);
    return result;
}
