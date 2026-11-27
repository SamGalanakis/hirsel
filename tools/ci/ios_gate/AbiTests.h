#include <stddef.h>
#include "Memory.h"
#include <stdint.h>

int32_t hirsel_embed_harness_manifest_fp(const uint8_t *manifest, size_t length, uint8_t *out);
ptrdiff_t hirsel_embed_harness_welcome_target(uint64_t generation, uint8_t *out, size_t capacity);
uint32_t hirsel_gate_workers(void);
void hirsel_embed_harness_measure_fibers(uint32_t enable);
uint64_t hirsel_embed_harness_live_fibers(void);
