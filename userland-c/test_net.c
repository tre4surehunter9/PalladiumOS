/* Copyright (c) 2026 tre4surehunter9
 * test_net: checks sockets over the loopback interface (no outside
 * network needed). Exits 0, or the line number of the first failed check. */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/ip.h>
#include <netinet/ip_icmp.h>
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

/* The Internet checksum (ICMP, IP headers). */
static unsigned short cksum(const void *data, int len)
{
    const unsigned short *p = data;
    unsigned long sum = 0;
    for (; len > 1; len -= 2)
        sum += *p++;
    if (len)
        sum += *(const unsigned char *)p;
    sum = (sum >> 16) + (sum & 0xffff);
    sum += sum >> 16;
    return (unsigned short)~sum;
}

/* Raw ICMP: an echo request to 127.0.0.1 and its reply (root only). */
static int raw_sockets(void)
{
    int r = socket(AF_INET, SOCK_RAW, IPPROTO_ICMP);
    CHECK(r >= 0);
    struct sockaddr_in to = loopback(0);
    unsigned char pkt[64] = {0};
    struct icmphdr *icmp = (struct icmphdr *)pkt;
    icmp->type = ICMP_ECHO;
    icmp->un.echo.id = htons(0x4242);
    icmp->un.echo.sequence = htons(7);
    memcpy(pkt + 8, "palladium", 9);
    icmp->checksum = cksum(pkt, sizeof pkt);
    CHECK(sendto(r, pkt, sizeof pkt, 0, (struct sockaddr *)&to, sizeof to) == sizeof pkt);
    /* Replies come with their IP header. (Our own request may come by too,
     * on loopback: skip it.) */
    for (int tries = 0; tries < 4; tries++) {
        unsigned char in[256];
        struct sockaddr_in from;
        socklen_t fl = sizeof from;
        struct pollfd pfd = {r, POLLIN, 0};
        CHECK(poll(&pfd, 1, 3000) == 1);
        ssize_t n = recvfrom(r, in, sizeof in, 0, (struct sockaddr *)&from, &fl);
        CHECK(n >= 20 + 8);
        struct iphdr *ip = (struct iphdr *)in;
        CHECK(ip->version == 4 && ip->protocol == IPPROTO_ICMP);
        CHECK(from.sin_addr.s_addr == htonl(INADDR_LOOPBACK));
        struct icmphdr *rep = (struct icmphdr *)(in + ip->ihl * 4);
        if (rep->type == ICMP_ECHO)
            continue;
        CHECK(rep->type == ICMP_ECHOREPLY && ntohs(rep->un.echo.id) == 0x4242 && ntohs(rep->un.echo.sequence) == 7);
        CHECK(memcmp((char *)rep + 8, "palladium", 9) == 0);
        close(r);

        /* IP_HDRINCL: we write the IP header ourselves */
        r = socket(AF_INET, SOCK_RAW, IPPROTO_ICMP);
        int on = 1;
        CHECK(setsockopt(r, IPPROTO_IP, IP_HDRINCL, &on, sizeof on) == 0);
        unsigned char full[20 + sizeof pkt];
        struct iphdr *h = (struct iphdr *)full;
        memset(full, 0, 20);
        h->version = 4;
        h->ihl = 5;
        h->tot_len = htons(sizeof full);
        h->ttl = 64;
        h->protocol = IPPROTO_ICMP;
        h->saddr = h->daddr = htonl(INADDR_LOOPBACK);
        icmp->un.echo.sequence = htons(8);
        icmp->checksum = 0;
        icmp->checksum = cksum(pkt, sizeof pkt);
        memcpy(full + 20, pkt, sizeof pkt);
        CHECK(sendto(r, full, sizeof full, 0, (struct sockaddr *)&to, sizeof to) == sizeof full);
        for (int t = 0; t < 4; t++) {
            struct pollfd p2 = {r, POLLIN, 0};
            CHECK(poll(&p2, 1, 3000) == 1);
            n = recv(r, in, sizeof in, 0);
            rep = (struct icmphdr *)(in + 20);
            if (n >= 28 && rep->type == ICMP_ECHOREPLY) {
                CHECK(ntohs(rep->un.echo.sequence) == 8);
                close(r);
                return 0;
            }
        }
        CHECK(!"no reply to the IP_HDRINCL echo");
    }
    CHECK(!"no echo reply");
    return 0;
}

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

    /* ---- raw sockets: root only ---- */
    if (geteuid() == 0) {
        int r = raw_sockets();
        if (r)
            return r;
        pid_t user = fork();
        if (user == 0) {
            if (setuid(1000) != 0)
                _exit(2);
            _exit(socket(AF_INET, SOCK_RAW, IPPROTO_ICMP) == -1 && errno == EPERM ? 0 : 1);
        }
        CHECK(waitpid(user, &st, 0) == user && WEXITSTATUS(st) == 0);
    } else {
        printf("test_net: not root, skipping raw sockets\n");
    }

    printf("test_net: all checks passed\n");
    return 0;
}
