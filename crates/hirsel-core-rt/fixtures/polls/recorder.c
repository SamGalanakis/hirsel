/* Test-only launch observer for a testing runtime. Records events under FIFO or
   T1's last-runnable script; registers itself as the runtime's launch before main. */
#include "hirsel_core_rt_host.h"
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <inttypes.h>
static FILE *trace;
static int replay;
static void event(void *s, HirselCoreEvent e) {
    (void)s;
    if(replay) {
        unsigned k; uint64_t t,o,v;
        if(fscanf(trace," E %u %"SCNu64" %"SCNu64" %"SCNu64,&k,&t,&o,&v)!=4 || k!=e.kind || t!=e.task || o!=e.object || v!=e.value) abort();
    } else fprintf(trace,"E %u %"PRIu64" %"PRIu64" %"PRIu64"\n",e.kind,e.task,e.object,e.value);
}
extern uint8_t hirsel_core_test_run_last(const HirselCoreProcessConfig *, HirselCoreEntry);
extern bool hirsel_core_test_set_launch(uint8_t (*)(const HirselCoreProcessConfig *, HirselCoreEntry));
static uint8_t observed_launch(const HirselCoreProcessConfig *input,HirselCoreEntry entry) {
    const char *file=getenv("HIRSEL_POLL_TRACE");
    if(!file) return hirsel_core_run(input,entry);
    replay=getenv("HIRSEL_POLL_REPLAY")!=NULL;
    trace=fopen(file,replay ? "r" : "w"); if(!trace) abort();
    setvbuf(trace,NULL,_IONBF,0);
    HirselCoreProcessConfig cfg=*input;
    cfg.scheduler=(HirselCoreSchedulerProvider){.event=event};
#ifdef PENDING_STDOUT
    extern void pending_stdout(HirselCoreProcessConfig *);
    pending_stdout(&cfg);
#endif
    uint8_t result;
    if (getenv("HIRSEL_POLL_LAST") != NULL)
        result = hirsel_core_test_run_last(&cfg, entry);
    else
        result = hirsel_core_run(&cfg, entry);
    if(replay) { int c; do {c=fgetc(trace);} while(c=='\n' || c==' '); if(c!=EOF) abort(); }
#ifdef PENDING_STDOUT
    free((void *)cfg.capabilities);
#endif
    fclose(trace);
    return result;
}
__attribute__((constructor)) static void install_observed_launch(void) {
    if(!hirsel_core_test_set_launch(observed_launch)) abort();
}
