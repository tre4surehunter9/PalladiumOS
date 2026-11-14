/* Copyright (c) 2026 tre4surehunter9
 * test_avx: AVX for programs. The OS must turn it on (XSAVE: xgetbv says
 * the YMM state is enabled) and keep each thread's 256-bit registers
 * across task switches and signal handlers:
 *  1. xgetbv lists SSE and AVX state, and an AVX instruction works;
 *  2. threads on every core each keep their own value in ymm15 while the
 *     others keep theirs (switches save and restore the upper halves);
 *  3. a signal handler that overwrites ymm15 leaves the interrupted code's
 *     value as it was (the signal frame carries the AVX state).
 * Inline assembly throughout (the program is built without -mavx); ymm15:
 * code compiled without AVX leaves it alone. */
#include <cpuid.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static int failures;

static void check(int ok, const char *what) {
    if (!ok) {
        printf("FAILED: %s\n", what);
        failures++;
    }
}

static void set_ymm15(uint64_t v) {
    uint64_t b[4] = {v, v ^ 0x1111, v ^ 0x2222, v ^ 0x3333};
    __asm__ volatile("vmovdqu %0, %%ymm15" : : "m"(b) : "xmm15");
}

static int ymm15_is(uint64_t v) {
    uint64_t b[4];
    __asm__ volatile("vmovdqu %%ymm15, %0" : "=m"(b));
    return b[0] == v && b[1] == (v ^ 0x1111) && b[2] == (v ^ 0x2222) && b[3] == (v ^ 0x3333);
}

static double now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec + ts.tv_nsec / 1e9;
}

static atomic_int bad_thread_reads;

static void *worker(void *arg) {
    uint64_t v = 0x0123456789abcdefULL * (uint64_t)(uintptr_t)arg;
    set_ymm15(v);
    double end = now() + 1.5;
    unsigned long n = 0;
    while (now() < end) {
        if (!ymm15_is(v))
            atomic_fetch_add(&bad_thread_reads, 1), set_ymm15(v);
        if (++n % 1000 == 0)
            sched_yield();
    }
    return NULL;
}

static volatile sig_atomic_t signals;

static void handler(int sig) {
    (void)sig;
    set_ymm15(0xdeaddeaddeaddeadULL); /* the handler's own use of ymm15 */
    signals++;
}

int main(void) {
    unsigned a, b, c, d;
    __cpuid(1, a, b, c, d);
    if (!(c & (1u << 28))) {
        printf("no AVX on this CPU: skipped; all checks passed\n");
        return 0;
    }
    /* 1. The OS has it on: OSXSAVE, and XCR0 with SSE (bit 1) and AVX (2). */
    check(c & (1u << 27), "CPUID says the OS turned XSAVE on (OSXSAVE)");
    unsigned lo = 0, hi = 0;
    if (c & (1u << 27))
        __asm__ volatile("xgetbv" : "=a"(lo), "=d"(hi) : "c"(0));
    check((lo & 6) == 6, "xgetbv: SSE and AVX state enabled");
    if (failures) {
        printf("%d check(s) failed\n", failures);
        return 1;
    }
    float x[8] = {1, 2, 3, 4, 5, 6, 7, 8}, y[8] = {8, 7, 6, 5, 4, 3, 2, 1}, z[8];
    __asm__ volatile("vmovups %1, %%ymm0\n\tvaddps %2, %%ymm0, %%ymm0\n\tvmovups %%ymm0, %0"
                     : "=m"(z) : "m"(x), "m"(y) : "xmm0");
    int sums = 1;
    for (int i = 0; i < 8; i++)
        sums &= z[i] == 9.0f;
    check(sums, "vaddps adds eight floats at once");

    /* 2. Threads, each with its own ymm15. */
    pthread_t t[6];
    for (uintptr_t i = 0; i < 6; i++)
        pthread_create(&t[i], NULL, worker, (void *)(i + 1));
    for (int i = 0; i < 6; i++)
        pthread_join(t[i], NULL);
    check(atomic_load(&bad_thread_reads) == 0, "every thread kept its own ymm15 across switches");
    if (atomic_load(&bad_thread_reads))
        printf("  (%d wrong reads)\n", atomic_load(&bad_thread_reads));

    /* 3. A timer signal 1000 times a second, its handler using ymm15. */
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = handler;
    sigaction(SIGALRM, &sa, NULL);
    struct itimerval it = {{0, 1000}, {0, 1000}};
    setitimer(ITIMER_REAL, &it, NULL);
    uint64_t mine = 0x5555aaaa5555aaaaULL;
    set_ymm15(mine);
    int wrong = 0;
    double end = now() + 1.0;
    while (now() < end)
        if (!ymm15_is(mine))
            wrong++, set_ymm15(mine);
    struct itimerval off = {{0, 0}, {0, 0}};
    setitimer(ITIMER_REAL, &off, NULL);
    check(signals > 10, "the timer signals arrived");
    check(wrong == 0, "ymm15 survived the signal handlers");
    if (wrong)
        printf("  (%d wrong reads after %d signals)\n", wrong, (int)signals);

    if (failures) {
        printf("%d check(s) failed\n", failures);
        return 1;
    }
    printf("AVX: %d signals, 6 threads; all checks passed\n", (int)signals);
    return 0;
}
