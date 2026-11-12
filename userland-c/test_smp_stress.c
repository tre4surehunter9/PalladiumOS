/* Copyright (c) 2026 tre4surehunter9
 * test_smp_stress FILE [SECONDS]: run several copies at once on more
 * cores to look for anything that disturbs a process: each reads FILE at
 * random offsets (stdio, as games read their data) and compares with a
 * copy read at the start, keeps a big memory pattern and checks it, keeps
 * floating-point sums and checks them, and checks its pid and its file
 * descriptor. Prints the first difference and exits 1; 0 if none. */
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static volatile unsigned long ticks;
static void on_alarm(int sig)
{
    (void)sig;
    ticks++;
}

static double now(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

int main(int argc, char **argv)
{
    if (argc < 2) {
        fprintf(stderr, "usage: test_smp_stress FILE [SECONDS]\n");
        return 2;
    }
    double seconds = argc > 2 ? atof(argv[2]) : 20;
    pid_t pid = getpid();
    FILE *f = fopen(argv[1], "rb");
    if (!f) {
        perror(argv[1]);
        return 2;
    }
    struct stat st;
    fstat(fileno(f), &st);
    size_t size = st.st_size > (8 << 20) ? (8 << 20) : st.st_size;
    uint8_t *copy = malloc(size);
    if (fread(copy, 1, size, f) != size) {
        fprintf(stderr, "[%d] first read failed\n", pid);
        return 2;
    }
    size_t words = (4 << 20) / 8;
    uint64_t *pat = malloc(words * 8);
    for (size_t i = 0; i < words; i++)
        pat[i] = (uint64_t)pid << 40 ^ i * 0x9E3779B97F4A7C15ull;
    uint8_t *buf = malloc(65536);
    /* A timer signal 100 times a second (as an X server's scheduler). */
    struct sigaction sa = { .sa_handler = on_alarm, .sa_flags = SA_RESTART };
    sigaction(SIGALRM, &sa, NULL);
    struct itimerval it = { { 0, 10000 }, { 0, 10000 } };
    setitimer(ITIMER_REAL, &it, NULL);
    uint32_t seed = pid * 2654435761u;
    double start = now();
    unsigned long rounds = 0;
    while (now() - start < seconds) {
        rounds++;
        /* A random piece of the file, through stdio. */
        seed = seed * 1103515245u + 12345u;
        size_t len = 1 + (seed >> 8) % 40000;
        seed = seed * 1103515245u + 12345u;
        size_t off = ((size_t)seed << 4) % (size - len);
        if (fseek(f, off, SEEK_SET) != 0) {
            printf("[%d] round %lu: fseek(%zu) failed: errno %d\n", pid, rounds, off, errno);
            return 1;
        }
        errno = 0;
        size_t got = fread(buf, 1, len, f);
        if (got != len) {
            printf("[%d] round %lu: fread(%zu at %zu) gave %zu: errno %d, eof %d, error %d, fd %d\n", pid, rounds, len,
                   off, got, errno, feof(f), ferror(f), fileno(f));
            return 1;
        }
        if (memcmp(buf, copy + off, len) != 0) {
            size_t i = 0;
            while (buf[i] == copy[off + i])
                i++;
            printf("[%d] round %lu: file bytes differ at %zu+%zu: %02x, not %02x\n", pid, rounds, off, i, buf[i],
                   copy[off + i]);
            return 1;
        }
        /* A big block (mmap'd by malloc), filled, checked, freed (munmap). */
        seed = seed * 1103515245u + 12345u;
        size_t blen = 200000 + (seed >> 8) % 2000000;
        uint32_t *blk = malloc(blen);
        for (size_t i = 0; i < blen / 4; i += 997)
            blk[i] = (uint32_t)(i ^ pid);
        for (size_t i = 0; i < blen / 4; i += 997) {
            if (blk[i] != (uint32_t)(i ^ pid)) {
                printf("[%d] round %lu: fresh block word %zu changed: %08x\n", pid, rounds, i, blk[i]);
                return 1;
            }
        }
        free(blk);
        /* The memory pattern (a slice of it each round, all of it every 64). */
        size_t from = rounds % 64 * (words / 64), to = from + words / 64;
        for (size_t i = from; i < to; i++) {
            if (pat[i] != ((uint64_t)pid << 40 ^ i * 0x9E3779B97F4A7C15ull)) {
                printf("[%d] round %lu: memory word %zu changed: %016llx\n", pid, rounds, i, (unsigned long long)pat[i]);
                return 1;
            }
        }
        /* Floating point (SSE registers live across the work). */
        double a = 0, b = 1;
        for (int i = 1; i <= 2000; i++) {
            a += 1.0 / i;
            b *= 1.0001;
        }
        if (a < 8.17836 || a > 8.17837 || b < 1.22139 || b > 1.22141) {
            printf("[%d] round %lu: floating point wrong: %.9f %.9f\n", pid, rounds, a, b);
            return 1;
        }
        if (getpid() != pid) {
            printf("[%d] round %lu: getpid() says %d\n", pid, rounds, getpid());
            return 1;
        }
        struct stat st2;
        if (fstat(fileno(f), &st2) != 0 || st2.st_ino != st.st_ino || st2.st_size != st.st_size) {
            printf("[%d] round %lu: fd %d isn't the file any more (errno %d)\n", pid, rounds, fileno(f), errno);
            return 1;
        }
    }
    printf("[%d] ok: %lu rounds, %lu timer signals\n", pid, rounds, ticks);
    return 0;
}
