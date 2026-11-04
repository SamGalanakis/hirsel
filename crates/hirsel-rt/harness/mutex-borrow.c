/* Optimized-runtime witness for a contended Free take's migrating retry.
 * The independent oracle is eight Some(1) results and payload 7 + 8 = 15. */
#include "hirsel_rt_v2.h"
#include <assert.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef uint8_t (*task_code)(hirsel_rt_v2_ctx *, hirsel_rt_v2_header *, uint64_t, uint64_t);
extern uint8_t hirsel_rt_v2_task_scope_enter(hirsel_rt_v2_ctx *, void *, uint64_t *, const uint64_t *);
extern uint8_t hirsel_rt_v2_task_scope_exit(hirsel_rt_v2_ctx *, uint64_t, uint64_t, const uint64_t *, uint8_t);
extern uint8_t hirsel_rt_v2_task_spawn(hirsel_rt_v2_ctx *, uint64_t, uint64_t, void *, uint64_t *, const uint64_t *, task_code, hirsel_rt_v2_header *, uint64_t);
extern uint8_t hirsel_rt_v2_task_join(hirsel_rt_v2_ctx *, uint64_t, uint64_t, const uint64_t *, uint64_t *, uint8_t *, uint64_t);
extern uint8_t hirsel_rt_v2_task_finish(hirsel_rt_v2_ctx *, uint8_t *, const uint8_t *, uint64_t, uint32_t);
extern uint8_t hirsel_rt_v2_mutex_create(hirsel_rt_v2_ctx *, void *, uint64_t *, uint8_t *, const uint8_t *, uint32_t, uint32_t);
extern uint8_t hirsel_rt_v2_mutex_take(hirsel_rt_v2_ctx *, uint32_t *, const uint64_t *, uint8_t *);
extern uint8_t hirsel_rt_v2_mutex_put(hirsel_rt_v2_ctx *, const uint64_t *, uint8_t *);
extern uint8_t hirsel_rt_v2_task_canceled(hirsel_rt_v2_ctx *, uint32_t *, const uint64_t *);
static const uint64_t none[2] = {0, 0};
static const hirsel_rt_v2_layout scalar = {.magic=HIRSEL_RT_V2_LAYOUT_MAGIC,.version=HIRSEL_RT_V2_LAYOUT_VERSION,.size=16,.align=8};
static const hirsel_rt_v2_step close_mutex = {.kind=HIRSEL_RT_V2_STEP_CLOSE_HANDLE,.field=HIRSEL_RT_V2_WHOLE_VALUE};
static const hirsel_rt_v2_layout wrapper = {.magic=HIRSEL_RT_V2_LAYOUT_MAGIC,.version=HIRSEL_RT_V2_LAYOUT_VERSION,.size=8,.align=8,.flags=HIRSEL_RT_V2_LAYOUT_FROZEN|HIRSEL_RT_V2_LAYOUT_MUTEX_ROOT,.steps=&close_mutex,.step_count=1};
static const hirsel_rt_v2_layout *layouts[] = {&scalar, &wrapper};
#define ROUNDS 1u
static atomic_uint started;
static uint8_t waiter(hirsel_rt_v2_ctx *ctx, hirsel_rt_v2_header *env, uint64_t caller, uint64_t site) {
    (void)caller; (void)site;
    const uint64_t handle = *(uint64_t *)(env + 1);
    atomic_fetch_add_explicit(&started, 1, memory_order_release);
    for (unsigned i=0;i<ROUNDS;i++) {
        uint64_t payload[2] = {0, 0}; uint32_t dead = 99;
        assert(hirsel_rt_v2_mutex_take(ctx,&dead,&handle,(uint8_t *)payload)==0);
        assert(dead==0 && payload[0]==1);
        payload[1]++;
        assert(hirsel_rt_v2_mutex_put(ctx,&handle,(uint8_t *)payload)==0);
        assert(payload[0]==0 && payload[1]==0);
    }
    uint64_t result[2] = {1, ROUNDS};
    assert(hirsel_rt_v2_task_finish(ctx,(uint8_t *)result,(const uint8_t *)none,16,UINT32_MAX)==0);
    return 0;
}
static uint8_t entry(hirsel_rt_v2_ctx *ctx, uint8_t *destination, uint8_t *out, uint64_t *roots) {
    (void)destination; (void)roots;
    uint64_t scope=0, tasks=7, handle=0, payload[2]={1,7};
    uint32_t dead=99;
    assert(hirsel_rt_v2_mutex_create(ctx,0,&handle,(uint8_t *)payload,(const uint8_t *)none,2,3)==0);
    assert(hirsel_rt_v2_task_scope_enter(ctx,0,&scope,&tasks)==0);
    assert(hirsel_rt_v2_mutex_take(ctx,&dead,&handle,(uint8_t *)payload)==0);
    assert(dead==0 && payload[0]==1 && payload[1]==7);
    uint64_t children[8]={0};
    for (unsigned i=0;i<8;i++) {
        hirsel_rt_v2_header *env=hirsel_rt_v2_alloc(ctx,2,16,8,HIRSEL_RT_V2_DOMAIN_HEAP,0);
        *(uint64_t *)(env+1)=handle;
        assert(hirsel_rt_v2_task_spawn(ctx,0,0,0,&children[i],&scope,waiter,env,0)==0);
    }
    /* Root retains the payload until eight children have started. Root plus
     * eight children exceeds every tested width, so at least one take parks. */
    while (atomic_load_explicit(&started, memory_order_acquire)!=8) {
        uint32_t canceled=99;
        assert(hirsel_rt_v2_task_canceled(ctx,&canceled,&scope)==0);
        assert(canceled==0);
    }
    assert(hirsel_rt_v2_mutex_put(ctx,&handle,(uint8_t *)payload)==0);
    for (unsigned i=0;i<8;i++) {
        uint64_t result[2]={0,0};
        assert(hirsel_rt_v2_task_join(ctx,0,0,&scope,&children[i],(uint8_t *)result,16)==0);
        assert(result[0]==1 && result[1]==ROUNDS);
    }
    assert(hirsel_rt_v2_task_scope_exit(ctx,0,0,&scope,0)==0);
    dead=99;
    assert(hirsel_rt_v2_mutex_take(ctx,&dead,&handle,(uint8_t *)payload)==0);
    assert(dead==0 && payload[0]==1 && payload[1]==7+8*ROUNDS);
    assert(hirsel_rt_v2_mutex_put(ctx,&handle,(uint8_t *)payload)==0);
    assert(hirsel_rt_v2_root_drop(ctx,(hirsel_rt_v2_header *)(uintptr_t)handle,0,0)==0);
    memset(out,0,4);
    printf("mutex total %u\n",7+8*ROUNDS);
    return 0;
}
int main(int argc, char **argv) {
    const uint8_t failures[8]={0};
    hirsel_rt_v2_program_descriptor program={.abi_version=HIRSEL_RT_V2_ABI_VERSION,.flags=HIRSEL_RT_V2_PROGRAM_STANDALONE,.roots=1,.layout_count=2,.entry=entry,.result_size=4,.layouts=layouts,.failure_table=failures,.failure_table_length=8,.entry_name=(const uint8_t *)"mutex_borrow@1/main",.entry_name_length=18};
    return hirsel_rt_v2_main(&program,argc,(const char *const *)argv);
}
