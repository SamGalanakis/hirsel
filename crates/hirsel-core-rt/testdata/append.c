#define _GNU_SOURCE
#include "hirsel_core_rt.h"
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <linux/perf_event.h>
#include <sys/syscall.h>
#include <unistd.h>
typedef struct {uint64_t value;uint8_t failed;} Answer;
extern Answer BODY(HirselCoreContext*,uint64_t,uint64_t);
static uint64_t grows;
HirselCoreGrowth __real_hirsel_core_grow(const HirselCoreContext*,void*,size_t,size_t,uint64_t,uint64_t,size_t,uint64_t,bool,uint8_t);
HirselCoreGrowth __wrap_hirsel_core_grow(const HirselCoreContext*c,void*p,size_t b,size_t a,uint64_t k,uint64_t n,size_t m,uint64_t r,bool o,uint8_t t) {grows++;return __real_hirsel_core_grow(c,p,b,a,k,n,m,r,o,t);}
static uint64_t count(int fd) {uint64_t v[3];assert(read(fd,v,sizeof(v))==sizeof(v));assert(v[1]==v[2]);return v[0];}
int main(void) {
 HirselCoreContext *ctx=hirsel_core_context_new(UINT64_MAX);assert(hirsel_core_task_start(ctx).pointer);hirsel_core_task_bind(ctx);
 struct perf_event_attr a={.type=PERF_TYPE_HARDWARE,.size=sizeof(a),.config=PERF_COUNT_HW_INSTRUCTIONS,.read_format=PERF_FORMAT_TOTAL_TIME_ENABLED|PERF_FORMAT_TOTAL_TIME_RUNNING,.pinned=1,.exclude_kernel=1,.exclude_hv=1};
 int fd=syscall(__NR_perf_event_open,&a,0,-1,-1,PERF_FLAG_FD_CLOEXEC);assert(fd>=0);
 uint64_t n=1024,calls=10000,sum=0,start=count(fd);
 for(uint64_t i=0;i<calls;i++){Answer v=BODY(ctx,n,0);assert(!v.failed&&v.value==EXPECTED);sum+=v.value;}
 uint64_t instructions=count(fd)-start;assert(!grows);
 printf("{\"instructions\":%llu,\"calls\":%llu,\"pushes_per_call\":%llu,\"checksum\":%llu,\"grows\":%llu,\"instructions_per_push\":%.6f}\n",(unsigned long long)instructions,(unsigned long long)calls,(unsigned long long)n,(unsigned long long)sum,(unsigned long long)grows,(double)instructions/(calls*n));
 hirsel_core_task_bind(NULL);hirsel_core_context_free(ctx);return 0;
}
