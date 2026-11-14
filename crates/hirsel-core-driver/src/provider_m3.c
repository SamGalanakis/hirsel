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
    unsigned opens, reads, writes, live, cancels, blocked;
    unsigned char file[32]; size_t length, cursor;
    HirselCoreContext *root, *active, *waiting;
    HirselCoreTask *child;
    unsigned wait_active;
    HirselCoreEntry entry;
} s;
#define CHECK(c) do { if (!(c)) { dprintf(2,"provider fixture: line %u: %s\n",__LINE__,#c); _exit(120); } } while(0)
static int is(const char *m) { return !strcmp(s.mode,m); }
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
extern HirselCoreSpawn __real_hirsel_core_spawn(HirselCoreContext*,uint64_t,void*,HirselCoreTaskBody,HirselCoreTaskLayout);
HirselCoreSpawn __wrap_hirsel_core_spawn(HirselCoreContext *ctx,uint64_t bytes,void *input,HirselCoreTaskBody body,HirselCoreTaskLayout layout) {
    HirselCoreSpawn h=__real_hirsel_core_spawn(ctx,bytes,input,body,layout);
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
extern HirselCoreHostOutcome __real_hirsel_core_host_op(HirselCoreContext*,uint8_t,uint64_t,HirselCoreBuffer*,uint8_t,uint64_t);
HirselCoreHostOutcome __wrap_hirsel_core_host_op(HirselCoreContext *ctx,uint8_t op,uint64_t handle,HirselCoreBuffer *b,uint8_t mode,uint64_t site) {
    HirselCoreContext *old=s.active; s.active=ctx;
    HirselCoreHostOutcome out=__real_hirsel_core_host_op(ctx,op,handle,b,mode,site);
    s.active=old;return out;
}
static HirselCoreHostOutcome issue(void *state,HirselCoreHostRequest *r) {
    (void)state; HirselCoreHostOutcome out={0};
    if(issued() && r->operation==(!strstr(s.mode,"write")?1:2) && !strncmp(s.mode,"issue-",6)) {
        CHECK(s.active && s.active!=s.root && s.child);__real_hirsel_core_cancel(s.active,s.child);
    }
    switch(r->operation) {
    case 0:
        CHECK(r->handle==7 && r->path_len==1 && r->path[0]=='f');
        if(is("errors") && s.opens<5) {
            out.error.tag=(uint8_t)(++s.opens);out.error.code=out.error.tag==5?1234:0;return out;
        }
        s.opens++;CHECK(!s.live);s.live=1;s.cursor=0;out.value=17;
        if(!strncmp(s.mode,"append",6) || strstr(s.mode,"write")) CHECK(r->mode==(s.opens==1?2:0));
        return out;
    case 1: {
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
        CHECK(r->handle==17 && s.live);
        unsigned n=s.writes++;size_t count=r->buffer.len;
        if(issued()) {CHECK(n==0 && count==3);count=1;out.error.tag=5;out.error.code=5;}
        else if(n==0) {CHECK(!strncmp(s.mode,"append",6) && count==3);count=(size_t)(s.mode[6]-'0');out.error.tag=5;out.error.code=5;}
        else if(n==2) {CHECK(count==1);count=0;out.error.tag=2;}
        CHECK(s.length+count<=sizeof s.file);
        if(count) memcpy(s.file+s.length,r->buffer.data,count);
        s.length+=count;out.written=count;return out;
    }
    case 3: CHECK(r->handle==17 && s.live);s.live=0;return out;
    default: CHECK(0);
    }
    return out;
}
static HirselCoreHostOutcome deliver(void *state,HirselCoreHostRequest *r,HirselCoreHostOutcome out) {
    (void)state;
    if(issued() && r->operation==(!strstr(s.mode,"write")?1:2) && !strncmp(s.mode,"delivery-",9)) {
        CHECK(s.active && s.active!=s.root && s.child);__real_hirsel_core_cancel(s.active,s.child);
    }
    return out;
}
static void diagnostic(void *state,const uint8_t *data,size_t len) {(void)state;CHECK(write(2,data,len)==(ssize_t)len);}
static void event(void *state,HirselCoreEvent e) {(void)state;if(e.kind==3)s.cancels++;}
static uint8_t entry(HirselCoreContext *ctx,const HirselCoreProcessConfig *cfg,HirselCoreMainResult *out) {s.root=ctx;return s.entry(ctx,cfg,out);}
uint8_t __wrap_hirsel_core_os_run(const HirselCoreOsConfig *input,const HirselCoreView *names,HirselCoreEntry main) {
    (void)names;CHECK(input->args_len==1 && input->parameters_len==3);
    CHECK(input->args[0].len<sizeof s.mode);memcpy(s.mode,input->args[0].data,input->args[0].len);
    s.entry=main;memcpy(s.file,is("read")?"abc":"old",3);s.length=3;
    HirselCoreProvider provider={&s,issue,deliver};
    HirselCoreEntryGrant caps[3];uint64_t grants[3];
    for(size_t i=0;i<3;i++) {uint8_t k=input->parameters[i].kind;caps[i]=(HirselCoreEntryGrant){k,&provider,k==0?7:0};grants[i]=i+1;}
    HirselCoreBuffer args[1];for(size_t i=0;i<1;i++) args[i]=(HirselCoreBuffer){(uint8_t*)input->args[i].data,input->args[i].len,0};
    HirselCoreProcessConfig cfg={0};cfg.root_limit=input->root_limit;cfg.args=args;cfg.args_len=1;
    cfg.grants=grants;cfg.grants_len=3;cfg.provider=&provider;cfg.state=&s;cfg.diagnostic=diagnostic;
    cfg.scheduler=(HirselCoreSchedulerProvider){&s,event,0};cfg.stop_fd=2;cfg.capabilities=caps;cfg.capabilities_len=3;
    uint8_t status=hirsel_core_run(&cfg,entry);CHECK(status==0 && !s.live && !s.wait_active);
    if(is("before-read"))CHECK(s.reads==0 && s.cancels==1);
    if(issued())CHECK(s.cancels==1 && (strstr(s.mode,"write")?s.writes:s.reads)==1);
    if(blocked())CHECK(s.blocked==1 && s.cancels==1);
    if(is("read"))CHECK(s.cursor==3 && s.reads==5);
    if(is("errors"))CHECK(s.opens==6 && s.length==3);
    if(!strncmp(s.mode,"append",6))CHECK(s.writes==4 && s.length==7 && !memcmp(s.file,"oldxyz!",7));
    return status;
}
