#include "hirsel_core_rt.h"
#include "hirsel_core_rt_host.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>
static void checkpoint(const char *s) {
    hirsel_core_memory_checkpoint((const uint8_t *)s, strlen(s));
}
static uint8_t entry(HirselCoreContext *ctx, const HirselCoreProcessConfig *cfg,
                     HirselCoreMainResult *result) {
    (void)cfg;
    void *region = hirsel_core_region_open(128);
    HirselCoreAllocation value = hirsel_core_region_alloc(ctx, region, 8, 8, 8, false);
    assert(value.pointer);
    checkpoint("allocated\n\"λ\"");
    hirsel_core_region_lower_charge(region, 0, 8, 0);
    checkpoint("trimmed");
    hirsel_core_region_close(region);
    checkpoint("closed");
    result->kind=0; result->exit=0;
    return 0;
}
static void diagnostic(void *state, const uint8_t *data, size_t len) {
    (void)state; fwrite(data, 1, len, stderr);
}
int main(void) {
    HirselCoreProcessConfig cfg = {.root_limit=4096, .diagnostic=diagnostic};
    assert(hirsel_core_run(&cfg, entry)==0);
    size_t len;
    const uint8_t *report = hirsel_core_memory_report(&len);
    assert(fwrite(report,1,len,stdout)==len);
    return 0;
}
