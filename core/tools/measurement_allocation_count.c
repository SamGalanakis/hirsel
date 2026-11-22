/* Linked into allocation-counting benchmarks. Report counts on stderr
 * just before the two phase markers reach the controller on stdout. */
#include <inttypes.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

uint64_t hirsel_bench_allocation_count(void);
ssize_t __real_write(int, const void *, size_t);

/* The runtime owns a duplicate of the stdout grant. */
static int writes_stdout(int fd) {
    struct stat stream, stdout_stream;
    return fstat(fd, &stream) == 0 && fstat(STDOUT_FILENO, &stdout_stream) == 0
        && stream.st_dev == stdout_stream.st_dev && stream.st_ino == stdout_stream.st_ino;
}

ssize_t __wrap_write(int fd, const void *bytes, size_t size) {
    if (size >= 5) {
        const char *phase = NULL;
        if (size == 6 && memcmp(bytes, "READY\n", 6) == 0)
            phase = "READY";
        else if (memcmp(bytes, "DONE ", 5) == 0)
            phase = "DONE";
        if (phase && writes_stdout(fd)) {
            char line[64];
            int length = snprintf(line, sizeof line, "BENCH_ALLOCATIONS %s %" PRIu64 "\n",
                                  phase, hirsel_bench_allocation_count());
            if (length > 0 && (size_t)length < sizeof line)
                (void)__real_write(STDERR_FILENO, line, (size_t)length);
        }
    }
    return __real_write(fd, bytes, size);
}
