/* Copyright (c) 2026 tre4surehunter9
 * A plain Linux program, built by the host's gcc against glibc: checks that
 * PalladiumOS runs Linux binaries as they are. build.rs puts it on the
 * system disk twice: /usr/share/linux-test/hello (static, at the usual
 * 0x400000) and hello-dyn (dynamically linked, with glibc's dynamic linker
 * and libc.so.6 in /usr/lib). */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <unistd.h>

int main(int argc, char **argv)
{
    struct utsname u;
    if (uname(&u) != 0)
        return 1;
    char *p = malloc(1 << 20);
    memset(p, 7, 1 << 20);
    pid_t child = fork();
    if (child == 0)
        _exit(p[4096] == 7 ? 9 : 1);
    int status;
    waitpid(child, &status, 0);
    printf("glibc on %s: %d args, heap %d, child %d\n", u.sysname, argc - 1, p[12345], WEXITSTATUS(status));
    return 0;
}
