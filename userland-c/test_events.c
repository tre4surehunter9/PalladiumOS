/* Copyright (c) 2026 tre4surehunter9
 * test_events [DIR]: checks eventfd, epoll, sysinfo, prctl, and (with a
 * directory on a disk) running a program bigger than the kernel's heap.
 * Exits 0, or the line number of the first failed check. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/sysinfo.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define CHECK(c) do { if (!(c)) { fprintf(stderr, "test_events: check failed at line %d: %s (errno %d)\n", __LINE__, #c, errno); return __LINE__; } } while (0)

static long ms_since(struct timespec *t0)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (t.tv_sec - t0->tv_sec) * 1000 + (t.tv_nsec - t0->tv_nsec) / 1000000;
}

static int eventfds(void)
{
    uint64_t v;
    int fd = eventfd(3, EFD_NONBLOCK);
    CHECK(fd >= 0);
    CHECK(read(fd, &v, 8) == 8 && v == 3);
    CHECK(read(fd, &v, 8) == -1 && errno == EAGAIN);
    v = 5;
    CHECK(write(fd, &v, 8) == 8);
    v = 2;
    CHECK(write(fd, &v, 8) == 8);
    CHECK(read(fd, &v, 8) == 8 && v == 7);
    CHECK(read(fd, &v, 4) == -1 && errno == EINVAL);
    close(fd);

    fd = eventfd(2, EFD_SEMAPHORE | EFD_NONBLOCK);
    CHECK(read(fd, &v, 8) == 8 && v == 1);
    CHECK(read(fd, &v, 8) == 8 && v == 1);
    CHECK(read(fd, &v, 8) == -1 && errno == EAGAIN);
    close(fd);
    return 0;
}

static int efd_for_thread;

static void *wake_later(void *arg)
{
    (void)arg;
    usleep(150 * 1000);
    uint64_t one = 1;
    write(efd_for_thread, &one, 8);
    return NULL;
}

static int epolls(void)
{
    struct epoll_event ev, out[8];
    int p[2];
    CHECK(pipe(p) == 0);
    int ep = epoll_create1(EPOLL_CLOEXEC);
    CHECK(ep >= 0);
    CHECK(fcntl(ep, F_GETFD) & FD_CLOEXEC);

    ev.events = EPOLLIN;
    ev.data.u64 = 0x1234567890ULL;
    CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, p[0], &ev) == 0);
    CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, p[0], &ev) == -1 && errno == EEXIST);
    CHECK(epoll_wait(ep, out, 8, 0) == 0); /* nothing yet */

    /* A timeout is a real wait. */
    struct timespec t0;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    CHECK(epoll_wait(ep, out, 8, 200) == 0);
    long waited = ms_since(&t0);
    CHECK(waited >= 150 && waited < 2000);

    CHECK(write(p[1], "x", 1) == 1);
    CHECK(epoll_wait(ep, out, 8, -1) == 1);
    CHECK(out[0].events == EPOLLIN && out[0].data.u64 == 0x1234567890ULL);
    /* Level-triggered: still ready until read. */
    CHECK(epoll_wait(ep, out, 8, 0) == 1);
    char c;
    CHECK(read(p[0], &c, 1) == 1);
    CHECK(epoll_wait(ep, out, 8, 0) == 0);

    /* The write end: writable at once. */
    ev.events = EPOLLOUT;
    ev.data.fd = p[1];
    CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, p[1], &ev) == 0);
    CHECK(epoll_wait(ep, out, 8, 0) == 1 && out[0].data.fd == p[1] && (out[0].events & EPOLLOUT));
    CHECK(epoll_ctl(ep, EPOLL_CTL_DEL, p[1], NULL) == 0);
    CHECK(epoll_ctl(ep, EPOLL_CTL_DEL, p[1], NULL) == -1 && errno == ENOENT);

    /* One-shot: reported once, until re-armed with MOD. */
    ev.events = EPOLLIN | EPOLLONESHOT;
    ev.data.u32 = 7;
    CHECK(epoll_ctl(ep, EPOLL_CTL_MOD, p[0], &ev) == 0);
    CHECK(write(p[1], "y", 1) == 1);
    CHECK(epoll_wait(ep, out, 8, 0) == 1 && out[0].data.u32 == 7);
    CHECK(epoll_wait(ep, out, 8, 0) == 0);
    CHECK(epoll_ctl(ep, EPOLL_CTL_MOD, p[0], &ev) == 0);
    CHECK(epoll_wait(ep, out, 8, 0) == 1);

    /* The epoll fd itself is readable (poll) while something is ready. */
    struct pollfd pfd = { .fd = ep, .events = POLLIN };
    CHECK(epoll_ctl(ep, EPOLL_CTL_MOD, p[0], &ev) == 0);
    CHECK(poll(&pfd, 1, 0) == 1 && (pfd.revents & POLLIN));

    /* Hang-up: the writer closes. */
    CHECK(read(p[0], &c, 1) == 1);
    ev.events = EPOLLIN;
    CHECK(epoll_ctl(ep, EPOLL_CTL_MOD, p[0], &ev) == 0);
    close(p[1]);
    CHECK(epoll_wait(ep, out, 8, 0) == 1 && (out[0].events & EPOLLHUP));
    /* A closed fd leaves the list. */
    close(p[0]);
    CHECK(epoll_wait(ep, out, 8, 0) == 0);

    /* An eventfd, written by another thread while epoll_wait sleeps. */
    efd_for_thread = eventfd(0, EFD_NONBLOCK);
    ev.events = EPOLLIN;
    ev.data.u32 = 99;
    CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, efd_for_thread, &ev) == 0);
    pthread_t th;
    CHECK(pthread_create(&th, NULL, wake_later, NULL) == 0);
    clock_gettime(CLOCK_MONOTONIC, &t0);
    CHECK(epoll_wait(ep, out, 8, 5000) == 1 && out[0].data.u32 == 99);
    CHECK(ms_since(&t0) < 4000);
    pthread_join(th, NULL);

    /* Regular files can't be watched (they are always ready). */
    int f = open("/tmp/test_events.file", O_CREAT | O_RDWR, 0644);
    CHECK(f >= 0);
    CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, f, &ev) == -1 && errno == EPERM);
    close(f);
    unlink("/tmp/test_events.file");
    close(efd_for_thread);
    close(ep);
    return 0;
}

static int misc(void)
{
    struct sysinfo si;
    CHECK(sysinfo(&si) == 0);
    CHECK(si.totalram > 16 * 1024 * 1024 && si.freeram > 0 && si.freeram <= si.totalram);
    CHECK(si.procs >= 1 && si.mem_unit == 1);

    char name[16];
    CHECK(prctl(PR_SET_NAME, "worker-1") == 0);
    CHECK(prctl(PR_GET_NAME, name) == 0 && strcmp(name, "worker-1") == 0);
    CHECK(prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0);
    return 0;
}

/* A program bigger than the kernel's heap (32 MiB): one segment of SIZE
 * bytes from the file. Its code exits with the segment's last byte, so it
 * only exits 42 if all of it was loaded. */
#define BIG_SIZE (34u * 1024 * 1024)
#define BIG_BASE 0x400000u

static int big_program(const char *dir)
{
    static unsigned char head[4096];
    memset(head, 0, sizeof head);
    unsigned char *e = head;
    memcpy(e, "\177ELF\2\1\1", 7);
    *(uint16_t *)(e + 16) = 2;   /* ET_EXEC */
    *(uint16_t *)(e + 18) = 62;  /* x86_64 */
    *(uint32_t *)(e + 20) = 1;
    *(uint64_t *)(e + 24) = BIG_BASE + 120; /* entry */
    *(uint64_t *)(e + 32) = 64;  /* program headers */
    *(uint16_t *)(e + 52) = 64;
    *(uint16_t *)(e + 54) = 56;
    *(uint16_t *)(e + 56) = 1;
    unsigned char *ph = e + 64;
    *(uint32_t *)(ph + 0) = 1;   /* PT_LOAD */
    *(uint32_t *)(ph + 4) = 5;   /* R X */
    *(uint64_t *)(ph + 16) = BIG_BASE;
    *(uint64_t *)(ph + 24) = BIG_BASE;
    *(uint64_t *)(ph + 32) = BIG_SIZE;
    *(uint64_t *)(ph + 40) = BIG_SIZE;
    *(uint64_t *)(ph + 48) = 0x1000;
    /* movzx edi, byte [BIG_BASE + BIG_SIZE - 1]; mov eax, 60; syscall */
    unsigned char code[] = { 0x0f, 0xb6, 0x3c, 0x25, 0, 0, 0, 0, 0xb8, 60, 0, 0, 0, 0x0f, 0x05 };
    *(uint32_t *)(code + 4) = BIG_BASE + BIG_SIZE - 1;
    memcpy(e + 120, code, sizeof code);

    char path[256];
    snprintf(path, sizeof path, "%s/test_events.big", dir);
    int fd = open(path, O_CREAT | O_TRUNC | O_WRONLY, 0755);
    CHECK(fd >= 0);
    CHECK(write(fd, head, sizeof head) == sizeof head);
    static unsigned char zeros[65536];
    unsigned left = BIG_SIZE - sizeof head - 1;
    while (left > 0) {
        unsigned n = left < sizeof zeros ? left : sizeof zeros;
        CHECK(write(fd, zeros, n) == (ssize_t)n);
        left -= n;
    }
    unsigned char last = 42;
    CHECK(write(fd, &last, 1) == 1);
    close(fd);

    pid_t pid = fork();
    CHECK(pid >= 0);
    if (pid == 0) {
        execl(path, path, (char *)NULL);
        _exit(100 + errno);
    }
    int status;
    CHECK(waitpid(pid, &status, 0) == pid);
    unlink(path);
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 42)
        fprintf(stderr, "test_events: big program: status %#x\n", status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 42);
    return 0;
}

int main(int argc, char **argv)
{
    int r;
    if ((r = eventfds()) || (r = epolls()) || (r = misc()))
        return r;
    /* The big program needs a directory on a disk (argv[1]). */
    if (argc > 1 && (r = big_program(argv[1])))
        return r;
    printf("test_events: all checks passed\n");
    return 0;
}
