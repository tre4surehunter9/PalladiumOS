/* Copyright (c) 2026 tre4surehunter9
 * test_tlb: threads of one process on several cores at once, and the
 * kernel taking mappings away under them (TLB shootdowns). Worker threads
 * keep using a page while the main thread
 *  1. replaces it (munmap + mmap MAP_FIXED, a new value): a worker that's
 *     been told must see the new page, not the old one through its TLB;
 *  2. makes it read-only (mprotect): a worker's write must then fault;
 *  3. forks while workers keep writing a counter: the child's copy must
 *     not change (a stale writable TLB entry would write into it).
 * With one core it passes trivially. */
#include <pthread.h>
#include <setjmp.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define WORKERS 3
#define ROUNDS 300

static volatile int *page;          /* the page being changed under the workers */
static _Atomic int round_no;        /* the value it should hold now */
static _Atomic int phase;           /* 1: read, 2: write (expect faults), 3: count */
static _Atomic int stop;
static _Atomic int stale, missed_faults, faults;
static _Atomic int started;
static __thread sigjmp_buf jump;
static volatile long *counter;

static void on_segv(int sig)
{
    (void)sig;
    faults++;
    siglongjmp(jump, 1);
}

static void *worker(void *arg)
{
    (void)arg;
    started++;
    while (!stop) {
        int p = phase;
        if (p == 1) {
            int want = round_no;
            if (sigsetjmp(jump, 1) != 0)
                continue; /* (the page was gone for a moment: fine) */
            int got = page[0];
            /* The main thread changes round_no only after the new page is
             * in: seeing an older round's value after that is a stale TLB
             * entry. (0: a newer page, its value not written yet.) */
            if (got != 0 && got < want) /* (-1: the scratch page, through a stale entry) */
                stale++;
        } else if (p == 2) {
            if (sigsetjmp(jump, 1) == 0) {
                page[1] = 1; /* read-only now: must fault */
                if (phase == 2)
                    missed_faults++;
            }
        } else if (p == 3) {
            (*counter)++;
        }
    }
    return NULL;
}

static void pause_ms(int ms)
{
    struct timespec ts = { 0, ms * 1000000L };
    nanosleep(&ts, NULL);
}

int main(void)
{
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_segv;
    sigaction(SIGSEGV, &sa, NULL);

    page = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    counter = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED || counter == MAP_FAILED) {
        puts("test_tlb: mmap failed");
        return 1;
    }
    page[0] = 0;
    pthread_t t[WORKERS];
    for (int i = 0; i < WORKERS; i++)
        pthread_create(&t[i], NULL, worker, NULL);
    while (started < WORKERS)
        sched_yield();

    /* 1. A new page at the same address, again and again. The old page's
     * frame goes to a scratch page in between, so the new page has another
     * one (else a stale TLB entry would point at the right frame anyway),
     * and the old frame holds another value. */
    /* (Reserved up front, elsewhere: a scratch mmap made after the munmap
     * would land at the freed address itself.) */
    int *scratch = mmap(NULL, (ROUNDS + 1) * 4096L, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (scratch == MAP_FAILED) {
        puts("test_tlb: mmap failed");
        return 1;
    }
    phase = 1;
    for (int r = 1; r <= ROUNDS; r++) {
        munmap((void *)page, 4096);
        scratch[r * 1024] = -1; /* (takes the freed frame; a stale read of it: -1 < want) */
        int *fresh = mmap((void *)page, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
        if (fresh != (int *)page) {
            puts("test_tlb: MAP_FIXED failed");
            return 1;
        }
        fresh[0] = r;
        round_no = r;
        if (r % 50 == 0)
            pause_ms(2);
    }
    pause_ms(20);
    phase = 0;
    pause_ms(5);
    munmap(scratch, (ROUNDS + 1) * 4096L);

    /* 2. Read-only: every write after this must fault. */
    page[1] = 0;
    mprotect((void *)page, 4096, PROT_READ);
    phase = 2;
    pause_ms(50);
    phase = 0;
    pause_ms(5);

    /* 3. fork while the counter is being written. */
    phase = 3;
    pause_ms(10);
    int pipefd[2];
    pipe(pipefd);
    pid_t pid = fork();
    if (pid == 0) {
        long first = *counter;
        pause_ms(50);
        long later = *counter;
        char ok = first == later ? 'y' : 'n';
        write(pipefd[1], &ok, 1);
        _exit(0);
    }
    char child_ok = '?';
    read(pipefd[0], &child_ok, 1);
    waitpid(pid, NULL, 0);
    phase = 0;

    stop = 1;
    for (int i = 0; i < WORKERS; i++)
        pthread_join(t[i], NULL);
    long cores = sysconf(_SC_NPROCESSORS_ONLN);
    printf("test_tlb: %ld cores, %d stale reads, %d writes that didn't fault, %d faults, child's copy %s\n", cores,
           (int)stale, (int)missed_faults, (int)faults, child_ok == 'y' ? "unchanged" : "CHANGED");
    if (stale || missed_faults || child_ok != 'y' || (cores > 1 && faults == 0)) {
        puts("test_tlb: FAILED");
        return 1;
    }
    puts("test_tlb: all checks passed");
    return 0;
}
