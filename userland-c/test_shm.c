/* Copyright (c) 2026 tre4surehunter9
 * test_shm: checks shared memory (MAP_SHARED): anonymous memory shared
 * with a forked child, /dev/shm files mapped by two processes, a named
 * semaphore between processes. Exits 0, or the line number of the first
 * failed check. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <semaphore.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHECK(c) do { if (!(c)) { fprintf(stderr, "test_shm: check failed at line %d: %s (errno %d)\n", __LINE__, #c, errno); return __LINE__; } } while (0)

/* Run `f` in a child; its exit status. */
static int in_child(int (*f)(void *), void *arg)
{
    pid_t pid = fork();
    if (pid == 0)
        _exit(f(arg));
    int status;
    if (pid < 0 || waitpid(pid, &status, 0) != pid || !WIFEXITED(status))
        return -1;
    return WEXITSTATUS(status);
}

static int write_both_pages(void *arg)
{
    char *p = arg;
    strcpy(p, "from the child");
    strcpy(p + 8192, "untouched before fork"); /* a page nobody touched yet */
    return 0;
}

static int write_first_page(void *arg)
{
    strcpy(arg, "the child's copy");
    return strcmp(arg, "the child's copy") == 0 ? 0 : 1;
}

static int anonymous(void)
{
    char *p = mmap(NULL, 3 * 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    CHECK(p != MAP_FAILED);
    strcpy(p, "from the parent");
    CHECK(in_child(write_both_pages, p) == 0);
    CHECK(strcmp(p, "from the child") == 0);
    CHECK(strcmp(p + 8192, "untouched before fork") == 0);

    /* Private memory stays private (copy-on-write). */
    char *q = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(q != MAP_FAILED);
    strcpy(q, "mine");
    CHECK(in_child(write_first_page, q) == 0);
    CHECK(strcmp(q, "mine") == 0);

    /* mprotect keeps it shared: read-only and back, then the child writes. */
    CHECK(mprotect(p, 4096, PROT_READ) == 0 && mprotect(p, 4096, PROT_READ | PROT_WRITE) == 0);
    strcpy(p, "again");
    CHECK(in_child(write_both_pages, p) == 0 && strcmp(p, "from the child") == 0);
    CHECK(munmap(p, 3 * 4096) == 0 && munmap(q, 4096) == 0);
    return 0;
}

static int map_and_write(void *arg)
{
    int fd = open(arg, O_RDWR);
    if (fd < 0)
        return 1;
    char *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (p == MAP_FAILED)
        return 2;
    p[100] = 'Z';
    strcpy(p + 200, "child was here");
    return munmap(p, 4096) == 0 ? 0 : 3;
}

static int files(void)
{
    struct stat st;
    CHECK(stat("/dev/shm", &st) == 0 && S_ISDIR(st.st_mode));
    const char *path = "/dev/shm/test_shm.region";
    int fd = open(path, O_CREAT | O_TRUNC | O_RDWR, 0600);
    CHECK(fd >= 0 && ftruncate(fd, 4096) == 0 && pwrite(fd, "file data", 9, 0) == 9);
    char *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK(p != MAP_FAILED && memcmp(p, "file data", 9) == 0);
    /* Another process maps the same file on its own and writes. */
    CHECK(in_child(map_and_write, (void *)path) == 0);
    CHECK(p[100] == 'Z' && strcmp(p + 200, "child was here") == 0);
    /* Changes reach the file (msync, and when the last mapping goes). */
    strcpy(p + 300, "synced");
    CHECK(msync(p, 4096, MS_SYNC) == 0);
    char buf[8] = {0};
    CHECK(pread(fd, buf, 6, 300) == 6 && memcmp(buf, "synced", 6) == 0);
    p[400] = 'Q';
    CHECK(munmap(p, 4096) == 0);
    CHECK(pread(fd, buf, 1, 400) == 1 && buf[0] == 'Q');
    CHECK(fstat(fd, &st) == 0 && st.st_size == 4096); /* (not grown) */
    /* read()/write() and the mapping stay in step, both ways. */
    p = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK(p != MAP_FAILED);
    CHECK(pwrite(fd, "via pwrite", 10, 500) == 10 && memcmp(p + 500, "via pwrite", 10) == 0);
    memcpy(p + 600, "via memory", 10); /* not written back yet */
    CHECK(pread(fd, buf, 3, 600) == 3 && memcmp(buf, "via", 3) == 0);
    CHECK(lseek(fd, 604, SEEK_SET) == 604 && read(fd, buf, 6) == 6 && memcmp(buf, "memory", 6) == 0);
    CHECK(ftruncate(fd, 550) == 0 && p[600] == 0 && memcmp(p + 500, "via pwrite", 10) == 0);
    CHECK(ftruncate(fd, 4096) == 0 && munmap(p, 4096) == 0);
    /* A read-only fd can't be mapped shared and writable. */
    int ro = open(path, O_RDONLY);
    CHECK(mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, ro, 0) == MAP_FAILED && errno == EACCES);
    close(ro);
    close(fd);
    CHECK(unlink(path) == 0);
    return 0;
}

static int post_it(void *arg)
{
    sem_t *s = sem_open(arg, 0);
    if (s == SEM_FAILED)
        return 1;
    return sem_post(s) == 0 ? 0 : 2;
}

static int semaphores(void)
{
    const char *name = "/test_shm.sem";
    sem_unlink(name);
    sem_t *s = sem_open(name, O_CREAT | O_EXCL, 0600, 0);
    CHECK(s != SEM_FAILED);
    CHECK(sem_trywait(s) == -1 && errno == EAGAIN);
    CHECK(in_child(post_it, (void *)name) == 0);
    CHECK(sem_trywait(s) == 0); /* the child's post */
    CHECK(sem_close(s) == 0 && sem_unlink(name) == 0);

    /* An unnamed semaphore in shared memory, between processes. */
    sem_t *u = mmap(NULL, sizeof(sem_t), PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    CHECK(u != MAP_FAILED && sem_init(u, 1, 0) == 0);
    pid_t pid = fork();
    if (pid == 0) {
        usleep(100 * 1000);
        _exit(sem_post(u) == 0 ? 0 : 1);
    }
    CHECK(sem_wait(u) == 0); /* woken by the child */
    int status;
    CHECK(waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    CHECK(munmap(u, sizeof(sem_t)) == 0);
    return 0;
}

int main(void)
{
    int r;
    if ((r = anonymous()) || (r = files()) || (r = semaphores()))
        return r;
    printf("test_shm: all checks passed\n");
    return 0;
}
