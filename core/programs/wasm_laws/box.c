// The contained laws' C half (docs/spec/foreign.md, "Contained laws"). The
// functions whose operators a law counts are in box_traps.s.
#include "native.h"

__attribute__((import_module("wasi_snapshot_preview1"), import_name("clock_time_get")))
int32_t wasi_clock_time_get(int32_t id, uint64_t precision, uint64_t *time);
__attribute__((import_module("wasi_snapshot_preview1"), import_name("proc_exit")))
_Noreturn void wasi_proc_exit(int32_t code);

static uint64_t entries;
static uint32_t area_mode;
static _Alignas(8) uint8_t area[4096];
static uint64_t now;
static struct { uint64_t value; uint32_t used; } cells[8];

// Every export except loan_area counts its entry; box_traps.s calls this too.
void bump(void) { entries++; }

void guest_exit(int32_t code) { wasi_proc_exit(code); }

void *hwl_make(void) {
    bump();
    for (int i = 0; i < 8; i++) {
        if (!cells[i].used) {
            cells[i].used = 1;
            return &cells[i];
        }
    }
    return 0;
}

int32_t hwl_free(void *p) {
    bump();
    for (int i = 0; i < 8; i++) {
        if (p == &cells[i]) {
            cells[i].used = 0;
            return 0;
        }
    }
    return -1;
}

uint64_t hwl_live(void) {
    bump();
    uint64_t live = 0;
    for (int i = 0; i < 8; i++) live += cells[i].used;
    return live;
}

int32_t hwl_grow(uint32_t pages) {
    bump();
    return (int32_t)__builtin_wasm_memory_grow(0, pages);
}

uint64_t hwl_fill(uint8_t *b_ptr, uint32_t b_len) {
    bump();
    for (uint32_t i = 0; i < b_len; i++) ((volatile uint8_t *)b_ptr)[i] = 0xAB;
    return b_len;
}

uint64_t hwl_sum(const uint8_t *b_ptr, uint32_t b_len) {
    bump();
    uint64_t sum = 0;
    for (uint32_t i = 0; i < b_len; i++) sum += b_ptr[i];
    return sum;
}

uint64_t hwl_fill_trap(uint8_t *b_ptr, uint32_t b_len) {
    bump();
    for (uint32_t i = 0; i < b_len; i++) ((volatile uint8_t *)b_ptr)[i] = 0xAB;
    __builtin_trap();
}

int32_t hwl_clock(void) {
    bump();
    return wasi_clock_time_get(0, 1, &now);
}

uint64_t hwl_count(void) {
    bump();
    return entries;
}

int32_t hwl_set_area(uint32_t mode) {
    bump();
    area_mode = mode;
    return 0;
}

uint32_t hwl_loan_area(uint32_t n) {
    if (area_mode == 1) return (uint32_t)__builtin_wasm_memory_size(0) * 65536u - 8u;
    if (n > sizeof area) __builtin_trap();
    return (uint32_t)(uintptr_t)area;
}
