/* Copyright (c) 2026 tre4surehunter9
 * test_siginfo: signals with their siginfo (SA_SIGINFO), handlers for
 * faults, and the alternate signal stack. Exits 0, or the line number of
 * the first failed check. */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>

#define CHECK(c) do { if (!(c)) { fprintf(stderr, "test_siginfo: check failed at line %d: %s (errno %d)\n", __LINE__, #c, errno); return __LINE__; } } while (0)

static volatile int got_sig, got_code, got_pid, got_uid, got_status, got_value;
static void *volatile got_addr;
static volatile int got_onstack;
static sigjmp_buf jump;
static char *volatile fix_page; /* SIGSEGV: map this and go back */

static void on_signal(int sig, siginfo_t *si, void *ucv)
{
    (void)ucv;
    got_sig = sig;
    got_code = si->si_code;
    got_pid = si->si_pid;
    got_uid = si->si_uid;
    got_status = si->si_status;
    got_value = si->si_value.sival_int;
    got_addr = si->si_addr;
    stack_t now;
    sigaltstack(NULL, &now);
    got_onstack = (now.ss_flags & SS_ONSTACK) != 0;
}

static void on_fault_jump(int sig, siginfo_t *si, void *ucv)
{
    on_signal(sig, si, ucv);
    siglongjmp(jump, 1);
}

static void on_fault_fix(int sig, siginfo_t *si, void *ucv)
{
    on_signal(sig, si, ucv);
    /* Map the page and return: the faulting instruction runs again. */
    mprotect(fix_page, 4096, PROT_READ | PROT_WRITE);
}

static void on_trap(int sig, siginfo_t *si, void *ucv)
{
    on_signal(sig, si, ucv); /* returns after the int3 */
}

static int handle(int sig, void (*fn)(int, siginfo_t *, void *), int flags)
{
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = fn;
    sa.sa_flags = SA_SIGINFO | flags;
    sigemptyset(&sa.sa_mask);
    return sigaction(sig, &sa, NULL);
}

static int sent_signals(void)
{
    CHECK(handle(SIGUSR1, on_signal, 0) == 0);
    CHECK(kill(getpid(), SIGUSR1) == 0);
    CHECK(got_sig == SIGUSR1 && got_code == SI_USER && got_pid == getpid() && got_uid == (int)getuid());
    /* sigqueue: a value with it. */
    CHECK(handle(SIGUSR2, on_signal, 0) == 0);
    union sigval v = { .sival_int = 4242 };
    CHECK(sigqueue(getpid(), SIGUSR2, v) == 0);
    CHECK(got_sig == SIGUSR2 && got_code == SI_QUEUE && got_value == 4242 && got_pid == getpid());
    /* raise: one thread (tkill). */
    CHECK(raise(SIGUSR1) == 0 && got_code == SI_TKILL);
    /* SIGCHLD: the child, how it ended. */
    CHECK(handle(SIGCHLD, on_signal, 0) == 0);
    got_sig = 0;
    pid_t c = fork();
    if (c == 0)
        _exit(7);
    while (got_sig != SIGCHLD)
        pause();
    CHECK(got_code == CLD_EXITED && got_pid == c && got_status == 7);
    CHECK(waitpid(c, NULL, 0) == c);
    got_sig = 0;
    c = fork();
    if (c == 0) {
        pause();
        _exit(0);
    }
    usleep(50000);
    CHECK(kill(c, SIGTERM) == 0);
    while (got_sig != SIGCHLD)
        pause();
    CHECK(got_code == CLD_KILLED && got_pid == c && got_status == SIGTERM);
    CHECK(waitpid(c, NULL, 0) == c);
    signal(SIGCHLD, SIG_DFL);
    return 0;
}

static int faults(void)
{
    /* SIGSEGV on an unmapped page: SEGV_MAPERR and the address. */
    CHECK(handle(SIGSEGV, on_fault_jump, 0) == 0);
    volatile int *bad = (int *)0x1238;
    if (sigsetjmp(jump, 1) == 0) {
        *bad = 1;
        CHECK(!"no fault");
    }
    CHECK(got_sig == SIGSEGV && got_code == SEGV_MAPERR && got_addr == (void *)0x1238);
    /* A PROT_NONE page: SEGV_ACCERR; the handler makes it writable and
     * returns, and the write happens. */
    char *page = mmap(NULL, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(page != MAP_FAILED);
    fix_page = page;
    CHECK(handle(SIGSEGV, on_fault_fix, 0) == 0);
    got_sig = 0;
    ((volatile char *)page)[100] = 'x'; /* (volatile: after got_sig = 0) */
    CHECK(got_sig == SIGSEGV && got_code == SEGV_ACCERR && got_addr == page + 100 && page[100] == 'x');
    munmap(page, 4096);
    /* SIGILL (ud2): ILL_ILLOPN at the instruction. */
    CHECK(handle(SIGILL, on_fault_jump, 0) == 0);
    static void *volatile ud2_at; /* (volatile: set before siglongjmp) */
    if (sigsetjmp(jump, 1) == 0) {
        void *at;
        __asm__ volatile("lea 1f(%%rip), %0\n\tmov %0, %1\n1: ud2" : "=&r"(at), "=m"(ud2_at));
        CHECK(!"no SIGILL");
    }
    CHECK(got_sig == SIGILL && got_code == ILL_ILLOPN && got_addr == ud2_at);
    /* SIGFPE: an integer division by zero. */
    CHECK(handle(SIGFPE, on_fault_jump, 0) == 0);
    volatile int zero = 0, n = 7;
    if (sigsetjmp(jump, 1) == 0) {
        n = n / zero;
        CHECK(!"no SIGFPE");
    }
    CHECK(got_sig == SIGFPE && got_code == FPE_INTDIV);
    /* SIGTRAP: int3 (a debugger's breakpoint); the handler returns. */
    CHECK(handle(SIGTRAP, on_trap, 0) == 0);
    got_sig = 0;
    __asm__ volatile("int3");
    CHECK(got_sig == SIGTRAP);
    /* No handler, or the signal blocked: the fault ends the program. */
    for (int blocked = 0; blocked < 2; blocked++) {
        pid_t c = fork();
        if (c == 0) {
            if (blocked) {
                sigset_t s;
                sigemptyset(&s);
                sigaddset(&s, SIGSEGV);
                sigprocmask(SIG_BLOCK, &s, NULL);
            } else {
                signal(SIGSEGV, SIG_DFL);
            }
            *(volatile int *)8 = 1;
            _exit(0);
        }
        int st;
        CHECK(waitpid(c, &st, 0) == c && WIFSIGNALED(st) && WTERMSIG(st) == SIGSEGV);
    }
    signal(SIGSEGV, SIG_DFL);
    signal(SIGILL, SIG_DFL);
    signal(SIGFPE, SIG_DFL);
    signal(SIGTRAP, SIG_DFL);
    return 0;
}

static char *alt_mem;

static int on_altstack(void)
{
    stack_t ss = { .ss_sp = alt_mem = malloc(65536), .ss_size = 65536, .ss_flags = 0 };
    stack_t old;
    CHECK(sigaltstack(&ss, &old) == 0 && old.ss_flags == SS_DISABLE);
    CHECK(sigaltstack(NULL, &old) == 0 && old.ss_sp == alt_mem && old.ss_size == 65536 && old.ss_flags == 0);
    stack_t tiny = { .ss_sp = alt_mem, .ss_size = 100, .ss_flags = 0 };
    CHECK(sigaltstack(&tiny, NULL) == -1 && errno == ENOMEM);
    CHECK(handle(SIGUSR1, on_signal, SA_ONSTACK) == 0);
    got_onstack = 0;
    raise(SIGUSR1);
    CHECK(got_onstack == 1);
    /* Without SA_ONSTACK: the normal stack. */
    CHECK(handle(SIGUSR1, on_signal, 0) == 0);
    raise(SIGUSR1);
    CHECK(got_onstack == 0);
    return 0;
}

/* A thread overflows its stack; its handler, on its alternate stack, gets
 * it out (as Rust and Go do to report a stack overflow). */
static int depth(int n)
{
    volatile char pad[512];
    pad[0] = (char)n;
    return n == 0 ? 0 : depth(n + 1) + pad[0];
}

static void *overflow_thread(void *arg)
{
    (void)arg;
    stack_t ss = { .ss_sp = malloc(65536), .ss_size = 65536, .ss_flags = 0 };
    sigaltstack(&ss, NULL);
    if (sigsetjmp(jump, 1) == 0)
        depth(1);
    return (void *)(intptr_t)(got_sig == SIGSEGV && got_onstack);
}

static int stack_overflow(void)
{
    CHECK(handle(SIGSEGV, on_fault_jump, SA_ONSTACK) == 0);
    got_sig = 0;
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, 128 * 1024);
    pthread_t t;
    CHECK(pthread_create(&t, &attr, overflow_thread, NULL) == 0);
    void *ok;
    CHECK(pthread_join(t, &ok) == 0 && ok == (void *)1);
    signal(SIGSEGV, SIG_DFL);
    return 0;
}

int main(void)
{
    int r;
    if ((r = sent_signals()) || (r = faults()) || (r = on_altstack()) || (r = stack_overflow()))
        return r;
    printf("test_siginfo: all checks passed\n");
    return 0;
}
