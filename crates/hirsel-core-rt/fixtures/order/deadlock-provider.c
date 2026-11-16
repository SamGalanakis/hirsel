/* The stdout operation remains in I with no timer until another host thread
   readies it. The evaluator's immediate stdout has the same observable result. */
#include "hirsel_core_rt.h"
#include <pthread.h>
#include <stdlib.h>
#include <time.h>
static HirselCoreProvider original;
static pthread_t worker;
static uint64_t pending_token;
static void *ready_later(void *unused) {
    (void)unused;
    struct timespec delay={0,20000000};
    nanosleep(&delay,NULL);
    hirsel_core_ready(pending_token);
    return NULL;
}
static HirselCoreWait wait_ready(void *s,HirselCoreHostRequest *r,uint64_t t) {
    (void)s;(void)r;(void)t;
    return (HirselCoreWait){0,0};
}
static HirselCoreStep issue(void *s,HirselCoreHostRequest *r,uint64_t t) {
    (void)s;(void)r;
    pending_token=t;
    if(pthread_create(&worker,NULL,ready_later,NULL)) abort();
    return (HirselCoreStep){.kind=1};
}
static HirselCoreStep resume(void *s,HirselCoreHostRequest *r,uint64_t t) {
    (void)s;
    if(pthread_join(worker,NULL)) abort();
    return original.issue(original.state,r,t);
}
static HirselCoreHostOutcome abort_op(void *s,HirselCoreHostRequest *r,uint64_t t,uint8_t why) {
    (void)s;(void)r;(void)t;(void)why; abort();
}
static HirselCoreHostOutcome deliver(void *s,HirselCoreHostRequest *r,HirselCoreHostOutcome o) {
    (void)s;
    return original.deliver(original.state,r,o);
}
void pending_stdout(HirselCoreProcessConfig *cfg) {
    HirselCoreEntryGrant *grants=malloc(cfg->capabilities_len*sizeof(*grants));
    if(!grants) abort();
    static const HirselCoreProvider provider={NULL,wait_ready,issue,resume,abort_op,deliver};
    for(size_t i=0;i<cfg->capabilities_len;i++) {
        grants[i]=cfg->capabilities[i];
        if(grants[i].kind==4) {
            original=*grants[i].provider;
            grants[i].provider=&provider;
        }
    }
    cfg->capabilities=grants;
}
