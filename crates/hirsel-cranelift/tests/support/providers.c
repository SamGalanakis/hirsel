/* Scripted callbacks beneath the real FORM #50 adapters and process runtime.
   Sample expectations come from providers.core; matrix words deliberately differ
   in every field so C padding, widths and waits-only status handling are visible. */
#include "hirsel_core_rt.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
extern void hirsel_core_test_reset_limit(HirselCoreContext *,uint64_t);
extern void hirsel_core_test_inject(HirselCoreContext *,size_t,bool);
extern uint64_t hirsel_core_test_available(const HirselCoreContext *);
extern size_t hirsel_core_test_attempts(const HirselCoreContext *);
struct world {
    HirselCoreContext *ctx;
    uint64_t limit, operation;
    int matrix, failing, seeding, exists, directory;
    uint8_t file[16], out[64], err[64];
    size_t len, cursor, out_len, err_len;
    size_t issues, deliveries;
};
static struct world world;
static uint8_t operand_bytes[64]="abc";
static int is_file(uint8_t op) { return op>=1 && op<=5; }
static int has_operand(uint8_t op) {
    return op==0 || op==1 || op==2 || (op>=6 && op<=9) || op>=12;
}
static int has_path(uint8_t op) { return op==0 || op==6 || op==8 || op==9; }
static void require(int condition) { if(!condition) abort(); }
static int path_is(HirselCoreHostRequest *r,const char *s) {
    return r->path_len==strlen(s) && !memcmp(r->path,s,r->path_len);
}
static HirselCoreHostOutcome issue(void *state,HirselCoreHostRequest *r) {
    struct world *w=state;
    if(w->seeding) return (HirselCoreHostOutcome){.value=111};
    if(w->matrix==2) { require(r->operation==3); return (HirselCoreHostOutcome){0}; }
    w->issues++;
    if(w->matrix) {
        uint8_t op=r->operation;
        require(op==w->operation && r->site==0);
        require(r->mode==(op==0 ? 3 : 0));
        require(r->handle==(is_file(op) ? 111 : 7));
        require(r->buffer.data==(has_operand(op) ? operand_bytes : NULL));
        require(r->buffer.len==(has_operand(op) ? 3 : 0));
        require(r->buffer.capacity==(has_operand(op) ? 64 : 0));
        require(r->path==(has_path(op) ? operand_bytes : NULL));
        require(r->path_len==(has_path(op) ? 3 : 0));
        if(op==1 || op==7) {
            r->buffer.data[r->buffer.len++]='X';
        }
        uint8_t status=(op==10 || op==11 || w->failing) ? 1 : 0;
        if(w->failing && op!=10 && op!=11)
            hirsel_core_raise(w->ctx,hirsel_core_cause_new(5,0));
        return (HirselCoreHostOutcome){
            .value=UINT64_C(0x1011121314151617),.error={5,UINT32_C(0x89abcdef)},
            .written=UINT64_C(0x2122232425262728),.status=status};
    }
    switch(r->operation) {
    case 0:
        require(r->handle==7 && path_is(r,"log"));
        if(r->mode==3) {
            if(w->exists) return (HirselCoreHostOutcome){.error={3,0}};
            w->exists=1; return (HirselCoreHostOutcome){.value=1};
        }
        require(r->mode==0 && w->exists); w->cursor=0;
        return (HirselCoreHostOutcome){.value=2};
    case 1: {
        require(r->handle==2 && r->buffer.capacity>=r->buffer.len);
        size_t n=w->len-w->cursor, room=r->buffer.capacity-r->buffer.len;
        if(n>room) n=room;
        memcpy(r->buffer.data+r->buffer.len,w->file+w->cursor,n);
        r->buffer.len+=n; w->cursor+=n;
        return (HirselCoreHostOutcome){.value=n};
    }
    case 2:
        require(r->handle==1 && r->buffer.len<=sizeof(w->file));
        memcpy(w->file,r->buffer.data,r->buffer.len); w->len=r->buffer.len;
        return (HirselCoreHostOutcome){.written=r->buffer.len};
    case 3: require(r->handle==1 || r->handle==2); break;
    case 6:
        require(r->handle==99 && path_is(r,"x"));
        if(w->directory) return (HirselCoreHostOutcome){.error={3,0}};
        w->directory=1; break;
    case 7:
        require(r->handle==7 && r->buffer.len==0 && r->buffer.capacity>=32);
        memcpy(r->buffer.data,"t0",2); r->buffer.len=2;
        return (HirselCoreHostOutcome){.value=99};
    case 12: case 13: {
        require(r->handle==0 && r->buffer.capacity==0);
        uint8_t *out=r->operation==12 ? w->out : w->err;
        size_t *len=r->operation==12 ? &w->out_len : &w->err_len;
        require(*len+r->buffer.len<=64);
        memcpy(out+*len,r->buffer.data,r->buffer.len); *len+=r->buffer.len;
        return (HirselCoreHostOutcome){.written=r->buffer.len};
    }
    default: abort();
    }
    return (HirselCoreHostOutcome){0};
}
static HirselCoreHostOutcome deliver(void *state,HirselCoreHostRequest *r,HirselCoreHostOutcome out) {
    struct world *w=state;
    if(!w->seeding && w->matrix!=2) w->deliveries++;
    return out;
}
static void diagnostic(void *s,const uint8_t *p,size_t n) { fwrite(p,1,n,stderr); abort(); }
static void put(uint64_t n) { require(fwrite(&n,8,1,stdout)==1); }
static void emit(HirselCoreContext *ctx,uint8_t status,const uint64_t *values,size_t n) {
    put(status!=0);
    HirselCoreCauseCell *causes=NULL;
    if(status) {
        causes=hirsel_core_causes_take(ctx); size_t count=0;
        for(const HirselCoreCauseCell *c=causes;c;c=hirsel_core_cause_next(c)) count++;
        put(count);
        for(const HirselCoreCauseCell *c=causes;c;c=hirsel_core_cause_next(c))
            put(hirsel_core_cause_kind(hirsel_core_cause_value(c)));
    } else { put(n); for(size_t i=0;i<n;i++) put(values[i]); }
    put(world.limit-hirsel_core_test_available(ctx));
    put(hirsel_core_test_attempts(ctx));
    put(world.out_len); for(size_t i=0;i<world.out_len;i++) put(world.out[i]);
    put(world.err_len); for(size_t i=0;i<world.err_len;i++) put(world.err[i]);
    require(world.issues==world.deliveries);
    hirsel_core_causes_drop(causes);
    if(world.matrix) world.matrix=2;
}
