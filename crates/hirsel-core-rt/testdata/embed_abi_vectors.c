/* Tiny C driver to print `HEADER_V1` or execute shared 3145 kernel transition vectors. */
#include "hirsel_embed.h"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern hirsel_bytes embed_abi_fixture_header(void);
extern intptr_t embed_abi_fixture_run_kernel_vectors(
    const uint8_t *input, size_t input_len, uint8_t *output, size_t output_cap);

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--header") == 0) {
        hirsel_bytes h = embed_abi_fixture_header();
        if (h.len > 0 && fwrite(h.ptr, 1, h.len, stdout) != h.len) {
            return 2;
        }
        return 0;
    }
    size_t cap = 65536, len = 0;
    uint8_t *buf = malloc(cap);
    if (!buf) return 3;
    for (;;) {
        if (len == cap) {
            cap *= 2;
            buf = realloc(buf, cap);
            if (!buf) return 3;
        }
        size_t n = fread(buf + len, 1, cap - len, stdin);
        len += n;
        if (n == 0) break;
    }
    size_t out_cap = 262144;
    uint8_t *out = malloc(out_cap);
    if (!out) return 3;
    intptr_t written = embed_abi_fixture_run_kernel_vectors(buf, len, out, out_cap);
    if (written < 0) {
        fprintf(stderr, "kernel_vectors error: %ld\n", (long)written);
        return 4;
    }
    if (written > 0 && fwrite(out, 1, (size_t)written, stdout) != (size_t)written) {
        return 5;
    }
    free(out);
    free(buf);
    return 0;
}
