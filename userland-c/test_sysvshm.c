/* Copyright (c) 2026 tre4surehunter9
 * test_sysvshm: System V shared memory (shmget, shmat, shmdt, shmctl):
 * segments by key and IPC_PRIVATE, sharing with a child (not copied on
 * write), several attachments, IPC_STAT, removal while attached, read-only
 * attachments, fixed addresses (SHM_REMAP), permissions, ipcs's queries.
 * Run as root. */
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/shm.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHECK(c) do { if (!(c)) { fprintf(stderr, "test_sysvshm: check failed at line %d: %s (errno %d)\n", __LINE__, #c, errno); return __LINE__; } } while (0)

static void on_segv(int sig)
{
    (void)sig;
    _exit(42);
}

static int nattch(int id)
{
    struct shmid_ds ds;
    if (shmctl(id, IPC_STAT, &ds) != 0)
        return -1;
    return (int)ds.shm_nattch;
}

int main(void)
{
    /* A private segment: three pages' worth, zero-filled. */
    int id = shmget(IPC_PRIVATE, 10000, IPC_CREAT | 0600);
    CHECK(id >= 0);
    char *a = shmat(id, NULL, 0);
    CHECK(a != (void *)-1);
    CHECK(a[0] == 0 && a[9999] == 0);
    strcpy(a, "hello");
    a[9999] = 'z';
    struct shmid_ds ds;
    CHECK(shmctl(id, IPC_STAT, &ds) == 0);
    CHECK(ds.shm_segsz == 10000 && ds.shm_nattch == 1 && ds.shm_cpid == getpid());
    CHECK((ds.shm_perm.mode & 0777) == 0600 && ds.shm_perm.uid == getuid());

    /* A child sees it, and its writes are seen (shared, not copied). */
    pid_t pid = fork();
    CHECK(pid >= 0);
    if (pid == 0) {
        if (strcmp(a, "hello") != 0 || a[9999] != 'z')
            _exit(1);
        strcpy(a + 100, "from the child");
        _exit(0);
    }
    int status;
    CHECK(waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    CHECK(strcmp(a + 100, "from the child") == 0);
    CHECK(nattch(id) == 1); /* (the child's went away with it) */

    /* A second attachment: the same memory. */
    char *b = shmat(id, NULL, 0);
    CHECK(b != (void *)-1 && b != a);
    CHECK(strcmp(b, "hello") == 0);
    b[1] = 'E';
    CHECK(a[1] == 'E');
    CHECK(nattch(id) == 2);
    CHECK(shmdt(b) == 0);
    CHECK(nattch(id) == 1);
    CHECK(shmdt(b) == -1 && errno == EINVAL); /* (nothing there now) */

    /* By key. */
    key_t key = 0x50414c44; /* "PALD" */
    int k = shmget(key, 4096, IPC_CREAT | IPC_EXCL | 0644);
    CHECK(k >= 0 && k != id);
    CHECK(shmget(key, 4096, IPC_CREAT | IPC_EXCL | 0644) == -1 && errno == EEXIST);
    CHECK(shmget(key, 0, 0) == k);
    CHECK(shmget(key, 100, IPC_CREAT | 0644) == k);
    CHECK(shmget(key, 8192, 0) == -1 && errno == EINVAL); /* (bigger than it is) */
    CHECK(shmget(0x7777, 4096, 0) == -1 && errno == ENOENT);

    /* Removed while attached: still usable, but its key and id are gone
     * for new attachments, and it's freed with the last detach. */
    char *c = shmat(k, NULL, 0);
    CHECK(c != (void *)-1);
    strcpy(c, "kept");
    CHECK(shmctl(k, IPC_RMID, NULL) == 0);
    CHECK(strcmp(c, "kept") == 0);
    CHECK(shmget(key, 0, 0) == -1 && errno == ENOENT);
    CHECK(shmat(k, NULL, 0) == (void *)-1 && errno == EINVAL);
    CHECK(shmctl(k, IPC_STAT, &ds) == 0 && (ds.shm_perm.mode & 01000)); /* SHM_DEST */
    CHECK(shmdt(c) == 0);
    CHECK(shmctl(k, IPC_STAT, &ds) == -1 && errno == EINVAL);

    /* Read-only: writing faults (SIGSEGV; caught, so the kernel doesn't
     * log a crash: the soak test counts those). */
    pid = fork();
    CHECK(pid >= 0);
    if (pid == 0) {
        signal(SIGSEGV, on_segv);
        volatile char *r = shmat(id, NULL, SHM_RDONLY);
        if (r == (void *)-1 || r[0] != 'h')
            _exit(1);
        r[0] = 'x';
        _exit(2);
    }
    CHECK(waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 42);
    CHECK(a[0] == 'h');

    /* At a given address; over a mapping only with SHM_REMAP. */
    char *want = a + 0x10000000;
    char *d = shmat(id, want, 0);
    CHECK(d == want && strcmp(d, "hEllo") == 0);
    CHECK(shmat(id, want, 0) == (void *)-1 && errno == EINVAL);
    CHECK(shmat(id, want + 1, 0) == (void *)-1 && errno == EINVAL); /* (not page-aligned) */
    char *e = shmat(id, want, SHM_REMAP);
    CHECK(e == want && e[0] == 'h');
    CHECK(shmdt(want) == 0);
    /* SHM_RND: rounded down to the page. */
    char *f = shmat(id, want + 5, SHM_RND);
    CHECK(f == want && f[0] == 'h');
    CHECK(shmdt(want) == 0);

    /* Permissions: another user can't attach a 0600 segment or remove it. */
    pid = fork();
    CHECK(pid >= 0);
    if (pid == 0) {
        if (setuid(1000) != 0)
            _exit(1);
        if (shmat(id, NULL, 0) != (void *)-1 || errno != EACCES)
            _exit(2);
        if (shmctl(id, IPC_RMID, NULL) != -1 || errno != EPERM)
            _exit(3);
        if (shmctl(id, IPC_STAT, &ds) != -1 || errno != EACCES)
            _exit(4);
        _exit(0);
    }
    CHECK(waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0);

    /* IPC_SET: the mode changed. */
    CHECK(shmctl(id, IPC_STAT, &ds) == 0);
    ds.shm_perm.mode = 0640;
    CHECK(shmctl(id, IPC_SET, &ds) == 0);
    CHECK(shmctl(id, IPC_STAT, &ds) == 0 && (ds.shm_perm.mode & 0777) == 0640);

    /* What ipcs asks. (musl names shm_info.used_ids __used_ids.) */
    struct shm_info info;
    CHECK(shmctl(0, SHM_INFO, (struct shmid_ds *)&info) >= 0 && info.__used_ids >= 1);
    int found = 0;
    for (int i = 0; i < 64; i++) {
        int r = shmctl(i, SHM_STAT, &ds);
        if (r == id)
            found = 1;
    }
    CHECK(found);

    CHECK(shmctl(id, IPC_RMID, NULL) == 0);
    CHECK(shmdt(a) == 0);
    CHECK(shmctl(id, IPC_STAT, &ds) == -1 && errno == EINVAL);
    printf("test_sysvshm: all checks passed\n");
    return 0;
}
