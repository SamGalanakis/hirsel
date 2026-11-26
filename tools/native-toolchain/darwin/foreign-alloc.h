#include <stdlib.h>

/* Give the codec's allocation calls the same names as ELF --wrap.
 * Include the declarations first so the adapter still uses the system API. */
#if defined(HIRSEL_WRAP_CALLOC)
extern __typeof__(calloc) calloc __asm("___wrap_calloc");
#endif
extern __typeof__(malloc) malloc __asm("___wrap_malloc");
extern __typeof__(free) free __asm("___wrap_free");
