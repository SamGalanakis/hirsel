/* 10,000 sixteen-byte STDOUT writes to /dev/null from the root task, through the
   OS provider ("os") or through a provider that forwards every callback to it
   ("wrap"). The runtime's standing provider-cost row; measure-provider.py runs it. */
#include "hirsel_core_rt_host.h"
#include "hirsel_core_rt_tasks.h"
#include <assert.h>
#include <fcntl.h>
#include <string.h>
static HirselCoreProvider inner, wrapper, *selected;
static HirselCoreWait w_wait(void *s, HirselCoreHostRequest *r, uint64_t t) { (void)s; return inner.wait(inner.state, r, t); }
static HirselCoreStep w_issue(void *s, HirselCoreHostRequest *r, uint64_t t) { (void)s; return inner.issue(inner.state, r, t); }
static HirselCoreStep w_resume(void *s, HirselCoreHostRequest *r, uint64_t t) { (void)s; return inner.resume(inner.state, r, t); }
static HirselCoreHostOutcome w_abort(void *s, HirselCoreHostRequest *r, uint64_t t, uint8_t why) { (void)s; return inner.abort(inner.state, r, t, why); }
static HirselCoreHostOutcome w_deliver(void *s, HirselCoreHostRequest *r, HirselCoreHostOutcome o) { (void)s; return inner.deliver(inner.state, r, o); }
static uint8_t entry(HirselCoreContext *c, const HirselCoreProcessConfig *cfg, HirselCoreMainResult *out) {
    (void)cfg;
    static uint8_t bytes[16] = "0123456789abcdef";
    for (int i = 0; i < 10000; i++) {
        HirselCoreHostRequest req = {.operation = 12, .buffer = {bytes, 16, 16}, .site = 1};
        assert(hirsel_core_host(c, selected, &req).status == 0);
    }
    out->kind = 0; out->exit = 0; return 0;
}
int main(int argc, char **argv) {
    int wrapped = argc > 1 && strcmp(argv[1], "wrap") == 0;
    int null = open("/dev/null", O_WRONLY);
    HirselCoreOsConfig cfg = {.root_limit = 1 << 20, .grant_mask = 1 << 4, .stdout_fd = null, .stderr_fd = 2};
    HirselCoreOpenProcess p = hirsel_core_os_process_new(&cfg);
    assert(p.process);
    const HirselCoreProcessConfig *run = hirsel_core_os_process_config(p.process);
    inner = *run->provider;
    wrapper = inner;
    wrapper.state = 0;
    wrapper.wait = w_wait; wrapper.resume = w_resume; wrapper.abort = w_abort;
    wrapper.issue = w_issue; wrapper.deliver = w_deliver;
    selected = wrapped ? &wrapper : &inner;
    assert(hirsel_core_run(run, entry) == 0);
    hirsel_core_os_process_free(p.process);
    return 0;
}
