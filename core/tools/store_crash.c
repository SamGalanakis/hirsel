/* Compiled store crash tests. Durable state belongs to the existing Rust W2
 * FileSystem; this shim adapts the published provider ABI and runs three phases. */
#include "hirsel_core_rt_host.h"
#include "hirsel_core_rt_tasks.h"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

extern void *store_crash_new(uint64_t);
extern void store_crash_phase(void *, uint64_t);
extern HirselCoreHostOutcome store_crash_op(void *, uint8_t, uint8_t, uint64_t,
    const uint8_t *, size_t, uint8_t *, size_t, size_t);
extern uint64_t store_crash_events(void *);
extern uint8_t store_crash_acked(void *);
extern uint64_t store_crash_floor(void *);
extern void store_crash_sync_failure(void *, uint8_t);
extern uint64_t store_crash_failed_syncs(void *);
extern void store_crash_free(void *);
_Static_assert(sizeof(HirselCoreHostOutcome)==32, "provider outcome ABI");

/* Borrow the owner's live Task handle from root spawn until root join returns.
 * All callbacks and task execution are serialized on this fixture's one worker.
 * Cancellation targets runtime-owned tasks, never host process IDs. */
static struct {
    uint64_t phase, target, events;
    int enabled, fired;
    HirselCoreContext *root;
    HirselCoreTask *owner;
    HirselCoreEntry entry;
} cancellation;
static void cancel_wait(HirselCoreContext *ctx) {
    if(!cancellation.enabled || cancellation.phase!=1 || ctx==cancellation.root || !cancellation.owner) return;
    uint64_t at=cancellation.events++;
    if(!cancellation.fired && at==cancellation.target) {
        cancellation.fired=1;
        hirsel_core_cancel(ctx,cancellation.owner);
    }
}
extern HirselCoreSpawn __real_hirsel_core_spawn(HirselCoreContext*,uint64_t,void*,HirselCoreTaskBody,HirselCoreTaskLayout);
HirselCoreSpawn __wrap_hirsel_core_spawn(HirselCoreContext *ctx,uint64_t bytes,void *input,HirselCoreTaskBody body,HirselCoreTaskLayout layout) {
    HirselCoreSpawn result=__real_hirsel_core_spawn(ctx,bytes,input,body,layout);
    if(cancellation.enabled && cancellation.phase==1 && ctx==cancellation.root && !cancellation.owner) cancellation.owner=result.task;
    return result;
}
extern HirselCoreHostOutcome __real_hirsel_core_host_op(HirselCoreContext*,uint8_t,uint64_t,HirselCoreBuffer*,uint8_t,uint64_t);
HirselCoreHostOutcome __wrap_hirsel_core_host_op(HirselCoreContext *ctx,uint8_t op,uint64_t handle,HirselCoreBuffer *data,uint8_t mode,uint64_t site) {
    /* Clock/random and witness output are not cancellable waits. */
    if(op!=10 && op!=11 && op!=12 && op!=13 && op!=18) cancel_wait(ctx);
    return __real_hirsel_core_host_op(ctx,op,handle,data,mode,site);
}
extern HirselCoreHostOutcome __real_hirsel_core_host_rename(HirselCoreContext*,uint64_t,HirselCoreBuffer*,HirselCoreBuffer*,uint64_t);
HirselCoreHostOutcome __wrap_hirsel_core_host_rename(HirselCoreContext *ctx,uint64_t handle,HirselCoreBuffer *from,HirselCoreBuffer *to,uint64_t site) {
    cancel_wait(ctx); return __real_hirsel_core_host_rename(ctx,handle,from,to,site);
}
extern HirselCoreTransfer __real_hirsel_core_send(HirselCoreContext*,HirselCoreChannel*,void*,uint64_t);
HirselCoreTransfer __wrap_hirsel_core_send(HirselCoreContext *ctx,HirselCoreChannel *channel,void *value,uint64_t site) {
    cancel_wait(ctx); return __real_hirsel_core_send(ctx,channel,value,site);
}
extern HirselCoreTransfer __real_hirsel_core_receive(HirselCoreContext*,HirselCoreChannel*,void*,uint64_t);
HirselCoreTransfer __wrap_hirsel_core_receive(HirselCoreContext *ctx,HirselCoreChannel *channel,void *value,uint64_t site) {
    cancel_wait(ctx); return __real_hirsel_core_receive(ctx,channel,value,site);
}
extern HirselCoreJoin __real_hirsel_core_join(HirselCoreContext*,HirselCoreTask*,void*,uint64_t);
HirselCoreJoin __wrap_hirsel_core_join(HirselCoreContext *ctx,HirselCoreTask *task,void *result,uint64_t site) {
    cancel_wait(ctx); return __real_hirsel_core_join(ctx,task,result,site);
}
static uint8_t cancellation_entry(HirselCoreContext *ctx,const HirselCoreProcessConfig *config,HirselCoreMainResult *result) {
    cancellation.root=ctx;
    return cancellation.entry(ctx,config,result);
}

static struct { uint64_t rng, hash; } schedule;
static void schedule_event(void *state,HirselCoreEvent event) {
    (void)state;
    uint64_t words[4]={event.kind,event.task,event.object,event.value};
    for(unsigned i=0;i<4;i++) schedule.hash=(schedule.hash^words[i])*1099511628211u;
}
static size_t schedule_choose(void *state,size_t runnable) {
    (void)state;
    schedule.rng=schedule.rng*6364136223846793005u+1442695040888963407u;
    return (size_t)((schedule.rng>>33)%runnable);
}

/* journal-ack (L24 (iv)(a)): each Sync answers Until(now) twice, so it stays
 * parked across exactly one scheduler turn of the other tasks. Counted in
 * turns, not time: an expired Until re-runs wait at the next turn. */
static struct { int enabled; uint64_t token[64], calls[64]; } hold;
static HirselCoreWait ready(void *state,HirselCoreHostRequest *request,uint64_t token) {
    (void)state;
    if(hold.enabled && request->operation==16) {
        /* Tokens are unique per operation; concurrent syncs keep separate counts. */
        uint64_t *calls=&hold.calls[token%64];
        if(hold.token[token%64]!=token) {hold.token[token%64]=token; *calls=0;}
        if((*calls)++<2) {
            struct timespec now; clock_gettime(CLOCK_MONOTONIC,&now);
            return (HirselCoreWait){1,(uint64_t)now.tv_sec*1000000000u+(uint64_t)now.tv_nsec};
        }
    }
    return (HirselCoreWait){0};
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
    if((input->args_len!=2 && input->args_len!=3) || input->parameters_len>16) return 120;
    char raw_cut[32]={0};
    if(input->args[1].len>=sizeof raw_cut) return 120;
    memcpy(raw_cut,input->args[1].data,input->args[1].len);
    uint64_t cut=!strcmp(raw_cut,"count")?UINT64_MAX:strtoull(raw_cut,0,10);
    char scenario[64]={0};
    if(input->args[0].len>=sizeof scenario) return 120;
    memcpy(scenario,input->args[0].data,input->args[0].len);
    uint64_t seed=0;
    if(input->args_len==3) {
        char raw_seed[32]={0};
        if(input->args[2].len>=sizeof raw_seed) return 120;
        memcpy(raw_seed,input->args[2].data,input->args[2].len);
        seed=strtoull(raw_seed,0,10);
    }
    schedule.rng=seed; schedule.hash=14695981039346656037u;
    cancellation.enabled=!strcmp(scenario,"journal-cancel");
    cancellation.target=cut; cancellation.entry=entry;
    int uncut=!strcmp(raw_cut,"count");
    uint8_t failure=(!strcmp(scenario,"poison-segment") || !strcmp(scenario,"poison-cut"))?1:
        !strcmp(scenario,"poison-end")?2:!strcmp(scenario,"poison-pack")?3:
        !strcmp(scenario,"poison-write")?4:0;
    if(failure || cancellation.enabled) cut=UINT64_MAX;
    void *state=store_crash_new(cut);
    store_crash_sync_failure(state,failure);
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
        cancellation.phase=phase;
        hold.enabled=phase==1 && !strcmp(scenario,"journal-ack");
        const char *expected=store_crash_acked(state)?"new":"prefix";
        char floor[32];
        snprintf(floor,sizeof floor,"%llu",(unsigned long long)store_crash_floor(state));
        HirselCoreBuffer args[4]={
            {(uint8_t*)phases[phase],strlen(phases[phase]),0},
            {(uint8_t*)input->args[0].data,input->args[0].len,0},
            {(uint8_t*)expected,strlen(expected),0},
            {(uint8_t*)floor,strlen(floor),0}
        };
        HirselCoreProcessConfig cfg={0};
        cfg.root_limit=input->root_limit; cfg.args=args; cfg.args_len=4;
        cfg.grants=grants; cfg.grants_len=input->parameters_len;
        cfg.provider=&provider; cfg.state=state; cfg.diagnostic=diagnostic;
        cfg.stop_fd=2; cfg.capabilities=caps; cfg.capabilities_len=input->parameters_len;
        if(seed) cfg.scheduler=(HirselCoreSchedulerProvider){&schedule,schedule_event,schedule_choose};
        status=hirsel_core_run(&cfg,cancellation.enabled?cancellation_entry:entry);
        if(status) {fprintf(stderr,"store phase %s: status %u\n",phases[phase],status);break;}
        if(phase==1 && failure && store_crash_failed_syncs(state)!=1) {
            fprintf(stderr,"named sync failure was not reached\n");status=120;break;
        }
        if(phase==1 && cancellation.enabled && !uncut && !cancellation.fired) {
            fprintf(stderr,"cancellation wait was not reached\n");status=120;break;
        }
        if(phase==1 && uncut && !store_crash_acked(state)) {
            fprintf(stderr,"uncut store run did not acknowledge its checkpoint\n");status=120;break;
        }
    }
    if(!status) printf("cuts %llu\n",(unsigned long long)(cancellation.enabled?cancellation.events:store_crash_events(state)));
    if(!status && seed) printf("events %llu\n",(unsigned long long)schedule.hash);
    store_crash_free(state);
    return status;
}
