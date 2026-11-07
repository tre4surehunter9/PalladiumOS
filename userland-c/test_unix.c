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
    CHECK(stat("/tmp/test.sock", &sb) == 0); /* the name is a file */
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

    printf("test_unix: all checks passed\n");
    return 0;
}
