/* Compiled store crash tests. Durable state belongs to the existing Rust W2
 * FileSystem; this shim adapts the published provider ABI and runs three phases. */
#include "hirsel_core_rt_host.h"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

extern void *store_crash_new(uint64_t);
extern void store_crash_phase(void *, uint64_t);
extern HirselCoreHostOutcome store_crash_op(void *, uint8_t, uint8_t, uint64_t,
    const uint8_t *, size_t, uint8_t *, size_t, size_t);
extern uint64_t store_crash_events(void *);
extern uint8_t store_crash_acked(void *);
extern void store_crash_free(void *);
_Static_assert(sizeof(HirselCoreHostOutcome)==32, "provider outcome ABI");

static HirselCoreWait ready(void *state,HirselCoreHostRequest *request,uint64_t token) {
    (void)state; (void)request; (void)token; return (HirselCoreWait){0};
}
static HirselCoreStep issue(void *state,HirselCoreHostRequest *r,uint64_t token) {
    (void)token;
    HirselCoreHostOutcome out=store_crash_op(state,r->operation,r->mode,r->handle,
        r->path,r->path_len,r->buffer.data,r->buffer.len,r->buffer.capacity);
    if(!out.error.tag) {
        if(r->operation==1) r->buffer.len+=(size_t)out.value;
        if(r->operation==18) r->buffer.len+=(size_t)out.written;
    }
    return (HirselCoreStep){.outcome=out};
}
static HirselCoreStep resume(void *state,HirselCoreHostRequest *r,uint64_t token) {
    (void)state; (void)r; (void)token; abort();
}
static HirselCoreHostOutcome cancel(void *state,HirselCoreHostRequest *r,uint64_t token,uint8_t why) {
    (void)state; (void)r; (void)token; (void)why; abort();
}
static HirselCoreHostOutcome deliver(void *state,HirselCoreHostRequest *r,HirselCoreHostOutcome out) {
    (void)state; (void)r; return out;
}
static void diagnostic(void *state,const uint8_t *data,size_t len) {
    (void)state; if(write(2,data,len)!=(ssize_t)len) abort();
}
uint8_t __wrap_hirsel_core_os_run_with_sources(const HirselCoreOsConfig *input,
    const HirselCoreView *names,const HirselCoreSourceTable *sources,HirselCoreEntry entry) {
    (void)names; (void)sources;
    if(input->args_len!=2 || input->parameters_len>16) return 120;
    char raw_cut[32]={0};
    if(input->args[1].len>=sizeof raw_cut) return 120;
    memcpy(raw_cut,input->args[1].data,input->args[1].len);
    uint64_t cut=!strcmp(raw_cut,"count")?UINT64_MAX:strtoull(raw_cut,0,10);
    void *state=store_crash_new(cut);
    HirselCoreProvider provider={state,ready,issue,resume,cancel,deliver};
    HirselCoreEntryGrant caps[16]; uint64_t grants[16];
    for(size_t i=0;i<input->parameters_len;i++) {
        uint8_t kind=input->parameters[i].kind;
        caps[i]=(HirselCoreEntryGrant){kind,&provider,kind==0?7:0}; grants[i]=i+1;
    }
    static const char *phases[]={"init","write","read"};
    uint8_t status=0;
    for(uint64_t phase=0;phase<3;phase++) {
        store_crash_phase(state,phase);
        const char *expected=store_crash_acked(state)?"new":"prefix";
        HirselCoreBuffer args[3]={
            {(uint8_t*)phases[phase],strlen(phases[phase]),0},
            {(uint8_t*)input->args[0].data,input->args[0].len,0},
            {(uint8_t*)expected,strlen(expected),0}
        };
        HirselCoreProcessConfig cfg={0};
        cfg.root_limit=input->root_limit; cfg.args=args; cfg.args_len=3;
        cfg.grants=grants; cfg.grants_len=input->parameters_len;
        cfg.provider=&provider; cfg.state=state; cfg.diagnostic=diagnostic;
        cfg.stop_fd=2; cfg.capabilities=caps; cfg.capabilities_len=input->parameters_len;
        status=hirsel_core_run(&cfg,entry);
        if(status) {fprintf(stderr,"store phase %s: status %u\n",phases[phase],status);break;}
        if(phase==1 && cut==UINT64_MAX && !store_crash_acked(state)) {
            fprintf(stderr,"uncut store run did not acknowledge its checkpoint\n");status=120;break;
        }
    }
    if(!status) printf("cuts %llu\n",(unsigned long long)store_crash_events(state));
    store_crash_free(state);
    return status;
}
