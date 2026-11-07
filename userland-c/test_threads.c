/* Copyright (c) 2026 tre4surehunter9
 * test_threads: checks POSIX threads (musl's pthreads on PalladiumOS's
 * clone/futex). Exits 0, or the line number of the first failed check. */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <semaphore.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define CHECK(c) do { if (!(c)) { fprintf(stderr, "test_threads: check failed at line %d: %s\n", __LINE__, #c); return __LINE__; } } while (0)

static pid_t gettid_(void) { return (pid_t)syscall(SYS_gettid); }

/* ---- a counter under contention ---- */
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static long counter;
#define ROUNDS 20000

static void *add(void *arg)
{
    for (int i = 0; i < ROUNDS; i++) {
        pthread_mutex_lock(&lock);
        long v = counter;
        if (i % 1000 == 0)
            sched_yield(); /* let another thread try the lock */
        counter = v + 1;
        pthread_mutex_unlock(&lock);
    }
    return arg;
}

/* ---- thread-local storage, ids ---- */
static __thread int tls = 1;
static pid_t main_pid;

static void *ids(void *arg)
{
    tls = (int)(long)arg; /* our own copy */
    if (getpid() != main_pid) return (void *)1; /* the same process */
    if (gettid_() == main_pid) return (void *)2; /* but another thread */
    usleep(50 * 1000);
    if (tls != (int)(long)arg) return (void *)3;
    return (void *)(long)gettid_();
}

/* ---- producer / consumer with a condition variable ---- */
static pthread_mutex_t qlock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t qcond = PTHREAD_COND_INITIALIZER;
static int queue[16], qlen, produced_sum;

static void *producer(void *arg)
{
    (void)arg;
    for (int i = 1; i <= 100; i++) {
        pthread_mutex_lock(&qlock);
        while (qlen == 16)
            pthread_cond_wait(&qcond, &qlock);
        queue[qlen++] = i;
        produced_sum += i;
        pthread_cond_broadcast(&qcond);
        pthread_mutex_unlock(&qlock);
    }
    return NULL;
}

/* ---- semaphores ---- */
static sem_t sem;
static void *poster(void *arg)
{
    (void)arg;
    for (int i = 0; i < 10; i++) {
        usleep(5 * 1000);
        sem_post(&sem);
    }
    return NULL;
}

/* ---- signals to one thread ---- */
static volatile pid_t handled_on;
static void on_usr1(int sig) { (void)sig; handled_on = gettid_(); }
static volatile int stop_waiting;
static pid_t waiter_tid;
static void *waiter(void *arg)
{
    (void)arg;
    waiter_tid = gettid_();
    while (!stop_waiting)
        usleep(10 * 1000);
    return NULL;
}

static void *exits_process(void *arg)
{
    (void)arg;
    usleep(20 * 1000);
    exit(42); /* ends every thread */
}

static void *finishes_later(void *arg)
{
    (void)arg;
    usleep(100 * 1000);
    printf("the other thread finished\n");
    fflush(stdout);
    return NULL;
}

int main(int argc, char **argv)
{
    if (argc > 1 && strcmp(argv[1], "main-exits") == 0) {
        /* pthread_exit in main: the process lives until its last thread */
        pthread_t t;
        pthread_create(&t, NULL, finishes_later, NULL);
        pthread_exit(NULL);
    }
    main_pid = getpid();

    /* contention: 4 threads, one mutex */
    pthread_t t[8];
    for (long i = 0; i < 4; i++)
        CHECK(pthread_create(&t[i], NULL, add, (void *)i) == 0);
    for (long i = 0; i < 4; i++) {
        void *ret;
        CHECK(pthread_join(t[i], &ret) == 0 && ret == (void *)i);
    }
    CHECK(counter == 4L * ROUNDS);

    /* ids and TLS */
    pid_t tids[8];
    for (long i = 0; i < 8; i++)
        CHECK(pthread_create(&t[i], NULL, ids, (void *)(i + 10)) == 0);
    for (int i = 0; i < 8; i++) {
        void *ret;
        CHECK(pthread_join(t[i], &ret) == 0);
        CHECK((long)ret > 3);
        tids[i] = (pid_t)(long)ret;
        for (int j = 0; j < i; j++)
            CHECK(tids[j] != tids[i]);
    }
    CHECK(tls == 1); /* main's copy untouched */

    /* producer / consumer */
    pthread_t prod;
    CHECK(pthread_create(&prod, NULL, producer, NULL) == 0);
    int consumed = 0, sum = 0;
    while (consumed < 100) {
        pthread_mutex_lock(&qlock);
        while (qlen == 0)
            pthread_cond_wait(&qcond, &qlock);
        sum += queue[--qlen];
        consumed++;
        pthread_cond_broadcast(&qcond);
        pthread_mutex_unlock(&qlock);
    }
    pthread_join(prod, NULL);
    CHECK(sum == 5050 && produced_sum == 5050);

    /* a timed wait that times out */
    struct timespec until;
    clock_gettime(CLOCK_REALTIME, &until);
    until.tv_nsec += 200 * 1000 * 1000;
    if (until.tv_nsec >= 1000000000) { until.tv_sec++; until.tv_nsec -= 1000000000; }
    pthread_mutex_lock(&qlock);
    CHECK(pthread_cond_timedwait(&qcond, &qlock, &until) == ETIMEDOUT);
    pthread_mutex_unlock(&qlock);

    /* semaphores */
    CHECK(sem_init(&sem, 0, 0) == 0);
    pthread_t p;
    CHECK(pthread_create(&p, NULL, poster, NULL) == 0);
    for (int i = 0; i < 10; i++)
        CHECK(sem_wait(&sem) == 0);
    pthread_join(p, NULL);
    CHECK(sem_trywait(&sem) == -1 && errno == EAGAIN);

    /* a signal for one thread runs its handler in that thread */
    signal(SIGUSR1, on_usr1);
    pthread_t w;
    CHECK(pthread_create(&w, NULL, waiter, NULL) == 0);
    while (!waiter_tid)
        usleep(1000);
    CHECK(pthread_kill(w, SIGUSR1) == 0);
    for (int i = 0; i < 100 && !handled_on; i++)
        usleep(10 * 1000);
    CHECK(handled_on == waiter_tid);
    stop_waiting = 1;
    pthread_join(w, NULL);

    /* detached threads clean up after themselves */
    for (int i = 0; i < 10; i++) {
        pthread_t d;
        CHECK(pthread_create(&d, NULL, ids, (void *)(long)(i + 10)) == 0);
        CHECK(pthread_detach(d) == 0);
    }
    usleep(200 * 1000);

    /* fork in a threaded program: the child has one thread */
    pthread_create(&w, NULL, waiter, NULL);
    stop_waiting = 0;
    pid_t child = fork();
    if (child == 0)
        _exit(counter == 4L * ROUNDS ? 7 : 1);
    int status;
    CHECK(waitpid(child, &status, 0) == child && WEXITSTATUS(status) == 7);
    stop_waiting = 1;
    pthread_join(w, NULL);

    /* exit() from another thread ends the whole process */
    child = fork();
    if (child == 0) {
        pthread_t e;
        pthread_create(&e, NULL, exits_process, NULL);
        for (;;)
            pause();
    }
    CHECK(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 42);

    printf("test_threads: all checks passed\n");
    return 0;
}
