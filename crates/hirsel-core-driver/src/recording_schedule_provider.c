/* Engine-agreement oracle: scripted clock, deterministic ready notifications,
   and an adversarial chooser. Only the recording run uses this provider. */
#include "hirsel_core_rt_sites.h"
#include <stdlib.h>
#include <string.h>
static uint64_t waits[16];
static size_t waited;
static HirselCoreWait script_wait(void *s,HirselCoreHostRequest *r,uint64_t token) {
    (void)s;
    if(r->operation!=15)return (HirselCoreWait){0};
    for(size_t i=0;i<waited;i++)if(waits[i]==token)return (HirselCoreWait){0};
    if(waited==16)abort();
    waits[waited++]=token;
    hirsel_core_ready(token);
    return (HirselCoreWait){.kind=2};
}
static HirselCoreStep script_issue(void *s,HirselCoreHostRequest *r,uint64_t token) {
    (void)s;
    if(r->operation==15) {
        hirsel_core_ready(token);
        return (HirselCoreStep){.kind=1};
    }
    return (HirselCoreStep){.outcome={.value=1000}};
}
static HirselCoreStep script_resume(void *s,HirselCoreHostRequest *r,uint64_t token) {
    (void)s;(void)r;(void)token;
    return (HirselCoreStep){0};
}
static HirselCoreHostOutcome script_abort(void *s,HirselCoreHostRequest *r,uint64_t token,uint8_t why) {
    (void)s;(void)r;(void)token;(void)why;
    return (HirselCoreHostOutcome){0};
}
static HirselCoreHostOutcome script_deliver(void *s,HirselCoreHostRequest *r,HirselCoreHostOutcome o) {
    (void)s;(void)r;return o;
}
extern uint8_t hirsel_core_test_run_last(const HirselCoreProcessConfig *,HirselCoreEntry);
extern uint8_t __real_hirsel_core_run(const HirselCoreProcessConfig *,HirselCoreEntry);
static int in_wrap;
uint8_t __wrap_hirsel_core_run(const HirselCoreProcessConfig *input,HirselCoreEntry entry) {
    if(in_wrap || !getenv("HIRSEL_RECORD_SCHEDULE"))return __real_hirsel_core_run(input,entry);
    HirselCoreProcessConfig cfg=*input;
    HirselCoreEntryGrant grants[16];
    if(cfg.capabilities_len>16)abort();
    memcpy(grants,cfg.capabilities,cfg.capabilities_len*sizeof *grants);
    HirselCoreProvider script={0,script_wait,script_issue,script_resume,script_abort,script_deliver};
    for(size_t i=0;i<cfg.capabilities_len;i++)if(grants[i].kind==2)grants[i].provider=&script;
    cfg.capabilities=grants;
    in_wrap=1;
    uint8_t status=hirsel_core_test_run_last(&cfg,entry);
    in_wrap=0;
    return status;
}
