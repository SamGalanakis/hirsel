/* Compiled store crash tests. Durable state belongs to the existing Rust W2
 * FileSystem; this shim adapts the published provider ABI and runs three phases. */
#include "hirsel_core_rt_host.h"
#include "hirsel_core_rt_tasks.h"
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

extern void *store_crash_new(uint64_t);
extern void store_crash_damage_seed(void *,uint64_t);
extern uint64_t store_crash_manifest_inode(void *);
extern void store_crash_phase(void *, uint64_t);
extern HirselCoreHostOutcome store_crash_op(void *, uint8_t, uint8_t, uint64_t,
    const uint8_t *, size_t, uint8_t *, size_t, size_t);
extern uint64_t store_crash_events(void *);
extern uint8_t store_crash_acked(void *);
extern uint64_t store_crash_floor(void *);
extern void store_crash_sync_failure(void *, uint8_t);
extern uint64_t store_crash_failed_syncs(void *);
extern void store_crash_free(void *);
extern void store_crash_orphan_end(void *);
extern uint8_t store_crash_crashed(void *);
extern void store_crash_checkpoint_reopen(void *);
extern void store_crash_expect_read_only(void *);
extern void store_crash_journal_fork(void *, uint8_t);
extern void store_crash_armed_io(void *, uint64_t *);
extern void store_crash_window(void *);
extern uint8_t store_crash_syncs(void *, uint8_t, uint64_t);
extern void store_crash_late_write(void *, uint8_t, uint8_t, uint64_t, uint64_t, uint64_t);
extern uint64_t store_crash_late_points(const void *);
extern uint8_t store_crash_late_polls(const void *);

static struct {
    uint64_t phase, issued;
    int enabled, issuing;
} window;
extern uint8_t hirsel_core_test_run_scripted(const HirselCoreProcessConfig *, HirselCoreEntry, uint64_t);
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
/* journal-worker-cancel: every Sync and SyncDir parks after issue and readies
 * itself. Each issue while the write phase's journal worker lives is one cut;
 * that cut cancels the worker alone, so a sync it issued is in flight and the
 * store's head survives to see the worker end. Resume answers the outcome
 * computed at issue. The worker is the spawn of journal_group_room(group_bytes)
 * + JOURNAL_WORKER_SLACK: the aligned group plus one tail block, with
 * journal_crash_options' group_bytes of 1 MiB. */
enum { JOURNAL_WORKER_BYTES=1048576+4096+32768 };
static struct {
    int enabled, fired;
    uint64_t phase, target, issued, token[64];
    HirselCoreTask *worker;
    HirselCoreHostOutcome out[64];
} parked;
extern HirselCoreSpawn __real_hirsel_core_spawn(HirselCoreContext*,uint64_t,uint64_t,void*,HirselCoreTaskBody,HirselCoreTaskLayout,uint64_t);
HirselCoreSpawn __wrap_hirsel_core_spawn(HirselCoreContext *ctx,uint64_t tasks,uint64_t bytes,void *input,HirselCoreTaskBody body,HirselCoreTaskLayout layout,uint64_t site) {
    HirselCoreSpawn result=__real_hirsel_core_spawn(ctx,tasks,bytes,input,body,layout,site);
    if(cancellation.enabled && cancellation.phase==1 && ctx==cancellation.root && !cancellation.owner) cancellation.owner=result.task;
    if(parked.enabled && parked.phase==1 && bytes==JOURNAL_WORKER_BYTES) parked.worker=result.task;
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
extern HirselCoreHostOutcome __real_hirsel_core_host_at(HirselCoreContext*,uint8_t,uint64_t,uint64_t,HirselCoreBuffer*,uint64_t);
HirselCoreHostOutcome __wrap_hirsel_core_host_at(HirselCoreContext *ctx,uint8_t op,uint64_t handle,uint64_t at,HirselCoreBuffer *data,uint64_t site) {
    cancel_wait(ctx);
    return __real_hirsel_core_host_at(ctx,op,handle,at,data,site);
}
extern HirselCoreHostOutcome __real_hirsel_core_host_stat(HirselCoreContext*,uint8_t,uint64_t,HirselCoreBuffer*,uint8_t,uint64_t);
HirselCoreHostOutcome __wrap_hirsel_core_host_stat(HirselCoreContext *ctx,uint8_t op,uint64_t handle,HirselCoreBuffer *data,uint8_t mode,uint64_t site) {
    cancel_wait(ctx);
    return __real_hirsel_core_host_stat(ctx,op,handle,data,mode,site);
}
extern HirselCoreTransfer __real_hirsel_core_send(HirselCoreContext*,HirselCoreChannel*,void*,uint64_t);
HirselCoreTransfer __wrap_hirsel_core_send(HirselCoreContext *ctx,HirselCoreChannel *channel,void *value,uint64_t site) {
    cancel_wait(ctx);
    HirselCoreTransfer sent=__real_hirsel_core_send(ctx,channel,value,site);
    if(window.enabled && window.phase==1 && ctx==cancellation.root && window.issuing) {
        window.issuing=0;
        if(sent.transferred) window.issued++;
    }
    return sent;
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

static uint64_t schedule_hash;
static void schedule_event(void *state,HirselCoreEvent event) {
    (void)state;
    uint64_t words[4]={event.kind,event.task,event.object,event.value};
    for(unsigned i=0;i<4;i++) schedule_hash=(schedule_hash^words[i])*1099511628211u;
}
/* journal-ack (L24 (iv)(a)): each Sync answers Until(now) twice, so it stays
 * parked across exactly one scheduler turn of the other tasks. Counted in
 * turns, not time: an expired Until re-runs wait at the next turn. */
static struct { int enabled, schedule_probe, child_probe; uint64_t token[64], calls[64]; } hold;
static HirselCoreWait ready(void *state,HirselCoreHostRequest *request,uint64_t token) {
    /* SleepUntil and SleepUntilWall (the runner's deadline and trigger timers) wait on
     * host time and never reach the W2 filesystem; their deadlines are far, so a timer
     * ends only by being dropped. A late-write case's new owner is the
     * exception: its sleeps end at once, so its lock polls advance the late
     * schedule past the write's completion. */
    if(request->operation==15 || request->operation==77)
        return store_crash_late_polls(state) ? (HirselCoreWait){0} : (HirselCoreWait){1,UINT64_MAX};
    const int probe=hold.schedule_probe && request->operation==12 &&
        request->buffer.len==8 && !memcmp(request->buffer.data,"SCHEDULE",8);
    const int child=hold.child_probe && (request->operation==2 || request->operation==38) && request->buffer.len>=20 &&
        !memcmp(request->buffer.data,"\x11\0",2) && !memcmp(request->buffer.data+16,"\x01\0\0\0",4);
    /* A Durable or Direct write is also its sync (§3.3b). */
    if((hold.enabled && store_crash_syncs(state,request->operation,request->handle)) || probe || child) {
        /* Tokens are unique per operation; concurrent syncs keep separate counts. */
        uint64_t *calls=&hold.calls[token%64];
        if(hold.token[token%64]!=token) {hold.token[token%64]=token; *calls=0;}
        if((*calls)++<(probe||child?1u:2u)) {
            struct timespec now; clock_gettime(CLOCK_MONOTONIC,&now);
            return (HirselCoreWait){1,(uint64_t)now.tv_sec*1000000000u+(uint64_t)now.tv_nsec};
        }
    }
    return (HirselCoreWait){0};
}
static uint64_t scratch_dirs;
static uint64_t park_wall_ms;
static HirselCoreStep issue(void *state,HirselCoreHostRequest *r,uint64_t token) {
    (void)token;
    if(window.enabled && window.phase==1 && r->operation==12 &&
        r->buffer.len==5 && !memcmp(r->buffer.data,"ISSUE",5)) window.issuing=1;
    if(r->operation==15 || r->operation==77) return (HirselCoreStep){0};
    if(park_wall_ms && r->operation==10) return (HirselCoreStep){.outcome.value=park_wall_ms};
    const uint64_t scratch_handle=UINT64_C(1)<<48;
    if(r->operation==7) {
        char name[32];
        int count=snprintf(name,sizeof name,"scratch-%llu",(unsigned long long)++scratch_dirs);
        if(count<0 || (size_t)count>r->buffer.capacity-r->buffer.len) abort();
        memcpy(r->buffer.data+r->buffer.len,name,(size_t)count);
        r->buffer.len+=(size_t)count;
        return (HirselCoreStep){.outcome.value=scratch_handle+scratch_dirs};
    }
    uint8_t path[4096];
    const uint8_t *used_path=r->path;
    size_t path_len=r->path_len;
    if(r->handle>=scratch_handle && (r->operation==0 || r->operation==8)) {
        int count=snprintf((char *)path,sizeof path,"scratch-%llu/",(unsigned long long)(r->handle-scratch_handle));
        if(count<0 || r->path_len>sizeof path-(size_t)count) abort();
        memcpy(path+count,r->path,r->path_len);
        used_path=path;
        path_len=(size_t)count+r->path_len;
    }
    HirselCoreHostOutcome out=store_crash_op(state,r->operation,r->mode,r->handle,
        used_path,path_len,r->buffer.data,r->buffer.len,r->buffer.capacity);
    if(!out.error.tag) {
        if(r->operation==1 || r->operation==37) r->buffer.len+=(size_t)out.value;
        if(r->operation==18) r->buffer.len+=(size_t)out.written;
    }
    if(parked.enabled && (r->operation==17 || store_crash_syncs(state,r->operation,r->handle))) {
        if(parked.phase==1 && parked.worker && parked.issued++==parked.target && !parked.fired) {
            parked.fired=1;
            hirsel_core_cancel(0,parked.worker);
        }
        /* A Durable or Direct write is its sync, but the host completes it
         * on the issuing stack (§3.3b ring writes park the unit, not the
         * task): the cancel lands during the write, which delivers its own
         * result. Only syncs park. */
        if(r->operation!=16 && r->operation!=17) return (HirselCoreStep){.outcome=out};
        if(parked.token[token%64]) abort();
        parked.token[token%64]=token; parked.out[token%64]=out;
        hirsel_core_ready(token);
        return (HirselCoreStep){.kind=1};
    }
    return (HirselCoreStep){.outcome=out};
}
static HirselCoreStep resume(void *state,HirselCoreHostRequest *r,uint64_t token) {
    (void)state; (void)r;
    if(!parked.enabled || parked.token[token%64]!=token) abort();
    parked.token[token%64]=0;
    return (HirselCoreStep){.outcome=parked.out[token%64]};
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
    if(input->args_len<2 || input->args_len>4 || input->parameters_len>16) return 120;
    char raw_cut[32]={0};
    if(input->args[1].len>=sizeof raw_cut) return 120;
    memcpy(raw_cut,input->args[1].data,input->args[1].len);
    uint64_t cut=!strcmp(raw_cut,"count")?UINT64_MAX:strtoull(raw_cut,0,10);
    char scenario[64]={0};
    if(input->args[0].len>=sizeof scenario) return 120;
    memcpy(scenario,input->args[0].data,input->args[0].len);
    /* BL12/BL13 (d1)/(d2), BL14 (d3) and G1: the cut argument is readable:complete:power_cut,
     * schedule points of the new owner's or follower's run; count takes 1:2 and no cut. */
    int late_failed=!strcmp(scenario,"late-group-failed") || !strcmp(scenario,"late-follow-failed");
    int late_creation=!strcmp(scenario,"late-cut") || !strcmp(scenario,"late-genesis");
    int late=late_failed || late_creation || !strcmp(scenario,"late-group") || !strcmp(scenario,"late-follow");
    unsigned long long readable=1,complete=2,power_cut=UINT64_MAX;
    if(late && strcmp(raw_cut,"count") &&
        (sscanf(raw_cut,"%llu:%llu:%llu",&readable,&complete,&power_cut)!=3 || !readable || complete<=readable)) return 120;
    if(late) cut=UINT64_MAX;
    uint64_t seed=0;
    if(input->args_len>=3) {
        char raw_seed[32]={0};
        if(input->args[2].len>=sizeof raw_seed) return 120;
        memcpy(raw_seed,input->args[2].data,input->args[2].len);
        seed=strtoull(raw_seed,0,10);
    }
    schedule_hash=14695981039346656037u;
    cancellation.enabled=!strcmp(scenario,"journal-cancel");
    cancellation.target=cut; cancellation.entry=entry;
    parked.enabled=!strcmp(scenario,"journal-worker-cancel");
    parked.target=cut;
    int uncut=!strcmp(raw_cut,"count");
    enum { ORPHAN_END_SYNC_FAILURE=6, OPEN_MANIFEST_SYNC_FAILURE=8, SECOND_SEGMENT_SHORT_WRITE=9,
        WINDOW_CHUNK_FAILURE=10, WINDOW_SYNC_FAILURE=11 };
    window.enabled=!strcmp(scenario,"window") || !strcmp(scenario,"window-chunk-failure") || !strcmp(scenario,"window-sync-failure");
    window.issued=0; window.issuing=0;
    int recovery_failure=!strcmp(scenario,"orphan-end-recovery-sync");
    int open_sync_failure=!strcmp(scenario,"checkpoint-reopen-sync-failure");
    int checkpoint_reopen=open_sync_failure || !strcmp(scenario,"checkpoint-reopen");
    uint8_t failure=(!strcmp(scenario,"poison-segment") || !strcmp(scenario,"poison-cut"))?1:
        !strcmp(scenario,"poison-end")?2:!strcmp(scenario,"poison-pack")?3:
        !strcmp(scenario,"poison-write")?4:!strcmp(scenario,"journal-place-split")?5:
        !strcmp(scenario,"journal-place-short")?SECOND_SEGMENT_SHORT_WRITE:
        !strcmp(scenario,"window-chunk-failure")?WINDOW_CHUNK_FAILURE:
        !strcmp(scenario,"window-sync-failure")?WINDOW_SYNC_FAILURE:
        recovery_failure?ORPHAN_END_SYNC_FAILURE:open_sync_failure?OPEN_MANIFEST_SYNC_FAILURE:0;
    int orphan_end=recovery_failure || !strcmp(scenario,"orphan-end");
    if((failure && failure!=5) || cancellation.enabled || parked.enabled || orphan_end) cut=UINT64_MAX;
    uint64_t damage_seed=0;
    if(input->args_len==4) {
        char raw_damage[32]={0};
        size_t len=input->args[3].len;
        if(cut==UINT64_MAX || checkpoint_reopen || len==0 || len>=sizeof raw_damage) return 120;
        if(input->args[1].len==0 || strspn(raw_cut,"0123456789")!=input->args[1].len) return 120;
        memcpy(raw_damage,input->args[3].data,len);
        if(strspn(raw_damage,"0123456789")!=len) return 120;
        errno=0;
        damage_seed=strtoull(raw_damage,0,10);
        if(errno==ERANGE) return 120;
    }
    void *state=store_crash_new(cut);
    if(input->args_len==4) store_crash_damage_seed(state,damage_seed);
    store_crash_sync_failure(state,failure);
    if(orphan_end) store_crash_orphan_end(state);
    if(window.enabled) store_crash_window(state);
    if(checkpoint_reopen) store_crash_checkpoint_reopen(state);
    int fork_image=!strcmp(scenario,"journal-fork")?0:!strcmp(scenario,"journal-fork-ack-cut")?1:
        !strcmp(scenario,"journal-fork-complete")?2:!strcmp(scenario,"journal-fork-torn")?3:
        !strcmp(scenario,"journal-fork-bad-end")?4:!strcmp(scenario,"journal-fork-child-unwritten")?5:-1;
    if(late) store_crash_late_write(state,(uint8_t)late_creation,(uint8_t)late_failed,readable,complete,power_cut);
    if(fork_image>=0) {
        store_crash_journal_fork(state,(uint8_t)fork_image);
    }
    HirselCoreProvider provider={state,ready,issue,resume,cancel,deliver};
    HirselCoreEntryGrant caps[16]; uint64_t grants[16];
    for(size_t i=0;i<input->parameters_len;i++) {
        uint8_t kind=input->parameters[i].kind;
        caps[i]=(HirselCoreEntryGrant){kind,&provider,kind==0?7:0,false}; grants[i]=i+1;
    }
    static const char *phases[]={"init","write","read","reopen"};
    static const uint64_t reopen_order[]={0,1,3,2};
    int reopen=orphan_end || checkpoint_reopen || late;
    int read_only=!strcmp(scenario,"read-only");
    uint64_t original_manifest=0;
    uint8_t status=0;
    for(uint64_t run=0;run<(reopen && !recovery_failure?4:3);run++) {
        uint64_t phase=reopen?reopen_order[run]:run;
        store_crash_phase(state,phase);
        if(phase==2 && read_only) store_crash_expect_read_only(state);
        if(phase==1 && read_only && input->args_len==4) original_manifest=store_crash_manifest_inode(state);
        cancellation.phase=phase; window.phase=phase;
        /* Law 8 opens at midnight, noon and the following midnight. */
        park_wall_ms=!strcmp(scenario,"steps-park")?1000+phase*UINT64_C(43200000):0;
        parked.phase=phase; parked.worker=0;
        hold.enabled=phase==1 && !strcmp(scenario,"journal-ack");
        hold.child_probe=phase==1 && fork_image==5;
        hold.schedule_probe=seed && phase==1 &&
            (!strcmp(scenario,"journal-group") || !strcmp(scenario,"journal-rotate"));
        /* This fixture syncs an intentionally corrupt MANIFEST.tmp. Its
         * rename can survive even if the directory sync never returned. */
        int torn_manifest=phase==2 && read_only && input->args_len==4
            && store_crash_manifest_inode(state)!=original_manifest;
        const char *expected=store_crash_acked(state)||torn_manifest?"new":"prefix";
        char issued[32], floor[32];
        if(window.enabled) {
            snprintf(issued,sizeof issued,"%llu",(unsigned long long)window.issued);
            expected=issued;
        }
        snprintf(floor,sizeof floor,"%llu",(unsigned long long)store_crash_floor(state));
        const char *fixture=checkpoint_reopen?"checkpoint":scenario;
        HirselCoreBuffer args[4]={
            {(uint8_t*)phases[phase],strlen(phases[phase]),0},
            {(uint8_t*)fixture,strlen(fixture),0},
            {(uint8_t*)expected,strlen(expected),0},
            {(uint8_t*)floor,strlen(floor),0}
        };
        HirselCoreProcessConfig cfg={0};
        cfg.root_limit=input->root_limit; cfg.args=args; cfg.args_len=4;
        cfg.grants=grants; cfg.grants_len=input->parameters_len;
        cfg.provider=&provider; cfg.state=state; cfg.diagnostic=diagnostic;
        cfg.stop_fd=2; cfg.capabilities=caps; cfg.capabilities_len=input->parameters_len;
        HirselCoreEntry run_entry=(cancellation.enabled || window.enabled)?cancellation_entry:entry;
        if(seed) cfg.scheduler.event=schedule_event;
        status=seed?hirsel_core_test_run_scripted(&cfg,run_entry,seed):hirsel_core_run(&cfg,run_entry);
        if(status) {fprintf(stderr,"store phase %s: status %u\n",phases[phase],status);break;}
        if(phase==1 && failure && !recovery_failure && !open_sync_failure && (failure!=5 || uncut) && store_crash_failed_syncs(state)!=1) {
            fprintf(stderr,"named sync failure was not reached\n");status=120;break;
        }
        if(phase==1 && (failure==5 || failure==SECOND_SEGMENT_SHORT_WRITE) && store_crash_failed_syncs(state) &&
            (store_crash_floor(state)!=2 || !store_crash_acked(state))) {
            fprintf(stderr,"byte split did not settle A as durable and B as failed\n");status=120;break;
        }
        if(phase==1 && parked.enabled && !uncut && !parked.fired) {
            fprintf(stderr,"journal worker sync %llu was not issued\n",(unsigned long long)parked.target);status=120;break;
        }
        if(phase==1 && cancellation.enabled && !uncut && !cancellation.fired) {
            fprintf(stderr,"cancellation wait was not reached\n");status=120;break;
        }
        if((recovery_failure || open_sync_failure) && phase==3 && store_crash_failed_syncs(state)!=1) {
            fprintf(stderr,"recovery directory sync failure was not reached once\n");status=120;break;
        }
        if(orphan_end && (phase==1 || (phase==3 && !recovery_failure)) && !store_crash_crashed(state)) {
            fprintf(stderr,"orphan END crash was not reached in phase %s\n",phases[phase]);status=120;break;
        }
        if(late && phase==1 && !store_crash_crashed(state)) {
            fprintf(stderr,"late group write was not held\n");status=120;break;
        }
        if(checkpoint_reopen && (phase==1 || (phase==3 && !open_sync_failure)) && !store_crash_crashed(state)) {
            fprintf(stderr,"checkpoint double crash was not reached in phase %s\n",phases[phase]);status=120;break;
        }
        /* STORE-SPEC §2.4, §3.2 Syncs: 16 frameless wakes do no I/O; four commits
         * at group_commits 2 are two groups, then one idle END; close with d
         * unchanged writes no END; END rewrites never reopen the END file. */
        if(phase==1 && !strcmp(scenario,"journal-sync-count")) {
            uint64_t io[4]; store_crash_armed_io(state,io);
            if(io[0]!=0 || io[1]!=1 || io[2]!=2 || io[3]!=0) {
                fprintf(stderr,"sync count: %llu END opens, %llu END syncs, %llu segment syncs, %llu other syncs; want 0 1 2 0\n",
                    (unsigned long long)io[0],(unsigned long long)io[1],(unsigned long long)io[2],(unsigned long long)io[3]);
                status=120;break;
            }
        }
        if(phase==1 && uncut && !reopen && !store_crash_acked(state)) {
            fprintf(stderr,"uncut store run did not acknowledge its checkpoint\n");status=120;break;
        }
    }
    if(!status && late) printf("points %llu\n",(unsigned long long)store_crash_late_points(state));
    else if(!status) printf("cuts %llu\n",(unsigned long long)(cancellation.enabled?cancellation.events:parked.enabled?parked.issued:store_crash_events(state)));
    if(!status && seed) printf("events %llu\n",(unsigned long long)schedule_hash);
    store_crash_free(state);
    return status;
}
