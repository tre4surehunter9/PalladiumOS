/* Copyright (c) 2026 tre4surehunter9
 * test_events [DIR]: checks eventfd, epoll, timerfd, sysinfo, prctl, and (with a
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
#include <sched.h>
#include <signal.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/time.h>
#include <sys/utsname.h>
#include <sys/personality.h>
#include <sys/uio.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/timerfd.h>
#include <sys/sysinfo.h>
#include <sys/times.h>
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

    /* times(): ticks since boot, and this process's CPU time. */
    struct tms t;
    volatile unsigned long spin = 0;
    for (unsigned long i = 0; i < 50000000; i++) spin += i;
    clock_t now = times(&t);
    CHECK(now > 0 && t.tms_utime > 0 && t.tms_utime <= now && t.tms_cutime == 0);

    /* fchmodat2 with AT_SYMLINK_NOFOLLOW (how musl chmods for apk): sets
     * setuid bits on a file, refuses a symlink. */
    const char *f = "/tmp/test_events.mode", *l = "/tmp/test_events.link";
    unlink(f);
    unlink(l);
    int fd = open(f, O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0);
    close(fd);
    CHECK(syscall(452, AT_FDCWD, f, 04750, AT_SYMLINK_NOFOLLOW) == 0);
    struct stat st;
    CHECK(stat(f, &st) == 0 && (st.st_mode & 07777) == 04750);
    CHECK(fchmodat(AT_FDCWD, f, 0600, AT_SYMLINK_NOFOLLOW) == 0); /* musl's wrapper */
    CHECK(stat(f, &st) == 0 && (st.st_mode & 07777) == 0600);
    CHECK(symlink(f, l) == 0);
    CHECK(syscall(452, AT_FDCWD, l, 0644, AT_SYMLINK_NOFOLLOW) == -1 && errno == EOPNOTSUPP);
    unlink(l);
    unlink(f);

    /* Clocks: time(), gettimeofday() and clock_gettime() agree. */
    struct timeval tv;
    struct timespec rt;
    time_t t1 = time(NULL);
    CHECK(syscall(SYS_gettimeofday, &tv, NULL) == 0);
    CHECK(clock_gettime(CLOCK_REALTIME, &rt) == 0);
    CHECK(t1 > 1577836800 && tv.tv_sec - t1 <= 1 && rt.tv_sec - tv.tv_sec <= 1 && tv.tv_usec < 1000000);
    time_t t2;
    CHECK(syscall(SYS_time, &t2) == t2 && t2 - t1 <= 1);

    /* Priorities, memory locking, the one CPU. */
    errno = 0;
    CHECK(getpriority(PRIO_PROCESS, 0) == 0 && errno == 0);
    CHECK(setpriority(PRIO_PROCESS, 0, 0) == 0);
    static char locked[8192];
    CHECK(mlock(locked, sizeof locked) == 0 && munlock(locked, sizeof locked) == 0);
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(0, &set);
    CHECK(sched_setaffinity(0, sizeof set, &set) == 0);
    unsigned cpu = 99, node = 99;
    CHECK(syscall(SYS_getcpu, &cpu, &node, NULL) == 0 && cpu == 0 && node == 0);

    /* close_range: close, or mark close-on-exec. */
    int a = dup(0), b = dup(0), c = dup(0);
    CHECK(a >= 0 && b == a + 1 && c == a + 2);
    CHECK(syscall(SYS_close_range, b, c, 4 /* CLOSE_RANGE_CLOEXEC */) == 0);
    CHECK(fcntl(b, F_GETFD) & FD_CLOEXEC);
    CHECK(syscall(SYS_close_range, a, c, 0) == 0);
    CHECK(fcntl(a, F_GETFD) == -1 && errno == EBADF && fcntl(c, F_GETFD) == -1);

    /* fallocate grows a file; copy_file_range copies between files. */
    const char *src = "/tmp/test_events.src", *dst = "/tmp/test_events.dst";
    int s1 = open(src, O_CREAT | O_TRUNC | O_RDWR, 0644);
    CHECK(s1 >= 0 && fallocate(s1, 0, 0, 10000) == 0);
    CHECK(fstat(s1, &st) == 0 && st.st_size == 10000);
    CHECK(ftruncate(s1, 0) == 0 && write(s1, "hello, world", 12) == 12);
    int d1 = open(dst, O_CREAT | O_TRUNC | O_RDWR, 0644);
    CHECK(d1 >= 0);
    loff_t in_off = 7, out_off = 0;
    CHECK(copy_file_range(s1, &in_off, d1, &out_off, 100, 0) == 5 && in_off == 12 && out_off == 5);
    char got[16] = {0};
    CHECK(pread(d1, got, sizeof got, 0) == 5 && memcmp(got, "world", 5) == 0);
    close(s1);
    close(d1);
    unlink(src);
    unlink(dst);

    /* sethostname changes uname's nodename. */
    struct utsname u;
    char old[65];
    CHECK(gethostname(old, sizeof old) == 0);
    CHECK(sethostname("testhost", 8) == 0);
    CHECK(uname(&u) == 0 && strcmp(u.nodename, "testhost") == 0);
    CHECK(sethostname(old, strlen(old)) == 0);

    /* utimes and utime set the modification time. */
    const char *tf = "/tmp/test_events.times";
    close(open(tf, O_CREAT | O_WRONLY, 0644));
    struct timeval tvs[2] = { { 1000000000, 0 }, { 1234567890, 0 } };
    CHECK(syscall(SYS_utimes, tf, tvs) == 0 && stat(tf, &st) == 0 && st.st_mtime == 1234567890);
    long ub[2] = { 0, 1111111111 };
    CHECK(syscall(SYS_utime, tf, ub) == 0 && stat(tf, &st) == 0 && st.st_mtime == 1111111111);
    unlink(tf);

    /* memfd: an anonymous file, gone when closed. */
    int m = syscall(SYS_memfd_create, "buf", 1 /* MFD_CLOEXEC */);
    CHECK(m >= 0 && (fcntl(m, F_GETFD) & FD_CLOEXEC));
    CHECK(write(m, "memory!", 7) == 7 && fstat(m, &st) == 0 && st.st_size == 7);
    char mb[8] = {0};
    CHECK(pread(m, mb, 7, 0) == 7 && memcmp(mb, "memory!", 7) == 0);
    CHECK(syscall(SYS_memfd_create, "a/b", 0) == -1 && errno == EINVAL);
    close(m);

    /* Scheduling policies (musl's wrappers for some of these don't call
     * the kernel, so ask it directly). */
    int prio = 7;
    CHECK(syscall(SYS_sched_getscheduler, 0) == SCHED_OTHER);
    CHECK(syscall(SYS_sched_getparam, 0, &prio) == 0 && prio == 0);
    prio = 0;
    CHECK(syscall(SYS_sched_setscheduler, 0, SCHED_OTHER, &prio) == 0);
    prio = 50;
    CHECK(syscall(SYS_sched_setscheduler, 0, SCHED_OTHER, &prio) == -1 && errno == EINVAL);
    CHECK(sched_get_priority_max(SCHED_FIFO) == 99 && sched_get_priority_min(SCHED_RR) == 1);
    CHECK(sched_get_priority_max(SCHED_OTHER) == 0);
    struct timespec rr;
    CHECK(sched_rr_get_interval(0, &rr) == 0 && rr.tv_sec == 0 && rr.tv_nsec == 100000000);

    /* personality, I/O priority, syncs, /proc/cmdline. */
    CHECK(personality(0xffffffff) == 0);
    CHECK(syscall(SYS_ioprio_get, 1, 0) == 0);
    int sf = open("/tmp/test_events.sync", O_CREAT | O_RDWR, 0644);
    CHECK(sf >= 0 && syncfs(sf) == 0 && sync_file_range(sf, 0, 0, 0) == 0 && readahead(sf, 0, 4096) == 0);
    close(sf);
    unlink("/tmp/test_events.sync");
    /* preadv/pwritev (and v2, where -1 means the fd's position). */
    int vf = open("/tmp/test_events.vec", O_CREAT | O_TRUNC | O_RDWR, 0644);
    char va[4] = "abc", vb[4] = "def", ra[4] = {0}, rb[4] = {0};
    struct iovec wv[2] = { { va, 3 }, { vb, 3 } }, rv[2] = { { ra, 3 }, { rb, 3 } };
    CHECK(vf >= 0 && pwritev(vf, wv, 2, 10) == 6 && lseek(vf, 0, SEEK_CUR) == 0);
    CHECK(preadv(vf, rv, 2, 10) == 6 && memcmp(ra, "abc", 3) == 0 && memcmp(rb, "def", 3) == 0);
    CHECK(syscall(SYS_pwritev2, vf, wv, 1, -1L, 0L, 0) == 3 && lseek(vf, 0, SEEK_CUR) == 3); /* (moves) */
    close(vf);
    unlink("/tmp/test_events.vec");
    char cmdline[64] = {0};
    int cf = open("/proc/cmdline", O_RDONLY);
    CHECK(cf >= 0 && read(cf, cmdline, sizeof cmdline - 1) > 0 && strstr(cmdline, "palladiumos"));
    close(cf);
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

static volatile int waited_sig;

static void *signal_thread(void *arg)
{
    int sig;
    if (sigwait(arg, &sig) == 0)
        waited_sig = sig;
    return NULL;
}

static int sigwaits(void)
{
    sigset_t s;
    sigemptyset(&s);
    sigaddset(&s, SIGUSR1);
    CHECK(pthread_sigmask(SIG_BLOCK, &s, NULL) == 0);
    struct timespec ts = { 0, 100 * 1000000 };
    CHECK(sigtimedwait(&s, NULL, &ts) == -1 && errno == EAGAIN);
    siginfo_t si;
    CHECK(kill(getpid(), SIGUSR1) == 0);
    CHECK(sigtimedwait(&s, &si, &ts) == SIGUSR1 && si.si_signo == SIGUSR1);
    /* A signal thread: blocked everywhere, sent to the process, taken by
     * the thread in sigwait (how MariaDB handles signals). */
    pthread_t th;
    CHECK(pthread_create(&th, NULL, signal_thread, &s) == 0);
    usleep(150 * 1000);
    CHECK(kill(getpid(), SIGUSR1) == 0);
    CHECK(pthread_join(th, NULL) == 0 && waited_sig == SIGUSR1);
    CHECK(pthread_sigmask(SIG_UNBLOCK, &s, NULL) == 0);
    return 0;
}

static int timerfds(void)
{
    uint64_t n;
    struct itimerspec its = {0}, cur;
    struct timespec t0;

    /* One-shot, 200 ms: not yet readable; then poll() with no timeout
     * must wake up when it expires. */
    int t = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK | TFD_CLOEXEC);
    CHECK(t >= 0);
    its.it_value.tv_nsec = 200 * 1000000;
    CHECK(timerfd_settime(t, 0, &its, NULL) == 0);
    CHECK(read(t, &n, 8) == -1 && errno == EAGAIN);
    CHECK(timerfd_gettime(t, &cur) == 0 && cur.it_value.tv_sec == 0 && cur.it_value.tv_nsec > 0);
    struct pollfd pfd = { .fd = t, .events = POLLIN };
    clock_gettime(CLOCK_MONOTONIC, &t0);
    CHECK(poll(&pfd, 1, -1) == 1 && (pfd.revents & POLLIN));
    long ms = ms_since(&t0);
    CHECK(ms >= 100 && ms < 2000);
    CHECK(read(t, &n, 8) == 8 && n == 1);
    CHECK(read(t, &n, 8) == -1 && errno == EAGAIN);
    CHECK(timerfd_gettime(t, &cur) == 0 && cur.it_value.tv_sec == 0 && cur.it_value.tv_nsec == 0);

    /* Repeating every 100 ms: expirations add up. */
    its.it_value.tv_nsec = 100 * 1000000;
    its.it_interval.tv_nsec = 100 * 1000000;
    CHECK(timerfd_settime(t, 0, &its, NULL) == 0);
    usleep(550 * 1000);
    CHECK(read(t, &n, 8) == 8 && n >= 3 && n <= 8);

    /* epoll_wait with no timeout on a timer (absolute: now + 150 ms). */
    int ep = epoll_create1(0);
    struct epoll_event ev = { .events = EPOLLIN, .data.u32 = 5 }, out;
    int t2 = timerfd_create(CLOCK_MONOTONIC, 0);
    CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, t2, &ev) == 0);
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    struct itimerspec abs = {0};
    abs.it_value.tv_sec = now.tv_sec + (now.tv_nsec + 150000000) / 1000000000;
    abs.it_value.tv_nsec = (now.tv_nsec + 150000000) % 1000000000;
    CHECK(timerfd_settime(t2, TFD_TIMER_ABSTIME, &abs, NULL) == 0);
    clock_gettime(CLOCK_MONOTONIC, &t0);
    CHECK(epoll_wait(ep, &out, 1, -1) == 1 && out.data.u32 == 5);
    CHECK(ms_since(&t0) < 2000);
    CHECK(read(t2, &n, 8) == 8 && n == 1); /* (blocking fd: data is there) */

    /* Disarmed: nothing more. */
    struct itimerspec off = {0};
    CHECK(timerfd_settime(t, 0, &off, NULL) == 0);
    CHECK(read(t, &n, 8) == -1 && errno == EAGAIN);
    close(t);
    close(t2);
    close(ep);
    return 0;
}

int main(int argc, char **argv)
{
    int r;
    if ((r = eventfds()) || (r = epolls()) || (r = misc()) || (r = timerfds()) || (r = sigwaits()))
        return r;
    /* The big program needs a directory on a disk (argv[1]). */
    if (argc > 1 && (r = big_program(argv[1])))
        return r;
    printf("test_events: all checks passed\n");
    return 0;
}
