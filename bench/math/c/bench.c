/* The C library's elementary functions on the inputs `bench/math` uses, for
 * comparison. Hand-run, not a gate:
 *
 *     clang -O2 bench/math/c/bench.c -o build/math-c
 *     build/math-c
 *
 * Each line is the mean time of one call in nanoseconds. */

#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <time.h>

#define COUNT 4096
#define ROUNDS 64

static double inputs[COUNT];
static uint64_t state;
volatile double sink;

/* The generator of `bench/math/main.lilac`. */
static double next(void) {
    state = state * 0x5851F42D4C957F2DULL + 0x14057B7EF767814FULL;
    return (double)(state >> 11) * 1.1102230246251565e-16;
}

static void fill(double low, double high) {
    state = 0x2545F4914F6CDD1DULL;
    for (int i = 0; i < COUNT; i++) inputs[i] = low + (high - low) * next();
}

static double seconds(void) {
    struct timespec t;
    timespec_get(&t, TIME_UTC);
    return (double)t.tv_sec + (double)t.tv_nsec * 1e-9;
}

static void run(const char *name, double (*f)(double), double low, double high) {
    fill(low, high);
    double total = 0.0;
    double start = seconds();
    for (int r = 0; r < ROUNDS; r++)
        for (int i = 0; i < COUNT; i++) total += f(inputs[i]);
    double end = seconds();
    sink = total;
    printf("%-10s %8.1f ns\n", name, (end - start) * 1e9 / (COUNT * ROUNDS));
}

int main(void) {
    run("exp", exp, -700, 700);
    run("exp_m1", expm1, -1, 1);
    run("ln", log, 1e-300, 1e300);
    run("ln_1p", log1p, -0.5, 1);
    run("log2", log2, 0.001, 1000);
    run("sin", sin, -6, 6);
    run("sin_big", sin, 1e3, 1e6);
    run("sin_huge", sin, 1e20, 1e300);
    run("cos", cos, -6, 6);
    run("tan", tan, -1.5, 1.5);
    run("atan", atan, -10, 10);
    run("asin", asin, -1, 1);
    run("sinh", sinh, -5, 5);
    run("tanh", tanh, -5, 5);
    run("asinh", asinh, -5, 5);
    run("cbrt", cbrt, -1000, 1000);
    run("erf", erf, -3, 3);
    run("gamma", tgamma, 0.5, 20);
    return 0;
}
