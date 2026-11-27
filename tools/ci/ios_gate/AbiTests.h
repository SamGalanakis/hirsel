#include <stddef.h>
#include "GateSymbols.h"
#include "Memory.h"
#include <stdint.h>

#define HIRSEL_HARNESS(name) __asm__(HIRSEL_HARNESS_PREFIX #name)

// These link to the archive's private, build-specific symbols. EMB-4 stays unchanged.
int32_t hirsel_embed_harness_manifest_fp(const uint8_t *manifest, size_t length, uint8_t *out)
    HIRSEL_HARNESS(hirsel_embed_harness_manifest_fp);
ptrdiff_t hirsel_embed_harness_welcome_target(uint64_t generation, uint8_t *out, size_t capacity)
    HIRSEL_HARNESS(hirsel_embed_harness_welcome_target);
uint32_t hirsel_gate_workers(void);
void hirsel_embed_harness_measure_fibers(uint32_t enable)
    HIRSEL_HARNESS(hirsel_embed_harness_measure_fibers);
uint64_t hirsel_embed_harness_live_fibers(void)
    HIRSEL_HARNESS(hirsel_embed_harness_live_fibers);
