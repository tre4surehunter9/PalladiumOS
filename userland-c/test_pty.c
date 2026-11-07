/* Copyright (c) 2026 tre4surehunter9
 * test_pty: checks pseudo-terminals, sessions and select. Exits 0, or the
 * line number of the first failed check. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pty.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/select.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <termios.h>
#include <time.h>
#include <poll.h>
#include <unistd.h>
#include <utmp.h>

#define CHECK(c) do { if (!(c)) { fprintf(stderr, "test_pty: check failed at line %d: %s (errno %d)\n", __LINE__, #c, errno); return __LINE__; } } while (0)

/* Read from the master until `want` bytes have come (or nothing more does). */
static int read_master(int m, char *buf, int want)
{
    int got = 0;
    while (got < want) {
        fd_set r;
        FD_ZERO(&r);
        FD_SET(m, &r);
        struct timeval tv = {2, 0};
        if (select(m + 1, &r, NULL, NULL, &tv) <= 0)
            break;
        ssize_t n = read(m, buf + got, want - got);
        if (n <= 0)
            break;
        got += n;
    }
    buf[got] = 0;
    return got;
}

static volatile sig_atomic_t winched;
static void on_winch(int s) { (void)s; winched = 1; }

int main(void)
{
    int m, s, st;
    char name[64], buf[256];
    pid_t child;

    /* ---- a pair ---- */
    CHECK(openpty(&m, &s, name, NULL, NULL) == 0);
    CHECK(strncmp(name, "/dev/pts/", 9) == 0);
    CHECK(isatty(m) && isatty(s));
    CHECK(ttyname(s) && strcmp(ttyname(s), name) == 0);
    struct stat sb;
    CHECK(stat(name, &sb) == 0 && S_ISCHR(sb.st_mode));
    CHECK(strcmp(ptsname(m), name) == 0);

    /* typed line: echoed to the master ("\r\n"), handed to the slave whole */
    CHECK(write(m, "hello\r", 6) == 6);
    CHECK(read(s, buf, sizeof buf) == 6 && memcmp(buf, "hello\n", 6) == 0);
    CHECK(read_master(m, buf, 7) == 7 && strcmp(buf, "hello\r\n") == 0);
    /* program output: "\n" becomes "\r\n" */
    CHECK(write(s, "out\n", 4) == 4);
    CHECK(read_master(m, buf, 5) == 5 && strcmp(buf, "out\r\n") == 0);
    /* line editing: DEL erases, ^U kills the line */
    CHECK(write(m, "abx\x7f" "c\n", 6) == 6);
    CHECK(read(s, buf, sizeof buf) == 4 && memcmp(buf, "abc\n", 4) == 0);
    CHECK(write(m, "junk\x15ok\n", 8) == 8);
    CHECK(read(s, buf, sizeof buf) == 3 && memcmp(buf, "ok\n", 3) == 0);
    /* ^D on an empty line: end-of-file; on a non-empty one: the line */
    CHECK(write(m, "\x04", 1) == 1);
    CHECK(read(s, buf, sizeof buf) == 0);
    CHECK(write(m, "ab\x04", 3) == 3);
    CHECK(read(s, buf, sizeof buf) == 2);
    tcflush(m, TCIOFLUSH);
    read_master(m, buf, sizeof buf - 1); /* drain echoes */

    /* raw mode: bytes at once, no echo */
    struct termios t;
    CHECK(tcgetattr(s, &t) == 0 && (t.c_lflag & ICANON) && (t.c_lflag & ECHO));
    cfmakeraw(&t);
    CHECK(tcsetattr(s, TCSANOW, &t) == 0);
    CHECK(write(m, "xyz", 3) == 3);
    CHECK(read(s, buf, sizeof buf) == 3 && memcmp(buf, "xyz", 3) == 0);
    CHECK(fcntl(m, F_SETFL, O_NONBLOCK) == 0);
    CHECK(read(m, buf, sizeof buf) == -1 && errno == EAGAIN); /* no echo */
    CHECK(fcntl(m, F_SETFL, 0) == 0);
    t.c_cc[VMIN] = 0;
    t.c_cc[VTIME] = 0;
    CHECK(tcsetattr(s, TCSANOW, &t) == 0);
    CHECK(read(s, buf, sizeof buf) == 0); /* VMIN 0, VTIME 0: don't wait */
    t.c_cc[VTIME] = 2;
    CHECK(tcsetattr(s, TCSANOW, &t) == 0);
    CHECK(read(s, buf, sizeof buf) == 0); /* waits 0.2 s, then gives up */

    /* window size */
    struct winsize ws = {40, 120, 0, 0}, got;
    CHECK(ioctl(m, TIOCSWINSZ, &ws) == 0);
    CHECK(ioctl(s, TIOCGWINSZ, &got) == 0 && got.ws_row == 40 && got.ws_col == 120);
    close(s);
    close(m);

    /* ---- a session on a terminal: setsid + login_tty ---- */
    CHECK(openpty(&m, &s, name, NULL, NULL) == 0);
    child = fork();
    if (child == 0) /* not a session leader: no terminal for it */
        _exit(ioctl(s, TIOCSCTTY, 0) == -1 && errno == EPERM ? 0 : 1);
    CHECK(waitpid(child, &st, 0) == child && WIFEXITED(st) && WEXITSTATUS(st) == 0);
    child = fork();
    if (child == 0) {
        close(m);
        if (login_tty(s) != 0)
            _exit(1);
        if (getsid(0) != getpid() || tcgetpgrp(0) != getpid())
            _exit(2);
        int tty = open("/dev/tty", O_RDWR); /* the controlling terminal */
        if (tty < 0 || !ttyname(0) || strcmp(ttyname(0), ttyname(tty)) != 0)
            _exit(3);
        signal(SIGWINCH, on_winch);
        printf("ready\n");
        fflush(stdout);
        /* the window change, then wait for ^C */
        for (int i = 0; i < 200 && !winched; i++)
            usleep(10000);
        if (!winched)
            _exit(4);
        printf("winched\n");
        fflush(stdout);
        for (;;)
            pause();
    }
    close(s);
    CHECK(read_master(m, buf, 7) == 7 && strcmp(buf, "ready\r\n") == 0);
    ws.ws_row = 30;
    CHECK(ioctl(m, TIOCSWINSZ, &ws) == 0); /* SIGWINCH for the foreground */
    CHECK(read_master(m, buf, 9) == 9 && strcmp(buf, "winched\r\n") == 0);
    CHECK(write(m, "\x03", 1) == 1); /* ^C: SIGINT */
    CHECK(waitpid(child, &st, 0) == child && WIFSIGNALED(st) && WTERMSIG(st) == SIGINT);
    CHECK(read_master(m, buf, 4) >= 2 && strncmp(buf, "^C", 2) == 0); /* echoed */
    /* every slave fd is closed: the master reads EIO */
    CHECK(read(m, buf, sizeof buf) == -1 && errno == EIO);
    close(m);

    /* ---- hangup: closing the master sends SIGHUP ---- */
    CHECK(openpty(&m, &s, name, NULL, NULL) == 0);
    int sync_pipe[2];
    CHECK(pipe(sync_pipe) == 0);
    child = fork();
    if (child == 0) {
        close(m);
        close(sync_pipe[0]);
        if (login_tty(s) != 0)
            _exit(1);
        write(sync_pipe[1], "x", 1);
        read(0, buf, sizeof buf); /* until hung up */
        _exit(5);
    }
    close(s);
    close(sync_pipe[1]);
    CHECK(read(sync_pipe[0], buf, 1) == 1);
    usleep(100000);
    close(m);
    CHECK(waitpid(child, &st, 0) == child && WIFSIGNALED(st) && WTERMSIG(st) == SIGHUP);
    close(sync_pipe[0]);

    /* ---- a background group reading the terminal: SIGTTIN stops it ---- */
    CHECK(openpty(&m, &s, name, NULL, NULL) == 0);
    child = fork();
    if (child == 0) {
        close(m);
        if (login_tty(s) != 0)
            _exit(1);
        signal(SIGTTIN, SIG_DFL);
        pid_t bg = fork();
        if (bg == 0) {
            setpgid(0, 0); /* its own group, not the foreground one */
            read(0, buf, sizeof buf);
            _exit(0);
        }
        int bst;
        if (waitpid(bg, &bst, WUNTRACED) != bg)
            _exit(2);
        int ok = WIFSTOPPED(bst) && WSTOPSIG(bst) == SIGTTIN;
        kill(bg, SIGKILL);
        waitpid(bg, &bst, 0);
        _exit(ok ? 0 : 3);
    }
    close(s);
    CHECK(waitpid(child, &st, 0) == child && WIFEXITED(st) && WEXITSTATUS(st) == 0);
    close(m);

    /* ---- select: timeouts, and a pipe becoming readable ---- */
    int p[2];
    CHECK(pipe(p) == 0);
    fd_set r;
    FD_ZERO(&r);
    FD_SET(p[0], &r);
    struct timeval tv = {0, 100000};
    CHECK(select(p[0] + 1, &r, NULL, NULL, &tv) == 0 && !FD_ISSET(p[0], &r));
    CHECK(write(p[1], "!", 1) == 1);
    FD_ZERO(&r);
    FD_SET(p[0], &r);
    fd_set w;
    FD_ZERO(&w);
    FD_SET(p[1], &w);
    tv.tv_sec = 1;
    CHECK(select(p[1] + 1, &r, &w, NULL, &tv) == 2 && FD_ISSET(p[0], &r) && FD_ISSET(p[1], &w));
    /* waiting sleeps: a process in select or poll is asleep (S in
     * /proc/PID/stat), not running, and the timeouts work */
    CHECK(read(p[0], buf, 1) == 1); /* (empty the pipe again) */
    for (int use_poll = 0; use_poll < 2; use_poll++) {
        pid_t w = fork();
        if (w == 0) {
            if (use_poll) {
                struct pollfd pfd = {p[0], POLLIN, 0};
                _exit(poll(&pfd, 1, 1000) == 0 ? 0 : 1);
            }
            fd_set rr;
            FD_ZERO(&rr);
            FD_SET(p[0], &rr);
            struct timeval t = {1, 0};
            _exit(select(p[0] + 1, &rr, NULL, NULL, &t) == 0 ? 0 : 1);
        }
        usleep(400000);
        char path[64], stat_line[256] = "";
        snprintf(path, sizeof path, "/proc/%d/stat", (int)w);
        FILE *f = fopen(path, "r");
        CHECK(f && fgets(stat_line, sizeof stat_line, f));
        fclose(f);
        char *end = strrchr(stat_line, ')'); /* "pid (name) S ..." */
        CHECK(end && end[1] == ' ' && end[2] == 'S');
        CHECK(waitpid(w, &st, 0) == w && WIFEXITED(st) && WEXITSTATUS(st) == 0);
    }
    close(p[0]);
    close(p[1]);

    printf("test_pty: all checks passed\n");
    return 0;
}
