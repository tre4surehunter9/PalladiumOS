/* Copyright (c) 2026 tre4surehunter9
 * test_unix: checks Unix-domain sockets. Exits 0, or the line number of
 * the first failed check. Uses /tmp. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHECK(c) do { if (!(c)) { fprintf(stderr, "test_unix: check failed at line %d: %s (errno %d)\n", __LINE__, #c, errno); return __LINE__; } } while (0)

static socklen_t path_addr(struct sockaddr_un *a, const char *path)
{
    memset(a, 0, sizeof *a);
    a->sun_family = AF_UNIX;
    strcpy(a->sun_path, path);
    return offsetof(struct sockaddr_un, sun_path) + strlen(path) + 1;
}

static socklen_t abstract_addr(struct sockaddr_un *a, const char *name)
{
    memset(a, 0, sizeof *a);
    a->sun_family = AF_UNIX;
    memcpy(a->sun_path + 1, name, strlen(name)); /* sun_path[0] = 0 */
    return offsetof(struct sockaddr_un, sun_path) + 1 + strlen(name);
}

/* Send fds over a socket (SCM_RIGHTS) with one byte of data. */
static int send_fds(int sock, const int *fds, int n)
{
    char byte = 'F';
    struct iovec iov = { &byte, 1 };
    union { char buf[CMSG_SPACE(8 * sizeof(int))]; struct cmsghdr align; } u;
    struct msghdr m = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = u.buf, .msg_controllen = CMSG_SPACE(n * sizeof(int)) };
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(n * sizeof(int));
    memcpy(CMSG_DATA(c), fds, n * sizeof(int));
    return sendmsg(sock, &m, 0) == 1 ? 0 : -1;
}

/* Receive one byte and the fds sent with it (room for `room` fds);
 * how many arrived, -1 on error; *flags gets msg_flags. */
static int recv_fds(int sock, int *fds, int room, int *flags)
{
    char byte;
    struct iovec iov = { &byte, 1 };
    union { char buf[CMSG_SPACE(8 * sizeof(int))]; struct cmsghdr align; } u;
    struct msghdr m = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = u.buf, .msg_controllen = room ? CMSG_SPACE(room * sizeof(int)) : 0 };
    if (recvmsg(sock, &m, 0) != 1 || byte != 'F')
        return -1;
    *flags = m.msg_flags;
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    if (!c)
        return 0;
    if (c->cmsg_level != SOL_SOCKET || c->cmsg_type != SCM_RIGHTS)
        return -1;
    int n = (c->cmsg_len - CMSG_LEN(0)) / sizeof(int);
    memcpy(fds, CMSG_DATA(c), n * sizeof(int));
    return n;
}

static int passing_fds(void)
{
    char buf[64];
    int sv[2], fds[4], flags, st;

    /* A file passed over a stream pair: the new fd shares the open file
     * (and its offset), and works after the sender closes its own. */
    int f = open("/tmp/passed.txt", O_CREAT | O_TRUNC | O_RDWR, 0644);
    CHECK(f >= 0 && write(f, "passed along", 12) == 12 && lseek(f, 0, SEEK_SET) == 0);
    CHECK(socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0);
    CHECK(send_fds(sv[0], &f, 1) == 0);
    close(f);
    CHECK(recv_fds(sv[1], fds, 4, &flags) == 1 && flags == 0); /* (it may reuse f's number) */
    CHECK(read(fds[0], buf, sizeof buf) == 12 && memcmp(buf, "passed along", 12) == 0);
    close(fds[0]);

    /* Two at once (a pipe's ends): what's written to one comes out of the other. */
    int p[2];
    CHECK(pipe(p) == 0 && send_fds(sv[0], p, 2) == 0);
    close(p[0]);
    close(p[1]);
    CHECK(recv_fds(sv[1], fds, 4, &flags) == 2);
    CHECK(write(fds[1], "through", 7) == 7 && read(fds[0], buf, sizeof buf) == 7 && memcmp(buf, "through", 7) == 0);
    close(fds[0]);
    close(fds[1]);

    /* Plain data, then fds right after: a big plain read stops before the
     * fds' byte, and the fds come with it (not with the earlier data). */
    f = open("/tmp/passed.txt", O_RDONLY);
    CHECK(write(sv[0], "hello", 5) == 5 && send_fds(sv[0], &f, 1) == 0);
    close(f);
    CHECK(read(sv[1], buf, sizeof buf) == 5 && memcmp(buf, "hello", 5) == 0);
    CHECK(recv_fds(sv[1], fds, 4, &flags) == 1);
    close(fds[0]);

    /* No room for them: MSG_CTRUNC, and they're closed. */
    f = open("/tmp/passed.txt", O_RDONLY);
    CHECK(send_fds(sv[0], &f, 1) == 0);
    close(f);
    CHECK(recv_fds(sv[1], fds, 0, &flags) == 0 && (flags & MSG_CTRUNC));
    close(sv[0]);
    close(sv[1]);

    /* To another process: the child gets a pipe's write end and writes. */
    CHECK(socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0 && pipe(p) == 0);
    pid_t child = fork();
    if (child == 0) {
        close(sv[0]);
        close(p[0]);
        close(p[1]);
        int got[4], fl;
        if (recv_fds(sv[1], got, 4, &fl) != 1)
            _exit(1);
        _exit(write(got[0], "from the child", 14) == 14 ? 0 : 2);
    }
    close(sv[1]);
    CHECK(send_fds(sv[0], &p[1], 1) == 0);
    close(p[1]);
    CHECK(waitpid(child, &st, 0) == child && WIFEXITED(st) && WEXITSTATUS(st) == 0);
    CHECK(read(p[0], buf, sizeof buf) == 14 && memcmp(buf, "from the child", 14) == 0);
    close(p[0]);
    close(sv[0]);

    /* Datagrams carry them too. */
    CHECK(socketpair(AF_UNIX, SOCK_DGRAM, 0, sv) == 0);
    f = open("/tmp/passed.txt", O_RDONLY);
    CHECK(send_fds(sv[0], &f, 1) == 0);
    close(f);
    CHECK(recv_fds(sv[1], fds, 4, &flags) == 1 && read(fds[0], buf, 6) == 6 && memcmp(buf, "passed", 6) == 0);
    close(fds[0]);
    close(sv[0]);
    close(sv[1]);
    unlink("/tmp/passed.txt");
    return 0;
}

int main(void)
{
    char buf[256];
    int sv[2], st;
    mkdir("/tmp", 01777);
    signal(SIGPIPE, SIG_IGN);

    /* ---- socketpair, stream ---- */
    CHECK(socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0);
    CHECK(write(sv[0], "ping", 4) == 4 && read(sv[1], buf, sizeof buf) == 4 && memcmp(buf, "ping", 4) == 0);
    CHECK(write(sv[1], "pong", 4) == 4 && read(sv[0], buf, sizeof buf) == 4 && memcmp(buf, "pong", 4) == 0);
    struct ucred cred;
    socklen_t cl = sizeof cred;
    CHECK(getsockopt(sv[0], SOL_SOCKET, SO_PEERCRED, &cred, &cl) == 0);
    CHECK(cred.pid == getpid() && cred.uid == geteuid() && cred.gid == getegid());
    CHECK(shutdown(sv[0], SHUT_WR) == 0);
    CHECK(read(sv[1], buf, sizeof buf) == 0); /* end of file */
    close(sv[1]);
    CHECK(write(sv[0], "x", 1) == -1 && errno == EPIPE); /* SHUT_WR'd */
    close(sv[0]);

    /* across fork, both ways */
    CHECK(socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0);
    pid_t child = fork();
    if (child == 0) {
        close(sv[0]);
        ssize_t n = read(sv[1], buf, sizeof buf);
        for (ssize_t i = 0; i < n; i++)
            buf[i] ^= 0x20; /* swap case */
        _exit(write(sv[1], buf, n) == n ? 0 : 1);
    }
    close(sv[1]);
    CHECK(write(sv[0], "Hello", 5) == 5);
    CHECK(read(sv[0], buf, sizeof buf) == 5 && memcmp(buf, "hELLO", 5) == 0);
    CHECK(waitpid(child, &st, 0) == child && WEXITSTATUS(st) == 0);
    close(sv[0]);

    /* ---- socketpair, datagram: message boundaries ---- */
    CHECK(socketpair(AF_UNIX, SOCK_DGRAM, 0, sv) == 0);
    CHECK(send(sv[0], "one", 3, 0) == 3 && send(sv[0], "second", 6, 0) == 6);
    CHECK(recv(sv[1], buf, sizeof buf, 0) == 3 && recv(sv[1], buf, sizeof buf, 0) == 6 && memcmp(buf, "second", 6) == 0);
    CHECK(recv(sv[1], buf, sizeof buf, MSG_DONTWAIT) == -1 && errno == EAGAIN);
    close(sv[0]);
    close(sv[1]);

    /* ---- a server at a path ---- */
    unlink("/tmp/test.sock");
    struct sockaddr_un addr, got;
    socklen_t alen = path_addr(&addr, "/tmp/test.sock");
    int srv = socket(AF_UNIX, SOCK_STREAM, 0);
    CHECK(srv >= 0 && bind(srv, (struct sockaddr *)&addr, alen) == 0 && listen(srv, 4) == 0);
    struct stat sb;
    CHECK(stat("/tmp/test.sock", &sb) == 0 && S_ISSOCK(sb.st_mode)); /* the name is a socket file */
    CHECK(open("/tmp/test.sock", O_RDONLY) == -1 && errno == ENXIO); /* connected to, not opened */
    {
        /* also on ext2 (/tmp is in memory) */
        struct sockaddr_un ea;
        socklen_t el = path_addr(&ea, "/test_unix.sock");
        unlink("/test_unix.sock");
        int es = socket(AF_UNIX, SOCK_STREAM, 0);
        CHECK(es >= 0 && bind(es, (struct sockaddr *)&ea, el) == 0);
        CHECK(lstat("/test_unix.sock", &sb) == 0 && S_ISSOCK(sb.st_mode));
        close(es);
        CHECK(unlink("/test_unix.sock") == 0);
    }
    socklen_t gl = sizeof got;
    CHECK(getsockname(srv, (struct sockaddr *)&got, &gl) == 0 && strcmp(got.sun_path, "/tmp/test.sock") == 0);
    int again = socket(AF_UNIX, SOCK_STREAM, 0);
    CHECK(bind(again, (struct sockaddr *)&addr, alen) == -1 && errno == EADDRINUSE);
    close(again);
    child = fork();
    if (child == 0) {
        int c = socket(AF_UNIX, SOCK_STREAM, 0);
        if (connect(c, (struct sockaddr *)&addr, alen) != 0)
            _exit(1);
        struct sockaddr_un peer;
        socklen_t pl = sizeof peer;
        if (getpeername(c, (struct sockaddr *)&peer, &pl) != 0 || strcmp(peer.sun_path, "/tmp/test.sock") != 0)
            _exit(2);
        if (write(c, "request", 7) != 7)
            _exit(3);
        ssize_t n = read(c, buf, sizeof buf);
        _exit(n == 5 && memcmp(buf, "reply", 5) == 0 ? 0 : 4);
    }
    int conn = accept(srv, NULL, NULL);
    CHECK(conn >= 0);
    CHECK(read(conn, buf, sizeof buf) == 7 && memcmp(buf, "request", 7) == 0);
    CHECK(write(conn, "reply", 5) == 5);
    CHECK(waitpid(child, &st, 0) == child && WEXITSTATUS(st) == 0);
    CHECK(read(conn, buf, sizeof buf) == 0); /* the client is gone */
    close(conn);

    /* non-blocking: nothing waiting, then a connection to accept */
    CHECK(fcntl(srv, F_SETFL, O_NONBLOCK) == 0);
    CHECK(accept(srv, NULL, NULL) == -1 && errno == EAGAIN);
    int c = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0);
    CHECK(connect(c, (struct sockaddr *)&addr, alen) == 0);
    struct pollfd pfd = {srv, POLLIN, 0};
    CHECK(poll(&pfd, 1, 2000) == 1 && (pfd.revents & POLLIN));
    conn = accept(srv, NULL, NULL);
    CHECK(conn >= 0);
    CHECK(read(c, buf, sizeof buf) == -1 && errno == EAGAIN);
    close(conn);
    CHECK(write(c, "x", 1) == -1 && errno == EPIPE); /* the other end closed */
    close(c);
    close(srv);
    /* no listener now: refused; no file: no such file */
    c = socket(AF_UNIX, SOCK_STREAM, 0);
    CHECK(connect(c, (struct sockaddr *)&addr, alen) == -1 && errno == ECONNREFUSED);
    CHECK(unlink("/tmp/test.sock") == 0);
    CHECK(connect(c, (struct sockaddr *)&addr, alen) == -1 && errno == ENOENT);
    close(c);

    /* ---- the abstract namespace ---- */
    alen = abstract_addr(&addr, "palladium-test");
    srv = socket(AF_UNIX, SOCK_STREAM, 0);
    CHECK(bind(srv, (struct sockaddr *)&addr, alen) == 0 && listen(srv, 1) == 0);
    c = socket(AF_UNIX, SOCK_STREAM, 0);
    CHECK(connect(c, (struct sockaddr *)&addr, alen) == 0);
    conn = accept(srv, NULL, NULL);
    CHECK(conn >= 0 && write(c, "abs", 3) == 3 && read(conn, buf, sizeof buf) == 3);
    close(c);
    close(conn);
    close(srv);

    /* ---- datagrams by path, with the sender's name ---- */
    unlink("/tmp/dg-a.sock");
    unlink("/tmp/dg-b.sock");
    struct sockaddr_un a1, a2, from;
    socklen_t l1 = path_addr(&a1, "/tmp/dg-a.sock"), l2 = path_addr(&a2, "/tmp/dg-b.sock");
    int d1 = socket(AF_UNIX, SOCK_DGRAM, 0), d2 = socket(AF_UNIX, SOCK_DGRAM, 0);
    CHECK(bind(d1, (struct sockaddr *)&a1, l1) == 0 && bind(d2, (struct sockaddr *)&a2, l2) == 0);
    CHECK(sendto(d2, "hi", 2, 0, (struct sockaddr *)&a1, l1) == 2);
    socklen_t fl = sizeof from;
    CHECK(recvfrom(d1, buf, sizeof buf, 0, (struct sockaddr *)&from, &fl) == 2);
    CHECK(strcmp(from.sun_path, "/tmp/dg-b.sock") == 0);
    CHECK(sendto(d1, "back", 4, 0, (struct sockaddr *)&from, fl) == 4);
    CHECK(recv(d2, buf, sizeof buf, 0) == 4 && memcmp(buf, "back", 4) == 0);
    close(d1);
    close(d2);
    unlink("/tmp/dg-a.sock");
    unlink("/tmp/dg-b.sock");

    /* ---- permissions: connecting needs write permission on the file ---- */
    if (geteuid() == 0) {
        unlink("/tmp/priv.sock");
        alen = path_addr(&addr, "/tmp/priv.sock");
        srv = socket(AF_UNIX, SOCK_STREAM, 0);
        CHECK(bind(srv, (struct sockaddr *)&addr, alen) == 0 && listen(srv, 1) == 0);
        CHECK(chmod("/tmp/priv.sock", 0600) == 0);
        child = fork();
        if (child == 0) {
            if (setuid(1000) != 0)
                _exit(2);
            int u = socket(AF_UNIX, SOCK_STREAM, 0);
            _exit(connect(u, (struct sockaddr *)&addr, alen) == -1 && errno == EACCES ? 0 : 1);
        }
        CHECK(waitpid(child, &st, 0) == child && WEXITSTATUS(st) == 0);
        close(srv);
        unlink("/tmp/priv.sock");
    }

    int r = passing_fds();
    if (r)
        return r;

    printf("test_unix: all checks passed\n");
    return 0;
}
