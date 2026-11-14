/* A valid allocator policy for the shadow oracle: make every successful resize
   move, so stale data copy-back cannot hide behind an in-place realloc. */
#include <stdlib.h>
#include <malloc.h>
#include <string.h>
void *realloc(void *old, size_t bytes) {
    if (!old) return malloc(bytes);
    if (!bytes) { free(old); return NULL; }
    void *next = malloc(bytes);
    if (!next) return NULL;
    size_t count = malloc_usable_size(old);
    if (count > bytes) count = bytes;
    memcpy(next, old, count);
    free(old);
    return next;
}
