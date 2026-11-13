/* Native runtime ABI lifecycle costs: one million decisions, no recorder or peer.
   Each successful send is paired with receive; Full keeps one queued value.
   Build/link at the same flags against before/after production archives. */
#include "hirsel_core_rt.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
static unsigned operation;
static uint64_t count;
static uint8_t inert(HirselCoreContext *ctx, void *p) { (void)ctx; (void)p; return 0; }
static uint8_t entry(HirselCoreContext *ctx, const HirselCoreProcessConfig *cfg, HirselCoreMainResult *result) {
    (void)cfg; (void)result;
    HirselCoreValueLayout layout={8,8,8,inert};
    HirselCoreChannelNew c=hirsel_core_channel_new(ctx,1,layout,0);
    if(!c.sender || c.status) abort();
    uint64_t x=7, y=0, checksum=0;
    if(operation==2 && hirsel_core_try_send(c.sender,&x)!=0) abort();
    for(uint64_t i=0;i<count;i++) {
        if(operation==3) {
            if(hirsel_core_check_cancel(ctx,3)) abort();
        } else if(operation==0) {
            HirselCoreTransfer s=hirsel_core_send(ctx,c.sender,&x,1);
            if(!s.transferred || s.status) abort();
        } else {
            uint8_t s=hirsel_core_try_send(c.sender,&x);
            if(s!=(operation==2 ? 1 : 0)) abort();
        }
        if(operation<2) {
            HirselCoreTransfer r=hirsel_core_receive(ctx,c.receiver,&y,2);
            if(!r.transferred || r.status || y!=7) abort();
        }
        checksum+=operation>=2 ? x : y;
    }
    hirsel_core_sender_drop(c.sender);
    if(hirsel_core_receiver_drop(ctx,c.receiver)) abort();
    printf("%llu\n",(unsigned long long)checksum);
    return 0;
}
static void diagnostic(void *s,const uint8_t *p,size_t n) { (void)s; fwrite(p,1,n,stderr); }
int main(int argc,char **argv) {
    if(argc!=3) return 2;
    operation=strcmp(argv[1],"send")==0 ? 0 : strcmp(argv[1],"ok")==0 ? 1 : strcmp(argv[1],"full")==0 ? 2 : 3;
    count=strtoull(argv[2],0,10);
    HirselCoreProcessConfig config={.root_limit=1000000,.diagnostic=diagnostic};
    return hirsel_core_run(&config,entry);
}
