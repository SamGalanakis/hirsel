/* runner_maxrss PROGRAM ARG: run PROGRAM ARG and print "status maxrss_kb". A child's
 * ru_maxrss starts from its parent's high-water at fork, so the caller (a Python harness
 * of ~30 MB) cannot measure a smaller program directly; this launcher is small, and its
 * fresh image after exec is the floor its child inherits. */
#include <stdio.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    pid_t pid = fork();
    if (pid < 0) return 3;
    if (pid == 0) {
        execl(argv[1], argv[1], argv[2], (char *)0);
        _exit(127);
    }
    int status = 0;
    struct rusage usage;
    if (wait4(pid, &status, 0, &usage) != pid) return 4;
    printf("%d %ld\n", status, usage.ru_maxrss);
    return 0;
}
