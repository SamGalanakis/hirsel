#include <inttypes.h>
#include <libproc.h>
#include <mach/mach_time.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    int pid = atoi(argv[1]);
    uint64_t marker = strtoull(argv[2], NULL, 10);
    struct rusage_info_v0 usage;
    mach_timebase_info_data_t timebase;
    if (proc_pid_rusage(pid, RUSAGE_INFO_V0, (rusage_info_t *)&usage) != 0) {
        perror("proc_pid_rusage");
        return 1;
    }
    if (usage.ri_proc_start_abstime == 0 || usage.ri_proc_start_abstime > marker ||
        mach_timebase_info(&timebase) != KERN_SUCCESS) return 1;
    double ns = (double)(marker - usage.ri_proc_start_abstime) * timebase.numer / timebase.denom;
    printf("%.3f\n", ns / 1e6);
    return 0;
}
