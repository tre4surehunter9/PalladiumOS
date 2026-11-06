/* Copyright (c) 2026 tre4surehunter9
 * chello: a C program, built with musl libc (see tools/build-musl.sh). */
#include <stdio.h>
#include <stdlib.h>
#include <sys/utsname.h>
#include <unistd.h>

int main(int argc, char **argv)
{
    struct utsname u;
    uname(&u);
    printf("Hello from C on %s %s!\n", u.sysname, u.release);
    printf("pid %d, %d argument(s), pi is about %.5f\n", (int)getpid(), argc - 1, 3.14159265358979);
    for (int i = 1; i < argc; i++)
        printf("  argv[%d] = %s\n", i, argv[i]);
    const char *user = getenv("USER");
    printf("USER=%s\n", user ? user : "(unset)");
    return 0;
}
