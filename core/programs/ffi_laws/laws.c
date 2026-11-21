#define _POSIX_C_SOURCE 200809L
#include "native.h"

#include <errno.h>
#include <pthread.h>
#include <semaphore.h>
#include <stdbool.h>
#include <stdlib.h>
#include <unistd.h>

HIRSEL_ABI_TEXT

static pthread_mutex_t pool_lock = PTHREAD_MUTEX_INITIALIZER;
static uint8_t cells[8];
static bool used[8];
static uint64_t made;
static uint64_t freed;
static uint64_t held;
static bool gated;
static pthread_once_t semaphore_once = PTHREAD_ONCE_INIT;
static sem_t semaphore;

static void initialize_semaphore(void) {
    if (sem_init(&semaphore, 0, 0) != 0) abort();
}

static void await_open(void) {
    pthread_once(&semaphore_once, initialize_semaphore);
    while (sem_wait(&semaphore) != 0) {
        if (errno != EINTR) abort();
    }
}

void *hfl_make(void) {
    uint8_t *cell = NULL;
    pthread_mutex_lock(&pool_lock);
    for (size_t i = 0; i < sizeof cells; ++i) {
        if (!used[i]) {
            used[i] = true;
            cells[i] = (uint8_t)++made;
            cell = &cells[i];
            break;
        }
    }
    pthread_mutex_unlock(&pool_lock);
    return cell;
}

void *hfl_make_w(void) { return hfl_make(); }

static void report_free(uint8_t number) {
    char line[11] = "freed ";
    size_t n = 6;
    if (number >= 100) line[n++] = '0' + number / 100;
    if (number >= 10) line[n++] = '0' + number / 10 % 10;
    line[n++] = '0' + number % 10;
    line[n++] = '\n';
    if (write(STDERR_FILENO, line, n) != (ssize_t)n) abort();
}

int32_t hfl_free(void *p) {
    pthread_mutex_lock(&pool_lock);
    size_t i = (uint8_t *)p - cells;
    if (i >= sizeof cells || !used[i]) abort();
    report_free(cells[i]);
    used[i] = false;
    ++freed;
    bool wait = gated;
    if (wait) ++held;
    pthread_mutex_unlock(&pool_lock);
    if (wait) await_open();
    return 0;
}

uint64_t hfl_freed(void) {
    pthread_mutex_lock(&pool_lock);
    uint64_t count = freed;
    pthread_mutex_unlock(&pool_lock);
    return count;
}

uint64_t hfl_made(void) {
    pthread_mutex_lock(&pool_lock);
    uint64_t count = made;
    pthread_mutex_unlock(&pool_lock);
    return count;
}

void *hfl_same(void *p) { return p; }

uint64_t hfl_mark(uint8_t *b_ptr, uint64_t b_len) {
    if (b_ptr == NULL) abort();
    for (uint64_t i = 0; i < b_len; ++i) b_ptr[i] = 0xab;
    return b_len;
}

uint64_t hfl_mark_w(uint8_t *b_ptr, uint64_t b_len) {
    return hfl_mark(b_ptr, b_len);
}

uint64_t hfl_loan_length(const uint8_t *b_ptr, uint64_t b_len) {
    if (b_ptr == NULL) abort();
    return b_len;
}

uint64_t hfl_loan_sum(const uint8_t *b_ptr, uint64_t b_len) {
    uint64_t sum = 0;
    for (uint64_t i = 0; i < b_len; ++i) sum += b_ptr[i];
    return sum;
}

int32_t hfl_gate(uint32_t on) {
    pthread_mutex_lock(&pool_lock);
    gated = on != 0;
    pthread_mutex_unlock(&pool_lock);
    return 0;
}

int32_t hfl_touch(void) {
    static const char line[] = "touched\n";
    if (write(STDERR_FILENO, line, sizeof line - 1) != sizeof line - 1) abort();
    return 0;
}

int32_t hfl_hold(void) {
    pthread_mutex_lock(&pool_lock);
    ++held;
    pthread_mutex_unlock(&pool_lock);
    await_open();
    return 0;
}

uint64_t hfl_held(void) {
    pthread_mutex_lock(&pool_lock);
    uint64_t count = held;
    pthread_mutex_unlock(&pool_lock);
    return count;
}

int32_t hfl_open(uint32_t n) {
    pthread_once(&semaphore_once, initialize_semaphore);
    for (uint32_t i = 0; i < n; ++i) {
        if (sem_post(&semaphore) != 0) abort();
    }
    return 0;
}
