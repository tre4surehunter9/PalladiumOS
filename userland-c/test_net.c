/* Copyright (c) 2026 tre4surehunter9
 * test_net: checks sockets over the loopback interface (no outside
 * network needed). Exits 0, or the line number of the first failed check. */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHECK(c) do { if (!(c)) { fprintf(stderr, "test_net: check failed at line %d: %s (errno %d)\n", __LINE__, #c, errno); return __LINE__; } } while (0)

static struct sockaddr_in loopback(int port)
{
    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons(port);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    return a;
}

/* Read exactly n bytes (or until EOF). */
static ssize_t read_all(int fd, char *buf, size_t n)
{
    size_t got = 0;
    while (got < n) {
        ssize_t r = read(fd, buf + got, n - got);
        if (r <= 0)
            return r < 0 ? r : (ssize_t)got;
        got += r;
    }
    return got;
}

#define BIG (200 * 1024)

int main(void)
{
    /* ---- TCP: a server in a child, a client here ---- */
    int srv = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(srv >= 0);
    int one = 1;
    CHECK(setsockopt(srv, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one) == 0);
    struct sockaddr_in a = loopback(7000);
    CHECK(bind(srv, (struct sockaddr *)&a, sizeof a) == 0);
    CHECK(listen(srv, 4) == 0);
    struct sockaddr_in got;
    socklen_t gl = sizeof got;
    CHECK(getsockname(srv, (struct sockaddr *)&got, &gl) == 0 && ntohs(got.sin_port) == 7000);

    pid_t server = fork();
    if (server == 0) {
        /* echo server: two connections, then exit */
        for (int i = 0; i < 2; i++) {
            struct sockaddr_in peer;
            socklen_t pl = sizeof peer;
            int c = accept(srv, (struct sockaddr *)&peer, &pl);
            if (c < 0 || peer.sin_addr.s_addr != htonl(INADDR_LOOPBACK))
                _exit(10);
            char buf[4096];
            ssize_t n;
            while ((n = read(c, buf, sizeof buf)) > 0)
                if (write(c, buf, n) != n)
                    _exit(11);
            close(c);
        }
        _exit(0);
    }
    close(srv);

    int c = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(c >= 0);
    CHECK(connect(c, (struct sockaddr *)&a, sizeof a) == 0);
    gl = sizeof got;
    CHECK(getpeername(c, (struct sockaddr *)&got, &gl) == 0 && ntohs(got.sin_port) == 7000);
    CHECK(write(c, "hello", 5) == 5);
    char buf[64] = {0};
    CHECK(read_all(c, buf, 5) == 5 && memcmp(buf, "hello", 5) == 0);
    CHECK(shutdown(c, SHUT_WR) == 0);
    CHECK(read(c, buf, sizeof buf) == 0); /* the server closed after our FIN: EOF */
    close(c);

    /* a big transfer, both ways at once: needs the stack to keep up */
    char *out = malloc(BIG), *in = malloc(BIG);
    for (int i = 0; i < BIG; i++)
        out[i] = (char)(i * 7);
    c = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(connect(c, (struct sockaddr *)&a, sizeof a) == 0);
    pid_t writer = fork();
    if (writer == 0) {
        _exit(write(c, out, BIG) == BIG ? 0 : 1);
    }
    CHECK(read_all(c, in, BIG) == BIG);
    CHECK(memcmp(in, out, BIG) == 0);
    int st;
    CHECK(waitpid(writer, &st, 0) == writer && WEXITSTATUS(st) == 0);
    close(c);
    CHECK(waitpid(server, &st, 0) == server && WIFEXITED(st) && WEXITSTATUS(st) == 0);

    /* nothing listening: refused */
    struct sockaddr_in nobody = loopback(7999);
    c = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(connect(c, (struct sockaddr *)&nobody, sizeof nobody) == -1 && errno == ECONNREFUSED);
    close(c);

    /* ---- non-blocking: EINPROGRESS, poll, EAGAIN ---- */
    srv = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
    struct sockaddr_in b = loopback(7001);
    CHECK(bind(srv, (struct sockaddr *)&b, sizeof b) == 0 && listen(srv, 2) == 0);
    CHECK(accept(srv, NULL, NULL) == -1 && errno == EAGAIN);
    c = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(fcntl(c, F_SETFL, fcntl(c, F_GETFL) | O_NONBLOCK) == 0);
    CHECK(connect(c, (struct sockaddr *)&b, sizeof b) == -1 && errno == EINPROGRESS);
    struct pollfd pfd = {srv, POLLIN, 0};
    CHECK(poll(&pfd, 1, 5000) == 1 && (pfd.revents & POLLIN));
    int s2 = accept(srv, NULL, NULL);
    CHECK(s2 >= 0);
    pfd.fd = c;
    pfd.events = POLLOUT;
    CHECK(poll(&pfd, 1, 5000) == 1 && (pfd.revents & POLLOUT));
    int err = -1;
    socklen_t el = sizeof err;
    CHECK(getsockopt(c, SOL_SOCKET, SO_ERROR, &err, &el) == 0 && err == 0);
    CHECK(read(c, buf, sizeof buf) == -1 && errno == EAGAIN);
    CHECK(write(s2, "x", 1) == 1);
    pfd.events = POLLIN;
    CHECK(poll(&pfd, 1, 5000) == 1 && read(c, buf, sizeof buf) == 1 && buf[0] == 'x');
    close(s2);
    close(c);
    close(srv);

    /* ---- writing to a closed connection: EPIPE (and SIGPIPE) ---- */
    signal(SIGPIPE, SIG_IGN);
    srv = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in d = loopback(7002);
    CHECK(bind(srv, (struct sockaddr *)&d, sizeof d) == 0 && listen(srv, 1) == 0);
    c = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(connect(c, (struct sockaddr *)&d, sizeof d) == 0);
    s2 = accept(srv, NULL, NULL);
    close(s2);
    usleep(200 * 1000);
    int pipe_err = 0;
    for (int i = 0; i < 20 && !pipe_err; i++) {
        if (write(c, "data", 4) < 0)
            pipe_err = errno;
        usleep(20 * 1000);
    }
    CHECK(pipe_err == EPIPE || pipe_err == ECONNRESET);
    close(c);
    close(srv);

    /* ---- UDP ---- */
    int u1 = socket(AF_INET, SOCK_DGRAM, 0), u2 = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in ua = loopback(7100);
    CHECK(bind(u1, (struct sockaddr *)&ua, sizeof ua) == 0);
    CHECK(sendto(u2, "ping", 4, 0, (struct sockaddr *)&ua, sizeof ua) == 4);
    struct sockaddr_in from;
    socklen_t fl = sizeof from;
    CHECK(recvfrom(u1, buf, sizeof buf, 0, (struct sockaddr *)&from, &fl) == 4 && memcmp(buf, "ping", 4) == 0);
    CHECK(from.sin_addr.s_addr == htonl(INADDR_LOOPBACK) && ntohs(from.sin_port) != 0);
    CHECK(sendto(u1, "pong", 4, 0, (struct sockaddr *)&from, fl) == 4);
    CHECK(recv(u2, buf, sizeof buf, 0) == 4 && memcmp(buf, "pong", 4) == 0);
    CHECK(recv(u1, buf, sizeof buf, MSG_DONTWAIT) == -1 && errno == EAGAIN);
    close(u1);
    close(u2);

    /* ---- names: /etc/hosts ---- */
    struct addrinfo hints = {0}, *res;
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    CHECK(getaddrinfo("localhost", "80", &hints, &res) == 0);
    CHECK(((struct sockaddr_in *)res->ai_addr)->sin_addr.s_addr == htonl(INADDR_LOOPBACK));
    freeaddrinfo(res);

    printf("test_net: all checks passed\n");
    return 0;
}
