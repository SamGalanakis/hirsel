#include <spawn.h>
int main(int c, char **v) { int fd; return pidfd_spawnp(&fd, v[0], 0, 0, v, 0); }
