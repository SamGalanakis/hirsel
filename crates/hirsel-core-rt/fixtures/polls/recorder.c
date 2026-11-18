/* Test-only link wrapper. Records events under FIFO or T1's last-runnable script. */
#include "hirsel_core_rt.h"
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
uint8_t __real_hirsel_core_run(const HirselCoreProcessConfig*,HirselCoreEntry);
#ifdef SCRIPTED_SCHEDULER
extern uint8_t hirsel_core_test_run_last_with_runner(
    uint8_t (*)(const HirselCoreProcessConfig *, HirselCoreEntry),
    const HirselCoreProcessConfig *, HirselCoreEntry);
#endif
uint8_t __wrap_hirsel_core_run(const HirselCoreProcessConfig *input,HirselCoreEntry entry) {
    const char *file=getenv("HIRSEL_POLL_TRACE");
    if(!file) return __real_hirsel_core_run(input,entry);
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
#ifdef SCRIPTED_SCHEDULER
    if (getenv("HIRSEL_POLL_LAST") != NULL)
        result = hirsel_core_test_run_last_with_runner(__real_hirsel_core_run, &cfg, entry);
    else
#endif
        result = __real_hirsel_core_run(&cfg, entry);
    if(replay) { int c; do {c=fgetc(trace);} while(c=='\n' || c==' '); if(c!=EOF) abort(); }
#ifdef PENDING_STDOUT
    free((void *)cfg.capabilities);
#endif
    fclose(trace);
    return result;
}
