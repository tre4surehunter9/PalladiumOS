/* Copyright (c) 2026 tre4surehunter9
 * test_dyn: a dynamically linked program (musl's dynamic linker loads it,
 * libc.so and libgreet.so). Exits 0, or the line number of the first
 * failed check. */
#include <dlfcn.h>
#include <link.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define CHECK(c) do { if (!(c)) { fprintf(stderr, "test_dyn: check failed at line %d: %s\n", __LINE__, #c); return __LINE__; } } while (0)

extern int greet_ready;
const char *greet(const char *name);
int greet_count(void);

static void *thread_main(void *arg)
{
    (void)arg;
    /* A new thread starts with its own (zero) TLS in the library. */
    greet("thread");
    greet("thread");
    return (void *)(long)greet_count();
}

static int found_greet;
static int each_library(struct dl_phdr_info *info, size_t size, void *data)
{
    (void)size;
    (void)data;
    if (info->dlpi_name && strstr(info->dlpi_name, "libgreet.so"))
        found_greet = 1;
    return 0;
}

int main(void)
{
    /* The library was loaded and its constructor ran before main. */
    CHECK(greet_ready == 42);
    CHECK(strcmp(greet("palladium"), "hello, palladium") == 0);
    CHECK(greet_count() == 1);

    /* Thread-local storage in the shared library, per thread. */
    pthread_t t;
    void *ret;
    CHECK(pthread_create(&t, NULL, thread_main, NULL) == 0);
    CHECK(pthread_join(t, &ret) == 0 && (long)ret == 2);
    CHECK(greet_count() == 1);

    /* dlopen gives the same, already loaded library; dlsym finds it. */
    void *h = dlopen("libgreet.so", RTLD_NOW);
    CHECK(h != NULL);
    const char *(*g)(const char *) = (const char *(*)(const char *))dlsym(h, "greet");
    CHECK(g == greet);
    CHECK(dlsym(h, "no_such_symbol") == NULL && dlerror() != NULL);
    CHECK(dlclose(h) == 0);
    CHECK(dlopen("libdoesnotexist.so", RTLD_NOW) == NULL && dlerror() != NULL);

    /* The loaded libraries can be listed. */
    dl_iterate_phdr(each_library, NULL);
    CHECK(found_greet);

    /* Ordinary libc works from the shared libc.so too. */
    char *p = malloc(100000);
    CHECK(p != NULL);
    memset(p, 'x', 100000);
    free(p);
    CHECK(getenv("PATH") != NULL);

    printf("test_dyn: all checks passed\n");
    return 0;
}
