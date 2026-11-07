/* Copyright (c) 2026 tre4surehunter9
 * test_chroot: chroot, mount/umount2, fchdir, flock and fcntl locks (run as
 * root). Exits 0, or the line number of the first failed check. Builds a
 * small root in /tmp/jail. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHECK(c) do { if (!(c)) { fprintf(stderr, "test_chroot: check failed at line %d: %s (errno %d)\n", __LINE__, #c, errno); return __LINE__; } } while (0)

static int put(const char *path, const char *text)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0)
        return -1;
    int ok = write(fd, text, strlen(text)) == (ssize_t)strlen(text);
    close(fd);
    return ok ? 0 : -1;
}

static int slurp(const char *path, char *buf, size_t size)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        return -1;
    ssize_t n = read(fd, buf, size - 1);
    close(fd);
    if (n < 0)
        return -1;
    buf[n] = 0;
    return (int)n;
}

static int copy(const char *from, const char *to, int mode)
{
    int in = open(from, O_RDONLY), out = open(to, O_WRONLY | O_CREAT | O_TRUNC, mode);
    if (in < 0 || out < 0)
        return -1;
    char buf[8192];
    ssize_t n;
    while ((n = read(in, buf, sizeof buf)) > 0)
        if (write(out, buf, n) != n)
            return -1;
    close(in);
    close(out);
    return n == 0 ? 0 : -1;
}

/* Run in the child, inside the jail. */
static int inside(void)
{
    char buf[256];
    CHECK(getcwd(buf, sizeof buf) && strcmp(buf, "/") == 0);
    CHECK(slurp("/etc/marker", buf, sizeof buf) > 0 && strcmp(buf, "inside\n") == 0);
    /* An absolute symlink means the jail's /etc/marker. */
    CHECK(slurp("/link", buf, sizeof buf) > 0 && strcmp(buf, "inside\n") == 0);
    /* ".." stops at the new root. */
    CHECK(chdir("/../..") == 0 && getcwd(buf, sizeof buf) && strcmp(buf, "/") == 0);
    CHECK(slurp("../../etc/marker", buf, sizeof buf) > 0);
    /* The real system isn't visible. */
    struct stat st;
    CHECK(stat("/bin/login", &st) == -1 && errno == ENOENT);
    /* proc mounted inside the jail works. */
    CHECK(slurp("/proc/self/status", buf, sizeof buf) > 0 && strstr(buf, "Name:"));
    /* A static program runs inside: it finds its own path in the jail. */
    pid_t c = fork();
    if (c == 0) {
        execl("/bin/test_chroot", "test_chroot", "probe", (char *)0);
        _exit(100);
    }
    int s;
    CHECK(waitpid(c, &s, 0) == c && WIFEXITED(s) && WEXITSTATUS(s) == 42);
    /* A dynamically linked one too: its dynamic linker is the jail's. */
    if (stat("/bin/test_dyn", &st) == 0) {
        c = fork();
        if (c == 0) {
            int null = open("/dev/null", O_WRONLY);
            dup2(null, 1);
            execl("/bin/test_dyn", "test_dyn", (char *)0);
            _exit(100);
        }
        CHECK(waitpid(c, &s, 0) == c && WIFEXITED(s) && WEXITSTATUS(s) == 0);
    }
    return 0;
}

int main(int argc, char **argv)
{
    char buf[256];
    struct stat st;
    if (argc > 1 && strcmp(argv[1], "probe") == 0)
        return stat("/etc/marker", &st) == 0 ? 42 : 1; /* (run inside the jail) */
    CHECK(geteuid() == 0);

    /* ---- fchdir, flock, fcntl record locks ---- */
    mkdir("/tmp", 01777);
    int dir = open("/tmp", O_RDONLY | O_DIRECTORY);
    CHECK(dir >= 0 && chdir("/") == 0 && fchdir(dir) == 0);
    CHECK(getcwd(buf, sizeof buf) && strcmp(buf, "/tmp") == 0);
    close(dir);
    CHECK(put("/tmp/lockfile", "x") == 0);
    int fd = open("/tmp/lockfile", O_RDWR);
    CHECK(fd >= 0 && flock(fd, LOCK_EX | LOCK_NB) == 0 && flock(fd, LOCK_UN) == 0);
    struct flock fl = {.l_type = F_WRLCK, .l_whence = SEEK_SET};
    CHECK(fcntl(fd, F_SETLK, &fl) == 0);
    fl.l_type = F_WRLCK;
    CHECK(fcntl(fd, F_GETLK, &fl) == 0 && fl.l_type == F_UNLCK);
    close(fd);

    /* ---- a small root in /tmp/jail ---- */
    const char *dirs[] = {"/tmp/jail", "/tmp/jail/etc", "/tmp/jail/bin", "/tmp/jail/proc", "/tmp/jail/dev",
                          "/tmp/jail/lib", "/tmp/jail/usr", "/tmp/jail/usr/lib"};
    for (unsigned i = 0; i < sizeof dirs / sizeof *dirs; i++)
        mkdir(dirs[i], 0755);
    CHECK(put("/tmp/jail/etc/marker", "inside\n") == 0);
    unlink("/tmp/jail/link");
    CHECK(symlink("/etc/marker", "/tmp/jail/link") == 0);
    CHECK(copy("/bin/test_chroot", "/tmp/jail/bin/test_chroot", 0755) == 0);
    /* A dynamic program, its libraries and musl's dynamic linker (if built). */
    if (stat("/bin/test_dyn", &st) == 0 && stat("/usr/lib/libc.so", &st) == 0) {
        CHECK(copy("/bin/test_dyn", "/tmp/jail/bin/test_dyn", 0755) == 0);
        CHECK(copy("/usr/lib/libc.so", "/tmp/jail/lib/ld-musl-x86_64.so.1", 0755) == 0);
        CHECK(copy("/usr/lib/libgreet.so", "/tmp/jail/usr/lib/libgreet.so", 0755) == 0);
    }
    CHECK(mount("proc", "/tmp/jail/proc", "proc", 0, NULL) == 0);
    CHECK(mount("dev", "/tmp/jail/dev", "devtmpfs", 0, NULL) == 0);
    CHECK(mount("proc", "/tmp/jail/proc", "proc", 0, NULL) == -1 && errno == EBUSY);
    CHECK(mount("x", "/tmp/jail/etc", "nosuchfs", 0, NULL) == -1 && errno == ENODEV);

    /* ---- inside it ---- */
    pid_t child = fork();
    if (child == 0) {
        if (chroot("/tmp/jail") != 0 || chdir("/") != 0)
            _exit(99);
        _exit(inside());
    }
    int st2;
    CHECK(waitpid(child, &st2, 0) == child && WIFEXITED(st2));
    if (WEXITSTATUS(st2) != 0) {
        fprintf(stderr, "test_chroot: inside the jail, check at line %d failed\n", WEXITSTATUS(st2));
        return WEXITSTATUS(st2);
    }
    /* Our own root didn't change. */
    CHECK(stat("/bin/test_chroot", &st) == 0);

    /* ---- only root ---- */
    child = fork();
    if (child == 0) {
        if (setuid(1000) != 0)
            _exit(2);
        int ok = chroot("/tmp/jail") == -1 && errno == EPERM;
        ok = ok && mount("proc", "/tmp/jail/dev", "proc", 0, NULL) == -1 && errno == EPERM;
        _exit(ok ? 0 : 1);
    }
    CHECK(waitpid(child, &st2, 0) == child && WIFEXITED(st2) && WEXITSTATUS(st2) == 0);

    CHECK(umount2("/tmp/jail/proc", 0) == 0 && umount2("/tmp/jail/dev", 0) == 0);
    CHECK(stat("/tmp/jail/proc/self", &st) == -1);
    printf("test_chroot: all checks passed\n");
    return 0;
}
