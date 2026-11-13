static bool created, subdir_created;
static uint64_t clock_reads;
static HirselCoreHostOutcome scripted_issue(void *state, HirselCoreHostRequest *r) {
    (void)state; HirselCoreHostOutcome out={0};
    switch(r->operation) {
    case 0:
        assert(r->handle==70 && r->path_len==3 && !memcmp(r->path,"log",3));
        if(r->mode==3) {
            if(created) out.error.tag=3;
            else { created=true; out.value=40; }
        } else { assert(r->mode==0 && created); out.value=41; }
        break;
    case 1:
        assert(r->handle==41 && r->buffer.len==0 && r->buffer.capacity==8);
        memcpy(r->buffer.data,"hi",2); r->buffer.len=2; out.value=2;
        break;
    case 2:
        assert(r->handle==40 && r->buffer.len==2 && !memcmp(r->buffer.data,"hi",2));
        out.written=2;
        break;
    case 3: assert(r->handle==40 || r->handle==41); break;
    case 6:
        assert(r->handle==71 && r->path_len==1 && r->path[0]=='x');
        if(subdir_created) out.error.tag=3; else subdir_created=true;
        break;
    case 7:
        assert(r->handle==70 && r->buffer.len==0 && r->buffer.capacity>=32);
        memcpy(r->buffer.data,"t0",2); r->buffer.len=2; out.value=71;
        break;
    case 8:
        assert(r->handle==70 && r->path_len==3 && !memcmp(r->path,"log",3));
        out.error.tag=5; out.error.code=UINT32_C(0xdecafbad);
        break;
    case 10: case 11: assert(r->handle==30); out.value=clock_reads++; break;
    case 12:
        assert(r->handle==10 && r->buffer.len==5 && !memcmp(r->buffer.data,"hello",5));
        out.written=5;
        break;
    default: assert(0);
    }
    return out;
}
static HirselCoreHostOutcome scripted_deliver(void *state, HirselCoreHostRequest *r, HirselCoreHostOutcome out) {
    (void)state; (void)r; return out;
}
static const HirselCoreProvider scripted={.issue=scripted_issue,.deliver=scripted_deliver};
static const HirselCoreEntryGrant scripted_grants[7]={
    [0]={.kind=4,.provider=&scripted,.handle=10},
    [1]={.kind=10,.provider=&scripted},
    [2]={.kind=2,.provider=&scripted,.handle=30},
    [6]={.kind=0,.provider=&scripted,.handle=70}
};
static const HirselCoreBuffer scripted_args[]={
    {.data=(uint8_t*)"prog",.len=4},
    {.data=(uint8_t*)"hello",.len=5}
};
