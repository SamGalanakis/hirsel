extern int pidfd_spawnp(void) __attribute__((weak));
int main(void) { return pidfd_spawnp ? pidfd_spawnp() : 0; }
