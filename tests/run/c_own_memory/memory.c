// The C half of `main.lilac`: a freestanding library that brings its own
// memory functions, as SDL built with `SDL_LIBC=OFF` does. A native Lilac
// object defines them too, weakly, so these link and win.
#include <stddef.h>
#include <stdint.h>

static uint32_t copies;

void *memcpy(void *dest, const void *src, size_t len) {
    copies++;
    unsigned char *to = dest;
    const unsigned char *from = src;
    for (size_t i = 0; i < len; i++) to[i] = from[i];
    return dest;
}

void *memmove(void *dest, const void *src, size_t len) {
    unsigned char *to = dest;
    const unsigned char *from = src;
    if (to < from) {
        for (size_t i = 0; i < len; i++) to[i] = from[i];
    } else {
        for (size_t i = len; i > 0; i--) to[i - 1] = from[i - 1];
    }
    return dest;
}

void *memset(void *dest, int value, size_t len) {
    unsigned char *to = dest;
    for (size_t i = 0; i < len; i++) to[i] = (unsigned char)value;
    return dest;
}

int memcmp(const void *left, const void *right, size_t len) {
    const unsigned char *a = left, *b = right;
    for (size_t i = 0; i < len; i++) {
        if (a[i] != b[i]) return a[i] - b[i];
    }
    return 0;
}

typedef struct { uint8_t bytes[4096]; } Page;

void page_fill(Page *page, uint8_t value) {
    for (size_t i = 0; i < sizeof page->bytes; i++) page->bytes[i] = value;
}

uint32_t page_sum(const Page *page) {
    uint32_t sum = 0;
    for (size_t i = 0; i < sizeof page->bytes; i++) sum += page->bytes[i];
    return sum;
}

uint32_t memcpy_calls(void) { return copies; }
