/* Copyright (c) 2026 tre4surehunter9
 * test_libc: checks that musl libc works on PalladiumOS. Exits 0, or the
 * line number of the first failed check (after printing it). */
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <math.h>
#include <poll.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/random.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define CHECK(c) do { if (!(c)) { fprintf(stderr, "test_libc: check failed at line %d: %s\n", __LINE__, #c); return __LINE__; } } while (0)

static __thread int tls_value = 42;
static volatile sig_atomic_t got_signal;
static volatile double handler_result;
static jmp_buf jump;
static int exit_flag_fd = -1;

static void on_usr1(int sig, siginfo_t *info, void *uc)
{
    (void)uc;
    got_signal = sig + (info->si_signo == sig ? 100 : 0);
    /* Floating point in a handler must not disturb the interrupted code. */
    double x = 0;
    for (int i = 1; i <= 10; i++)
        x += sqrt((double)i);
    handler_result = x;
}

static void on_alarm(int sig) { got_signal = sig; }

static int compare(const void *a, const void *b) { return *(const int *)a - *(const int *)b; }

static void at_exit_handler(void)
{
    if (exit_flag_fd >= 0)
        write(exit_flag_fd, "x", 1);
}

int main(void)
{
    char buf[256];

    /* ---- printf and floating point ---- */
    snprintf(buf, sizeof buf, "%d %s %x %5.2f %g %e", -42, "str", 255, 3.14159, 0.5, 12345.678);
    CHECK(strcmp(buf, "-42 str ff  3.14 0.5 1.234568e+04") == 0);
    snprintf(buf, sizeof buf, "%.6f", sqrt(2.0));
    CHECK(strcmp(buf, "1.414214") == 0);
    CHECK(fabs(strtod("2.5e3", NULL) - 2500.0) < 1e-9);
    CHECK(atoi("  123abc") == 123);
    double d = 1.0;
    for (int i = 0; i < 20; i++)
        d *= 1.5;
    CHECK(d > 3325.25 && d < 3325.26);

    /* ---- malloc: small (brk) and large (mmap) ---- */
    char *small = malloc(100);
    CHECK(small != NULL);
    strcpy(small, "hello");
    char *big = malloc(4 * 1024 * 1024);
    CHECK(big != NULL);
    memset(big, 0xab, 4 * 1024 * 1024);
    CHECK((unsigned char)big[4 * 1024 * 1024 - 1] == 0xab);
    big = realloc(big, 8 * 1024 * 1024);
    CHECK(big && (unsigned char)big[1000] == 0xab);
    free(big);
    int *zeros = calloc(1000, sizeof(int));
    CHECK(zeros && zeros[999] == 0);
    free(zeros);
    for (int i = 0; i < 1000; i++) {
        void *p = malloc(i * 37 + 1);
        CHECK(p != NULL);
        free(p);
    }
    CHECK(strcmp(small, "hello") == 0);
    free(small);

    /* ---- strings, sorting ---- */
    int nums[] = {5, 3, 9, 1, 7};
    qsort(nums, 5, sizeof(int), compare);
    CHECK(nums[0] == 1 && nums[4] == 9);
    char long_str[1000];
    memset(long_str, 'a', sizeof long_str - 1);
    long_str[999] = 0;
    CHECK(strlen(long_str) == 999);
    CHECK(strstr("find the needle", "needle") != NULL);

    /* ---- errno is thread-local; TLS works ---- */
    CHECK(tls_value == 42);
    tls_value++;
    CHECK(tls_value == 43);
    CHECK(open("/no/such/file", O_RDONLY) == -1 && errno == ENOENT);
    CHECK(strcmp(strerror(ENOENT), "No such file or directory") == 0);

    /* ---- files: stdio and syscalls ---- */
    mkdir("/tmp", 0777);
    FILE *f = fopen("/tmp/libc.txt", "w");
    CHECK(f != NULL);
    for (int i = 0; i < 100; i++)
        fprintf(f, "line %d\n", i);
    CHECK(fclose(f) == 0);
    f = fopen("/tmp/libc.txt", "r");
    CHECK(f != NULL);
    CHECK(fgets(buf, sizeof buf, f) && strcmp(buf, "line 0\n") == 0);
    CHECK(fseek(f, 0, SEEK_END) == 0 && ftell(f) == 790);
    fclose(f);
    struct stat st;
    CHECK(stat("/tmp/libc.txt", &st) == 0 && st.st_size == 790 && S_ISREG(st.st_mode));
    int fd = open("/tmp/libc.txt", O_RDWR);
    CHECK(fd >= 0);
    CHECK(pread(fd, buf, 6, 7) == 6 && memcmp(buf, "line 1", 6) == 0);
    CHECK(lseek(fd, 0, SEEK_CUR) == 0); /* pread doesn't move the offset */
    CHECK(ftruncate(fd, 10) == 0 && fstat(fd, &st) == 0 && st.st_size == 10);
    struct iovec iov[2] = {{"ab", 2}, {"cd", 2}};
    CHECK(lseek(fd, 0, SEEK_END) == 10 && writev(fd, iov, 2) == 4);
    close(fd);
    CHECK(unlink("/tmp/libc.txt") == 0);
    DIR *dir = opendir("/bin");
    CHECK(dir != NULL);
    int found = 0;
    struct dirent *e;
    while ((e = readdir(dir)))
        found |= strcmp(e->d_name, "test_libc") == 0;
    closedir(dir);
    CHECK(found);
    CHECK(getcwd(buf, sizeof buf) != NULL);

    /* ---- mmap ---- */
    long page = sysconf(_SC_PAGESIZE);
    CHECK(page == 4096);
    char *m = mmap(NULL, 3 * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(m != MAP_FAILED && m[0] == 0 && m[3 * page - 1] == 0);
    m[page] = 'x';
    CHECK(mprotect(m, page, PROT_READ) == 0);
    CHECK(munmap(m + page, page) == 0); /* a hole in the middle */
    CHECK(munmap(m, 3 * page) == 0);
    char *reserved = mmap(NULL, 2 * page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(reserved != MAP_FAILED);
    CHECK(mprotect(reserved, page, PROT_READ | PROT_WRITE) == 0);
    reserved[10] = 1;
    munmap(reserved, 2 * page);
    fd = open("/tmp/map.txt", O_CREAT | O_RDWR | O_TRUNC, 0644);
    CHECK(fd >= 0 && write(fd, "mapped file", 11) == 11);
    char *fm = mmap(NULL, 11, PROT_READ, MAP_PRIVATE, fd, 0);
    CHECK(fm != MAP_FAILED && memcmp(fm, "mapped file", 11) == 0);
    munmap(fm, 11);
    close(fd);
    unlink("/tmp/map.txt");

    /* ---- processes and pipes ---- */
    int p[2];
    CHECK(pipe(p) == 0);
    pid_t child = fork();
    CHECK(child >= 0);
    if (child == 0) {
        close(p[0]);
        dprintf(p[1], "from child %d", tls_value);
        _exit(7);
    }
    close(p[1]);
    struct pollfd pfd = {p[0], POLLIN, 0};
    CHECK(poll(&pfd, 1, 5000) == 1 && (pfd.revents & POLLIN));
    ssize_t n = read(p[0], buf, sizeof buf - 1);
    CHECK(n > 0);
    buf[n] = 0;
    CHECK(strcmp(buf, "from child 43") == 0);
    close(p[0]);
    int status;
    CHECK(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 7);
    child = fork();
    if (child == 0) {
        execl("/bin/echo", "echo", "exec works", (char *)NULL);
        _exit(99);
    }
    CHECK(waitpid(child, &status, 0) == child && WEXITSTATUS(status) == 0);

    /* ---- signals ---- */
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_usr1;
    sa.sa_flags = SA_SIGINFO;
    CHECK(sigaction(SIGUSR1, &sa, NULL) == 0);
    double before = 0;
    for (int i = 1; i <= 100; i++)
        before += 1.0 / i;
    CHECK(raise(SIGUSR1) == 0);
    double after = 0;
    for (int i = 1; i <= 100; i++)
        after += 1.0 / i;
    CHECK(got_signal == SIGUSR1 + 100);
    CHECK(before == after);
    CHECK(handler_result > 22.46 && handler_result < 22.47);
    signal(SIGALRM, on_alarm);
    got_signal = 0;
    alarm(1);
    pause();
    CHECK(got_signal == SIGALRM);
    if (setjmp(jump) == 0)
        longjmp(jump, 5);

    /* ---- time, environment, identity ---- */
    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    struct timespec nap = {0, 200 * 1000 * 1000};
    CHECK(nanosleep(&nap, NULL) == 0);
    clock_gettime(CLOCK_MONOTONIC, &t1);
    long ms = (t1.tv_sec - t0.tv_sec) * 1000 + (t1.tv_nsec - t0.tv_nsec) / 1000000;
    CHECK(ms >= 190 && ms < 1000);
    CHECK(time(NULL) > 1700000000); /* the real date, from the CMOS clock */
    CHECK(setenv("LIBC_TEST", "yes", 1) == 0 && strcmp(getenv("LIBC_TEST"), "yes") == 0);
    struct utsname u;
    CHECK(uname(&u) == 0 && strcmp(u.sysname, "PalladiumOS") == 0);
    CHECK(getpid() > 0 && getppid() >= 0);
    unsigned char rnd[32] = {0};
    CHECK(getrandom(rnd, sizeof rnd, 0) == 32);
    int nonzero = 0;
    for (int i = 0; i < 32; i++)
        nonzero += rnd[i] != 0;
    CHECK(nonzero > 16);

    /* ---- atexit runs, stdout is flushed at exit ---- */
    exit_flag_fd = open("/tmp/atexit.flag", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(exit_flag_fd >= 0);
    atexit(at_exit_handler);
    printf("test_libc: all checks passed\n");
    return 0;
}
