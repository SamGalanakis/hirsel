/* Native link-wrapped fault harness for Slice 2s laws:
 *   -Wl,--wrap=exit,--wrap=_exit,--wrap=mprotect,--wrap=munmap,--wrap=malloc
 *
 * Verifies zero calls to exit/_exit across startup refusals, guard faults,
 * refused stack growth (both growth-fault and fiber::room pre-commit),
 * whole-region munmap of retired stacks, lock-held malloc refusal,
 * CountOverflow, Deadlock, host stop, and replaced fault handler refusal.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

extern void __real_exit(int status) __attribute__((noreturn));
extern void __real__exit(int status) __attribute__((noreturn));
extern int __real_mprotect(void *addr, size_t len, int prot);
extern int __real_munmap(void *addr, size_t len);
extern void *__real_malloc(size_t size);

/* Armed around every Hirsel call; any exit/_exit aborts the test immediately. */
static _Atomic int g_no_exit = 0;

/* Targeted mprotect failure inside a specific stack reservation [base, base+len). */
static _Atomic int g_fail_mprotect = 0;
static _Atomic uintptr_t g_fail_stack_base = 0;
static _Atomic uintptr_t g_fail_stack_end = 0;

/* Targeted malloc failure (next allocation of size == 64 returns NULL). */
static _Atomic int g_fail_malloc = 0;

/* Lock-free ring recording munmap(addr, len) calls for whole-region check. */
#define MUNMAP_CAP 256
static _Atomic uintptr_t g_munmap_addr[MUNMAP_CAP];
static _Atomic size_t g_munmap_len[MUNMAP_CAP];
static _Atomic size_t g_munmap_count = 0;

void __wrap_exit(int status) {
    if (atomic_load_explicit(&g_no_exit, memory_order_acquire)) {
        const char msg[] = "FORBIDDEN: exit() called during embedded Hirsel run\n";
        (void)write(2, msg, sizeof(msg) - 1);
        abort();
    }
    __real_exit(status);
}

void __wrap__exit(int status) {
    if (atomic_load_explicit(&g_no_exit, memory_order_acquire)) {
        const char msg[] = "FORBIDDEN: _exit() called during embedded Hirsel run\n";
        (void)write(2, msg, sizeof(msg) - 1);
        abort();
    }
    __real__exit(status);
}

int __wrap_mprotect(void *addr, size_t len, int prot) {
    if ((prot & (PROT_READ | PROT_WRITE)) == (PROT_READ | PROT_WRITE) &&
        atomic_load_explicit(&g_fail_mprotect, memory_order_acquire) != 0) {
        uintptr_t p = (uintptr_t)addr;
        uintptr_t base = atomic_load_explicit(&g_fail_stack_base, memory_order_acquire);
        uintptr_t end = atomic_load_explicit(&g_fail_stack_end, memory_order_acquire);
        if (p >= base && p < end) {
            int expected = 1;
            if (atomic_compare_exchange_strong_explicit(
                    &g_fail_mprotect, &expected, 0,
                    memory_order_acq_rel, memory_order_acquire)) {
                errno = ENOMEM;
                return -1;
            }
        }
    }
    return __real_mprotect(addr, len, prot);
}

int __wrap_munmap(void *addr, size_t len) {
    size_t idx = atomic_fetch_add_explicit(&g_munmap_count, 1, memory_order_acq_rel);
    if (idx < MUNMAP_CAP) {
        atomic_store_explicit(&g_munmap_addr[idx], (uintptr_t)addr, memory_order_release);
        atomic_store_explicit(&g_munmap_len[idx], len, memory_order_release);
    }
    return __real_munmap(addr, len);
}

void *__wrap_malloc(size_t size) {
    if (size == 64 && atomic_load_explicit(&g_fail_malloc, memory_order_acquire) != 0) {
        int expected = 1;
        if (atomic_compare_exchange_strong_explicit(
                &g_fail_malloc, &expected, 0,
                memory_order_acq_rel, memory_order_acquire)) {
            errno = ENOMEM;
            return NULL;
        }
    }
    return __real_malloc(size);
}

/* Hooks exported to embed_stop_fixture.rs */
void wrap_arm_fail_next_mprotect(uintptr_t base, size_t len) {
    atomic_store_explicit(&g_fail_stack_base, base, memory_order_release);
    atomic_store_explicit(&g_fail_stack_end, base + len, memory_order_release);
    atomic_store_explicit(&g_fail_mprotect, 1, memory_order_release);
}

void wrap_disarm_fail_mprotect(void) {
    atomic_store_explicit(&g_fail_mprotect, 0, memory_order_release);
}

void wrap_arm_fail_next_malloc(void) {
    atomic_store_explicit(&g_fail_malloc, 1, memory_order_release);
}

void wrap_disarm_fail_malloc(void) {
    atomic_store_explicit(&g_fail_malloc, 0, memory_order_release);
}

void wrap_reset_munmap_log(void) {
    atomic_store_explicit(&g_munmap_count, 0, memory_order_release);
}

int wrap_saw_whole_munmap(uintptr_t base, size_t len) {
    size_t count = atomic_load_explicit(&g_munmap_count, memory_order_acquire);
    if (count > MUNMAP_CAP) {
        count = MUNMAP_CAP;
    }
    for (size_t i = 0; i < count; i++) {
        uintptr_t a = atomic_load_explicit(&g_munmap_addr[i], memory_order_acquire);
        size_t l = atomic_load_explicit(&g_munmap_len[i], memory_order_acquire);
        if (a == base && l == len) {
            return 1;
        }
    }
    return 0;
}

extern int embed_fixture_isa_denied(char *name_buf, size_t cap);
extern int embed_fixture_guard_fault(char *name_buf, size_t cap);
extern int embed_fixture_refused_growth_fault(char *name_buf, size_t cap);
extern int embed_fixture_refused_growth_room(char *name_buf, size_t cap);
extern int embed_fixture_lock_held_malloc_refusal(char *name_buf, size_t cap);
extern int embed_fixture_count_overflow(char *name_buf, size_t cap);
extern int embed_fixture_deadlock(char *name_buf, size_t cap);
extern int embed_fixture_host_stop(void);
extern int embed_fixture_replaced_handler(char *name_buf, size_t cap);

static void check_stop(const char *label, int rc, const char *got, const char *want) {
    if (rc != 1 || strcmp(got, want) != 0) {
        fprintf(stderr, "FAIL %s: rc=%d got='%s' want='%s'\n", label, rc, got, want);
        atomic_store_explicit(&g_no_exit, 0, memory_order_release);
        __real__exit(1);
    }
}

int main(void) {
    char name[128];

    /* Arm the exit/_exit trap across EVERY embedded scenario below. */
    atomic_store_explicit(&g_no_exit, 1, memory_order_release);

    memset(name, 0, sizeof(name));
    check_stop("isa_denied", embed_fixture_isa_denied(name, sizeof(name)), name, "UnsupportedCpu");

    memset(name, 0, sizeof(name));
    check_stop("guard_fault", embed_fixture_guard_fault(name, sizeof(name)), name, "Stack");

    memset(name, 0, sizeof(name));
    check_stop("refused_growth_fault", embed_fixture_refused_growth_fault(name, sizeof(name)), name, "HostMemory");

    memset(name, 0, sizeof(name));
    check_stop("refused_growth_room", embed_fixture_refused_growth_room(name, sizeof(name)), name, "HostMemory");

    memset(name, 0, sizeof(name));
    check_stop("lock_held_malloc_refusal", embed_fixture_lock_held_malloc_refusal(name, sizeof(name)), name, "HostMemory");

    memset(name, 0, sizeof(name));
    check_stop("count_overflow", embed_fixture_count_overflow(name, sizeof(name)), name, "CountOverflow");

    memset(name, 0, sizeof(name));
    check_stop("deadlock", embed_fixture_deadlock(name, sizeof(name)), name, "Deadlock");

    int hs = embed_fixture_host_stop();
    if (hs != 1) {
        fprintf(stderr, "FAIL host_stop: rc=%d\n", hs);
        atomic_store_explicit(&g_no_exit, 0, memory_order_release);
        __real__exit(1);
    }

    memset(name, 0, sizeof(name));
    check_stop("replaced_handler", embed_fixture_replaced_handler(name, sizeof(name)), name, "FaultHandlerReplaced");

    /* Live-host completion marker: proves none of the scenarios exited. */
    atomic_store_explicit(&g_no_exit, 0, memory_order_release);
    fputs("EMBED_STOP_WRAP_OK\n", stdout);
    fflush(stdout);
    return 0;
}
