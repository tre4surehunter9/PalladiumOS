/* Copyright (c) 2026 tre4surehunter9
 * test_ipv6: IPv6 sockets over loopback (::1): TCP and UDP on AF_INET6,
 * an AF_INET6 listener taking IPv4 connections too (their address as
 * ::ffff:127.0.0.1), 28-byte socket addresses, an AF_INET socket refusing
 * an IPv6 address, ::1 in /proc/net/if_inet6, and an ICMPv6 echo through
 * a raw socket (the kernel fills in the checksum). Run as root. */
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#define CHECK(c) do { if (!(c)) { fprintf(stderr, "test_ipv6: check failed at line %d: %s (errno %d)\n", __LINE__, #c, errno); return __LINE__; } } while (0)

static struct sockaddr_in6 v6(const char *addr, int port)
{
    struct sockaddr_in6 a;
    memset(&a, 0, sizeof a);
    a.sin6_family = AF_INET6;
    a.sin6_port = htons(port);
    inet_pton(AF_INET6, addr, &a.sin6_addr);
    return a;
}

int main(void)
{
    char text[64];

    /* A TCP listener on [::]:5600 hears IPv6 and IPv4. */
    int l = socket(AF_INET6, SOCK_STREAM, 0);
    CHECK(l >= 0);
    struct sockaddr_in6 any = v6("::", 5600);
    CHECK(bind(l, (struct sockaddr *)&any, sizeof any) == 0);
    CHECK(listen(l, 4) == 0);
    struct sockaddr_in6 got;
    socklen_t len = sizeof got;
    CHECK(getsockname(l, (struct sockaddr *)&got, &len) == 0 && len == sizeof got && got.sin6_family == AF_INET6 &&
          ntohs(got.sin6_port) == 5600);

    /* Over IPv6. */
    int c = socket(AF_INET6, SOCK_STREAM, 0);
    struct sockaddr_in6 lo = v6("::1", 5600);
    CHECK(connect(c, (struct sockaddr *)&lo, sizeof lo) == 0);
    len = sizeof got;
    int s = accept(l, (struct sockaddr *)&got, &len);
    CHECK(s >= 0 && len == sizeof got && IN6_IS_ADDR_LOOPBACK(&got.sin6_addr));
    CHECK(write(c, "over six", 8) == 8);
    CHECK(read(s, text, sizeof text) == 8 && memcmp(text, "over six", 8) == 0);
    len = sizeof got;
    CHECK(getpeername(c, (struct sockaddr *)&got, &len) == 0 && IN6_IS_ADDR_LOOPBACK(&got.sin6_addr) &&
          ntohs(got.sin6_port) == 5600);
    close(c);
    close(s);

    /* Over IPv4, to the same listener: the peer as ::ffff:127.0.0.1. */
    int c4 = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in lo4 = { .sin_family = AF_INET, .sin_port = htons(5600) };
    lo4.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    CHECK(connect(c4, (struct sockaddr *)&lo4, sizeof lo4) == 0);
    len = sizeof got;
    s = accept(l, (struct sockaddr *)&got, &len);
    CHECK(s >= 0 && len == sizeof got);
    CHECK(inet_ntop(AF_INET6, &got.sin6_addr, text, sizeof text) && strcmp(text, "::ffff:127.0.0.1") == 0);
    CHECK(write(c4, "four", 4) == 4 && read(s, text, sizeof text) == 4);
    close(c4);
    close(s);
    close(l);

    /* An IPv4 socket takes no IPv6 address. */
    int bad = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(connect(bad, (struct sockaddr *)&lo, sizeof lo) == -1 && errno == EAFNOSUPPORT);
    close(bad);

    /* UDP over ::1, with the sender's address. */
    int u1 = socket(AF_INET6, SOCK_DGRAM, 0), u2 = socket(AF_INET6, SOCK_DGRAM, 0);
    struct sockaddr_in6 ua = v6("::1", 5601);
    CHECK(bind(u1, (struct sockaddr *)&ua, sizeof ua) == 0);
    CHECK(sendto(u2, "datagram", 8, 0, (struct sockaddr *)&ua, sizeof ua) == 8);
    struct pollfd pfd = { .fd = u1, .events = POLLIN };
    CHECK(poll(&pfd, 1, 2000) == 1);
    len = sizeof got;
    CHECK(recvfrom(u1, text, sizeof text, 0, (struct sockaddr *)&got, &len) == 8 && len == sizeof got &&
          IN6_IS_ADDR_LOOPBACK(&got.sin6_addr));
    /* A connected UDP socket's own address is the one it sends from. */
    CHECK(connect(u2, (struct sockaddr *)&ua, sizeof ua) == 0);
    len = sizeof got;
    CHECK(getsockname(u2, (struct sockaddr *)&got, &len) == 0 && IN6_IS_ADDR_LOOPBACK(&got.sin6_addr));
    close(u1);
    close(u2);

    /* ::1 is listed. */
    FILE *f = fopen("/proc/net/if_inet6", "r");
    CHECK(f != NULL);
    int has_lo = 0;
    while (fgets(text, sizeof text, f))
        if (strncmp(text, "00000000000000000000000000000001", 32) == 0)
            has_lo = 1;
    fclose(f);
    CHECK(has_lo);

    /* Ping ::1: an ICMPv6 echo request (checksum left 0: the kernel's job),
     * then its reply (type 129, same id and data). */
    int r = socket(AF_INET6, SOCK_RAW, IPPROTO_ICMPV6);
    CHECK(r >= 0);
    unsigned char echo[16] = { 128, 0, 0, 0, 0x12, 0x34, 0, 1, 'p', 'a', 'l', 'l', 'a', 'd', 'i', 'o' };
    CHECK(sendto(r, echo, sizeof echo, 0, (struct sockaddr *)&lo, sizeof lo) == sizeof echo);
    int replied = 0;
    for (int tries = 0; tries < 10 && !replied; tries++) {
        pfd.fd = r;
        if (poll(&pfd, 1, 500) != 1)
            continue;
        unsigned char in[128];
        len = sizeof got;
        int n = recvfrom(r, in, sizeof in, 0, (struct sockaddr *)&got, &len);
        if (n == sizeof echo && in[0] == 129 && in[4] == 0x12 && in[5] == 0x34 && memcmp(in + 8, echo + 8, 8) == 0)
            replied = 1;
    }
    CHECK(replied);
    close(r);

    printf("test_ipv6: all checks passed\n");
    return 0;
}
