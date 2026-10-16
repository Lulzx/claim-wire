#include <stdint.h>

uint64_t fib(uint64_t n) {
    uint64_t a = 0, b = 1;
    for (uint64_t i = 0; i < n; i++) {
        uint64_t t = a + b;
        a = b;
        b = t;
    }
    return a;
}
