/* A helper of another build: it answers READY with an identity no Hirsel
 * build has, then waits for the embedder to close its socket (EMB-3). The
 * frame is little-endian u32 length, tag 0x81, then a u32-length string. */
#include <stdint.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    static const char identity[] = "0.0.0-fake";
    uint32_t text = sizeof identity - 1;
    uint32_t length = 1 + 4 + text;
    uint8_t frame[64];
    memcpy(frame, &length, 4);
    frame[4] = 0x81;
    memcpy(frame + 5, &text, 4);
    memcpy(frame + 9, identity, text);
    if (write(3, frame, 4 + length) != (ssize_t)(4 + length)) return 1;
    char byte;
    while (read(3, &byte, 1) > 0) {}
    return 0;
}
