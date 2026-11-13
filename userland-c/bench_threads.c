/* Copyright (c) 2026 tre4surehunter9
 * bench_threads: how much faster 4 threads of one process go on all cores
 * than on one (sched_setaffinity to core 0), for three kinds of work:
 * arithmetic (no system calls), clock_gettime (answered without the
 * kernel lock) and getppid (a call that still takes the kernel lock). */
#define _GNU_SOURCE
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
#include <stdint.h>
#include <time.h>
#define THREADS 4
#define WORK 150000000UL
#define CALLS 1000000UL
#include <unistd.h>
static int pin, mode; /* 0: arithmetic, 1: clock_gettime, 2: getppid */
static volatile uint64_t sink;
static void *work(void *arg)
{
    (void)arg;
    if (pin) {
        cpu_set_t s;
        CPU_ZERO(&s);
        CPU_SET(0, &s);
        sched_setaffinity(0, sizeof s, &s);
    }
    if (mode == 1) {
        struct timespec ts;
        for (unsigned long i = 0; i < CALLS; i++)
            clock_gettime(CLOCK_MONOTONIC, &ts);
        return NULL;
    }
    if (mode == 2) {
        for (unsigned long i = 0; i < CALLS; i++)
            getppid();
        return NULL;
    }
    uint64_t x = 88172645463325252ULL;
    for (unsigned long i = 0; i < WORK; i++) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
    }
    sink += x;
    return NULL;
}
static double run(int p)
{
    pin = p;
    struct timespec a, b;
    clock_gettime(CLOCK_MONOTONIC, &a);
    pthread_t t[THREADS];
    for (int i = 0; i < THREADS; i++)
        pthread_create(&t[i], NULL, work, NULL);
    for (int i = 0; i < THREADS; i++)
        pthread_join(t[i], NULL);
    clock_gettime(CLOCK_MONOTONIC, &b);
    return (b.tv_sec - a.tv_sec) + (b.tv_nsec - a.tv_nsec) / 1e9;
}
int main(void)
{
    const char *names[] = { "arithmetic", "clock_gettime x 1M", "getppid x 1M" };
    for (mode = 0; mode < 3; mode++) {
        double one = run(1), all = run(0);
        printf("bench_threads: %s, %d threads: %.2f s on one core, %.2f s on all: %.1fx\n", names[mode], THREADS, one, all, one / all);
    }
    return 0;
}
