/* Copyright (c) 2026 tre4surehunter9
 * libgreet.so: a small shared library for test_dyn (installed in /usr/lib
 * on the system disk). */
#include <stdio.h>

/* Set by the constructor: the dynamic linker runs it before main. */
int greet_ready;
/* Thread-local storage in a shared library: each thread has its own. */
__thread int greet_calls;

__attribute__((constructor)) static void setup(void)
{
    greet_ready = 42;
}

const char *greet(const char *name)
{
    static __thread char buf[64];
    greet_calls++;
    snprintf(buf, sizeof buf, "hello, %s", name);
    return buf;
}

int greet_count(void)
{
    return greet_calls;
}
