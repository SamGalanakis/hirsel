#define _POSIX_C_SOURCE 200809L
/* Native test provider. No product code or old fault-schema reader is linked. */
#include "hirsel_core_rt_host.h"
#include "hirsel_core_rt_tasks.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static struct {
    char mode[32];
    unsigned opens, reads, writes, syncs, sync_dirs, live, cancels, blocked;
    unsigned crash_cut, crash_events, crash_pending, crashed;
    unsigned char crash_file[32]; size_t crash_length;
    char live_name, durable_name;
    unsigned char file[32]; size_t length, cursor;
    HirselCoreContext *root, *active, *waiting;
    HirselCoreTask *child;
    unsigned wait_active;
    HirselCoreEntry entry;
} s;
#define CHECK(c) do { if (!(c)) { dprintf(2,"provider fixture: line %u: %s\n",__LINE__,#c); _exit(120); } } while(0)
static int is(const char *m) { return !strcmp(s.mode,m); }
static int crash_mode(void) { return !strncmp(s.mode,"crash",5); }
static void crash_mark_cut(void) {
    if(crash_mode() && !s.crashed && s.crash_events++==s.crash_cut) s.crash_pending=1;
}
static int issued(void) { return !strncmp(s.mode,"issue-",6) || !strncmp(s.mode,"delivery-",9); }
static int blocked(void) { return !strncmp(s.mode,"blocked-",8); }
extern void __real_hirsel_core_cancel(HirselCoreContext*,HirselCoreTask*);
void __wrap_hirsel_core_cancel(HirselCoreContext *ctx,HirselCoreTask *task) {
    if (blocked()) {
        CHECK(ctx==s.root && s.wait_active && s.waiting!=ctx);
        s.blocked++;
    }
    __real_hirsel_core_cancel(ctx,task);
}
extern HirselCoreSpawn __real_hirsel_core_spawn(HirselCoreContext*,uint64_t,uint64_t,void*,HirselCoreTaskBody,HirselCoreTaskLayout,uint64_t);
HirselCoreSpawn __wrap_hirsel_core_spawn(HirselCoreContext *ctx,uint64_t tasks,uint64_t bytes,void *input,HirselCoreTaskBody body,HirselCoreTaskLayout layout,uint64_t site) {
    HirselCoreSpawn h=__real_hirsel_core_spawn(ctx,tasks,bytes,input,body,layout,site);
    CHECK(h.task); s.child=h.task; return h;
}
extern HirselCoreTransfer __real_hirsel_core_receive(HirselCoreContext*,HirselCoreChannel*,void*,uint64_t);
HirselCoreTransfer __wrap_hirsel_core_receive(HirselCoreContext *ctx,HirselCoreChannel *ch,void *out,uint64_t site) {
    int track=is("blocked-receive") && ctx!=s.root;
    if(track) { CHECK(!s.wait_active);s.wait_active=1;s.waiting=ctx; }
    HirselCoreTransfer r=__real_hirsel_core_receive(ctx,ch,out,site);
    if(track) {CHECK(r.status==1 && !r.transferred);s.wait_active=0;}
    return r;
}
extern HirselCoreTransfer __real_hirsel_core_send(HirselCoreContext*,HirselCoreChannel*,void*,uint64_t);
HirselCoreTransfer __wrap_hirsel_core_send(HirselCoreContext *ctx,HirselCoreChannel *ch,void *input,uint64_t site) {
    int track=is("blocked-send") && ctx!=s.root;
    if(track) {CHECK(!s.wait_active);s.wait_active=1;s.waiting=ctx;}
    HirselCoreTransfer r=__real_hirsel_core_send(ctx,ch,input,site);
    if(track) {CHECK(r.status==1 && !r.transferred);s.wait_active=0;}
    return r;
}
extern HirselCoreHostOutcome __real_hirsel_core_host_io(HirselCoreContext*,uint8_t,uint64_t,HirselCoreBuffer*,uint8_t,uint64_t);
HirselCoreHostOutcome __wrap_hirsel_core_host_io(HirselCoreContext *ctx,uint8_t op,uint64_t handle,HirselCoreBuffer *b,uint8_t mode,uint64_t site) {
    HirselCoreContext *old=s.active; s.active=ctx;
    HirselCoreHostOutcome out=__real_hirsel_core_host_io(ctx,op,handle,b,mode,site);
    s.active=old;return out;
}
extern HirselCoreHostOutcome __real_hirsel_core_host_clock(HirselCoreContext*,uint8_t,uint64_t,HirselCoreBuffer*,uint8_t,uint64_t);
HirselCoreHostOutcome __wrap_hirsel_core_host_clock(HirselCoreContext *ctx,uint8_t op,uint64_t handle,HirselCoreBuffer *b,uint8_t mode,uint64_t site) {
    HirselCoreContext *old=s.active; s.active=ctx;
    HirselCoreHostOutcome out=__real_hirsel_core_host_clock(ctx,op,handle,b,mode,site);
    s.active=old;return out;
}
extern HirselCoreHostOutcome __real_hirsel_core_host_op(HirselCoreContext*,uint8_t,uint64_t,HirselCoreBuffer*,uint8_t,uint64_t);
HirselCoreHostOutcome __wrap_hirsel_core_host_op(HirselCoreContext *ctx,uint8_t op,uint64_t handle,HirselCoreBuffer *b,uint8_t mode,uint64_t site) {
    HirselCoreContext *old=s.active; s.active=ctx;
    HirselCoreHostOutcome out=__real_hirsel_core_host_op(ctx,op,handle,b,mode,site);
    s.active=old;return out;
}
static HirselCoreHostOutcome legacy_issue(void *state,HirselCoreHostRequest *r) {
    (void)state; HirselCoreHostOutcome out={0};
    if(crash_mode() && s.crash_pending && !s.crashed) {
        s.crashed=1; s.crash_pending=0; out.error.tag=5; out.error.code=125; return out;
    }
    if(issued() && r->operation==(!strstr(s.mode,"write")?1:2) && !strncmp(s.mode,"issue-",6)) {
        CHECK(s.active && s.active!=s.root && s.child);__real_hirsel_core_cancel(s.active,s.child);
    }
    switch(r->operation) {
    case 0:
        if(crash_mode()) {
            CHECK(r->handle==7 && r->path_len==1);
            if(r->mode==1 && r->path[0]=='a') {
                s.live_name='a'; s.length=0; s.live=1; out.value=17; crash_mark_cut(); return out;
            }
            if(r->mode==0 && ((r->path[0]=='a' && s.live_name=='a') || (r->path[0]=='b' && s.live_name=='b'))) {
                s.live=1; out.value=17; return out;
            }
            out.error.tag=1; return out;
        }
        if(is("sync") || is("sync-fail")) {
            CHECK(r->handle==7 && ((r->path_len==6 && !memcmp(r->path,"sync-a",6) && r->mode==1) || (r->path_len==6 && !memcmp(r->path,"sync-b",6) && r->mode==0)));
            s.opens++; CHECK(!s.live || (r->path_len==6 && !memcmp(r->path,"sync-b",6)));
            if(r->path_len==6 && !memcmp(r->path,"sync-a",6)) s.length=0;
            s.live=1; s.cursor=0; out.value=17; return out;
        }
        CHECK(r->handle==7 && r->path_len==1 && r->path[0]=='f');
        if(is("errors") && s.opens<5) {
            out.error.tag=(uint8_t)(++s.opens);out.error.code=out.error.tag==5?1234:0;return out;
        }
        s.opens++;CHECK(!s.live);s.live=1;s.cursor=0;out.value=17;
        if(!strncmp(s.mode,"append",6) || strstr(s.mode,"write")) CHECK(r->mode==(s.opens==1?2:0));
        return out;
    case 1: {
        if(crash_mode()) {
            CHECK(r->handle==17 && s.live);
            size_t count=s.length;
            if(count>r->buffer.capacity-r->buffer.len) count=r->buffer.capacity-r->buffer.len;
            memcpy(r->buffer.data+r->buffer.len,s.file,count); r->buffer.len+=count; out.value=count; return out;
        }
        CHECK(r->handle==17 && s.live);
        unsigned n=s.reads++;
        if(is("read") && (n==0 || n==2)) {out.error.tag=n==0?5:1;out.error.code=n==0?5:0;return out;}
        if(is("before-read")) CHECK(0);
        const unsigned char *bytes=s.file;size_t len=s.length;
        if(issued()) {bytes=(const unsigned char*)"A";len=1;}
        size_t count=len-s.cursor,spare=r->buffer.capacity>r->buffer.len?r->buffer.capacity-r->buffer.len:0;
        if(is("read") && n==1 && count>2) count=2;
        if(count>spare) count=spare;
        if(count) memcpy(r->buffer.data+r->buffer.len,bytes+s.cursor,count);
        r->buffer.len+=count;s.cursor+=count;out.value=count;return out;
    }
    case 2: {
        if(crash_mode()) {
            CHECK(r->handle==17 && s.live && r->buffer.len==1);
            s.file[0]=r->buffer.data[0]; s.length=1; crash_mark_cut(); return out;
        }
        CHECK(r->handle==17 && s.live);
        unsigned n=s.writes++;size_t count=r->buffer.len;
        if(issued()) {CHECK(n==0 && count==3);count=1;out.error.tag=5;out.error.code=5;}
        else if(is("sync") || is("sync-fail")) {
            CHECK(n==0 && count==7);
        }
        else if(n==0) {CHECK(!strncmp(s.mode,"append",6) && count==3);count=(size_t)(s.mode[6]-'0');out.error.tag=5;out.error.code=5;}
        else if(n==2) {CHECK(count==1);count=0;out.error.tag=2;}
        CHECK(s.length+count<=sizeof s.file);
        if(count) memcpy(s.file+s.length,r->buffer.data,count);
        s.length+=count;out.written=count;return out;
    }
    case 3: CHECK(r->handle==17 && s.live);s.live=0;if(crash_mode()) crash_mark_cut();return out;
    case 16:
        if(crash_mode()) { CHECK(r->handle==17 && s.live); memcpy(s.crash_file,s.file,s.length); s.crash_length=s.length; s.syncs++; crash_mark_cut(); return out; }
        CHECK(r->handle==17 && s.live);
        s.syncs++;
        if(is("sync-fail")) { out.error.tag=5; out.error.code=5; }
        return out;
    case 17:
        if(crash_mode()) { CHECK(r->handle==7); s.durable_name=s.live_name; s.sync_dirs++; crash_mark_cut(); return out; }
        CHECK(r->handle==7);
        s.sync_dirs++;
        return out;
    case 18:
        CHECK(r->handle==0 && r->buffer.capacity-r->buffer.len>=4);
        for(size_t i=0;i<r->buffer.capacity-r->buffer.len;i++) r->buffer.data[r->buffer.len+i]=(uint8_t)(0xa0+i);
        out.written=r->buffer.capacity-r->buffer.len;
        r->buffer.len=r->buffer.capacity;
        return out;
    case 14:
        if(crash_mode()) { CHECK(r->handle==7 && r->path_len==3 && r->path[0]=='a' && r->path[1]==0 && r->path[2]=='b'); s.live_name='b'; crash_mark_cut(); return out; }
        CHECK(r->handle==7 && r->path_len==13 && !memcmp(r->path,"sync-a\0sync-b",13));
        return out;
    default: CHECK(0);
    }
    return out;
}
static HirselCoreStep issue(void *state,HirselCoreHostRequest *r,uint64_t token) {
    (void)token; HirselCoreStep s={0}; s.outcome=legacy_issue(state,r); return s;
}
static HirselCoreWait ready_wait(void *state,HirselCoreHostRequest *r,uint64_t token) {
    (void)state;(void)r;(void)token; HirselCoreWait w={0}; return w;
}
static HirselCoreStep never_resume(void *state,HirselCoreHostRequest *r,uint64_t token) {
    (void)state;(void)r;(void)token; CHECK(0); HirselCoreStep st={0}; return st;
}
static HirselCoreHostOutcome never_abort(void *state,HirselCoreHostRequest *r,uint64_t token,uint8_t why) {
    (void)state;(void)r;(void)token;(void)why; CHECK(0); HirselCoreHostOutcome o={0}; return o;
}
static HirselCoreHostOutcome deliver(void *state,HirselCoreHostRequest *r,HirselCoreHostOutcome out) {
    (void)state;
    if(issued() && r->operation==(!strstr(s.mode,"write")?1:2) && !strncmp(s.mode,"delivery-",9)) {
        CHECK(s.active && s.active!=s.root && s.child);__real_hirsel_core_cancel(s.active,s.child);
    }
    return out;
}
/* REPLAY-SPEC section 3 standing cases, run as "s0-*" modes. A wrapper forwards
   all five callbacks to an inner provider and counts each call per operation. */
static HirselCoreProvider s0_inner, s0_outer;
static unsigned s0_calls[5][19];
static char s0_order[4]; static int s0_at;
static HirselCoreWait s0_wait(void *st,HirselCoreHostRequest *r,uint64_t t) {s0_calls[0][r->operation]++;return s0_inner.wait(st,r,t);}
static HirselCoreStep s0_issue(void *st,HirselCoreHostRequest *r,uint64_t t) {s0_calls[1][r->operation]++;return s0_inner.issue(st,r,t);}
static HirselCoreStep s0_resume(void *st,HirselCoreHostRequest *r,uint64_t t) {s0_calls[2][r->operation]++;return s0_inner.resume(st,r,t);}
static HirselCoreHostOutcome s0_abort(void *st,HirselCoreHostRequest *r,uint64_t t,uint8_t why) {s0_calls[3][r->operation]++;return s0_inner.abort(st,r,t,why);}
static HirselCoreHostOutcome s0_deliver(void *st,HirselCoreHostRequest *r,HirselCoreHostOutcome o) {s0_calls[4][r->operation]++;return s0_inner.deliver(st,r,o);}
static void s0_wrap(void) {
    s0_outer=s0_inner; s0_outer.wait=s0_wait; s0_outer.issue=s0_issue; s0_outer.resume=s0_resume;
    s0_outer.abort=s0_abort; s0_outer.deliver=s0_deliver;
}
static const HirselCoreProvider *s0_selected;
/* (a): the sleeper's 20 ms sleep must not block the scheduler, so the peer
   finishes first: PS, with or without the wrapper. */
static uint8_t s0_sleeper(HirselCoreContext *c,void *out,void *in) {
    (void)out;(void)in;
    HirselCoreHostRequest now={0}; now.operation=11;
    uint64_t deadline=hirsel_core_host(c,s0_selected,&now).value+20000000;
    HirselCoreHostRequest sleep={0}; sleep.operation=15; sleep.site=1;
    sleep.buffer=(HirselCoreBuffer){(uint8_t*)&deadline,8,0};
    CHECK(hirsel_core_host(c,s0_selected,&sleep).status==0);
    s0_order[s0_at++]='S'; return 0;
}
static uint8_t s0_peer(HirselCoreContext *c,void *out,void *in) {(void)c;(void)out;(void)in;s0_order[s0_at++]='P';return 0;}
static uint8_t s0_sleep_entry(HirselCoreContext *c,const HirselCoreProcessConfig *cfg,HirselCoreMainResult *out) {
    (void)cfg; char in=0,res=0; HirselCoreValueLayout v={0,1,0,NULL}; HirselCoreTaskLayout l={v,v};
    HirselCoreSpawn a=hirsel_core_spawn(c,0,4096,&in,s0_sleeper,l,0), b=hirsel_core_spawn(c,0,4096,&in,s0_peer,l,0);
    CHECK(hirsel_core_join(c,a.task,&res,2).status==0 && hirsel_core_join(c,b.task,&res,3).status==0);
    out->kind=0; out->exit=0; return 0;
}
static uint8_t s0_sleep(int wrapped) {
    HirselCoreOsConfig os={0}; os.root_limit=1<<20; os.grant_mask=(1<<2)|(1<<9); os.stdout_fd=1; os.stderr_fd=2;
    HirselCoreOpenProcess p=hirsel_core_os_process_new(&os); CHECK(p.process);
    HirselCoreProcessConfig cfg=*hirsel_core_os_process_config(p.process);
    s0_inner=*cfg.provider; s0_wrap();
    s0_selected=wrapped?&s0_outer:cfg.provider; cfg.provider=s0_selected;
    CHECK(hirsel_core_run(&cfg,s0_sleep_entry)==0);
    CHECK(!strcmp(s0_order,"PS"));
    if(wrapped) CHECK(s0_calls[0][15]>=2 && s0_calls[1][15]==1 && s0_calls[2][15]==0 && s0_calls[3][15]==0 && s0_calls[4][15]==1);
    hirsel_core_os_process_free(p.process);
    return 0;
}
/* (d): every tag is issued and delivered once through the wrapper. SLEEP_UNTIL
   first waits Until a passed deadline, so its wait runs twice. */
static HirselCoreWait s0_test_wait(void *st,HirselCoreHostRequest *r,uint64_t t) {
    (void)st;(void)t; HirselCoreWait w={0};
    if(r->operation==15 && s0_calls[0][15]==1) {w.kind=1;w.deadline_ns=1;}
    return w;
}
static HirselCoreStep s0_test_issue(void *st,HirselCoreHostRequest *r,uint64_t t) {(void)st;(void)r;(void)t;HirselCoreStep x={0};return x;}
static HirselCoreHostOutcome s0_test_deliver(void *st,HirselCoreHostRequest *r,HirselCoreHostOutcome o) {(void)st;(void)r;return o;}
static uint8_t s0_deliver_entry(HirselCoreContext *c,const HirselCoreProcessConfig *cfg,HirselCoreMainResult *out) {
    (void)cfg; uint8_t bytes[64]={0};
    for(uint8_t op=0;op<=18;op++) {
        uint64_t deadline=0; HirselCoreHostRequest r={0}; r.operation=op; r.site=op+1u;
        r.path=bytes; r.buffer=(HirselCoreBuffer){bytes,0,sizeof bytes};
        if(op==15) r.buffer=(HirselCoreBuffer){(uint8_t*)&deadline,8,8};
        CHECK(hirsel_core_host(c,&s0_outer,&r).status==0 && s0_calls[4][op]==1);
    }
    for(unsigned op=0;op<=18;op++)
        CHECK(s0_calls[0][op]==(op==15?2u:1u) && s0_calls[1][op]==1 && s0_calls[2][op]==0 && s0_calls[3][op]==0 && s0_calls[4][op]==1);
    out->kind=0; out->exit=0; return 0;
}
static uint8_t s0_run(uint8_t (*entry)(HirselCoreContext*,const HirselCoreProcessConfig*,HirselCoreMainResult*)) {
    HirselCoreProcessConfig cfg={0}; cfg.root_limit=1<<20; cfg.provider=&s0_outer; cfg.stop_fd=2;
    return hirsel_core_run(&cfg,entry);
}
static uint8_t s0_case(void) {
    s0_inner=(HirselCoreProvider){&s,s0_test_wait,s0_test_issue,never_resume,never_abort,s0_test_deliver}; s0_wrap();
    if(is("s0-sleep-direct")) return s0_sleep(0);
    if(is("s0-sleep-wrapper")) return s0_sleep(1);
    if(is("s0-deliver")) {CHECK(s0_run(s0_deliver_entry)==0);return 0;}
    /* A NULL callback is refused at install: "run stop: Provider", status 102,
       before main runs. s0-null-N clears callback N (wait..deliver). */
    if(!strncmp(s.mode,"s0-null-",8)) {
        void **words=(void**)&s0_outer; words[1+(s.mode[8]-'0')]=0;
        s0_run(s0_deliver_entry); CHECK(0);
    }
    CHECK(0); return 1;
}
static void diagnostic(void *state,const uint8_t *data,size_t len) {if(crash_mode() && s.crashed)return;(void)state;CHECK(write(2,data,len)==(ssize_t)len);}
static void event(void *state,HirselCoreEvent e) {(void)state;if(e.kind==HIRSEL_CORE_EVENT_CANCELLED)s.cancels++;}
static uint8_t entry(HirselCoreContext *ctx,const HirselCoreProcessConfig *cfg,HirselCoreMainResult *out) {s.root=ctx;return s.entry(ctx,cfg,out);}
uint8_t __wrap_hirsel_core_os_run_with_sources(const HirselCoreOsConfig *input,const HirselCoreView *names,const HirselCoreSourceTable *sources,HirselCoreEntry main) {
    (void)names;(void)sources;CHECK(input->args_len==1 && input->parameters_len==4);
    CHECK(input->args[0].len<sizeof s.mode);memcpy(s.mode,input->args[0].data,input->args[0].len);
    if(!strncmp(s.mode,"s0-",3)) return s0_case();
    s.entry=main;memcpy(s.file,is("read")?"abc":"old",3);s.length=3;
    HirselCoreProvider provider={&s,ready_wait,issue,never_resume,never_abort,deliver};
    HirselCoreEntryGrant caps[4];uint64_t grants[4];
    for(size_t i=0;i<4;i++) {uint8_t k=input->parameters[i].kind;caps[i]=(HirselCoreEntryGrant){k,&provider,k==0?7:0};grants[i]=i+1;}
    HirselCoreBuffer args[1];for(size_t i=0;i<1;i++) args[i]=(HirselCoreBuffer){(uint8_t*)input->args[i].data,input->args[i].len,0};
    HirselCoreProcessConfig cfg={0};cfg.root_limit=input->root_limit;cfg.args=args;cfg.args_len=1;
    cfg.grants=grants;cfg.grants_len=4;cfg.provider=&provider;cfg.state=&s;cfg.diagnostic=diagnostic;
    cfg.scheduler=(HirselCoreSchedulerProvider){&s,event,0};cfg.stop_fd=2;cfg.capabilities=caps;cfg.capabilities_len=4;
    if(crash_mode()) { s.crash_cut=(unsigned)strtoul(s.mode+5,NULL,10); s.crash_pending=s.crash_cut==0; }
    uint8_t status=hirsel_core_run(&cfg,entry);
    if(crash_mode() && s.crashed) {
        s.live=0; s.live_name=s.durable_name; memcpy(s.file,s.crash_file,s.crash_length); s.length=s.crash_length;
        args[0].data=(uint8_t*)"crash-read"; args[0].len=10;
        status=hirsel_core_run(&cfg,entry);
    }
    CHECK(status==0 && !s.live && !s.wait_active);
    if(is("before-read"))CHECK(s.reads==0 && s.cancels==1);
    if(issued())CHECK(s.cancels==1 && (strstr(s.mode,"write")?s.writes:s.reads)==1);
    if(blocked())CHECK(s.blocked==1 && s.cancels==1);
    if(is("read"))CHECK(s.cursor==3 && s.reads==5);
    if(is("errors"))CHECK(s.opens==6 && s.length==3);
    if(!strncmp(s.mode,"append",6))CHECK(s.writes==4 && s.length==7 && !memcmp(s.file,"oldxyz!",7));
    if(is("sync"))CHECK(s.syncs==1 && s.sync_dirs==1 && !s.live);
    if(is("sync-fail"))CHECK(s.syncs==1 && s.sync_dirs==0 && !s.live);
    return status;
}
