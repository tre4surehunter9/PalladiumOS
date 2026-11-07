# PalladiumOS (Formerly FrostDOS)

## An OS (more of a kernel) coded entirely in Rust

![Screenshot](Screenshot_20260811_212123.png)

## Warning
Currently compling using 'cargo build' is broken for some Goddamn reason so it only works on my pc I really dont know why.

## Notice
I do want to make things clear and say i used AI for some parts of my code as this is my first kernel so I dont understand much yet

i used AI for
* the filesystem
* the shell
* text editor (as a guide)
* finding bugs
* guiding for implimenting stuff
* ring 3/usermode stuff

## Features
* Basic Shell
* Heap Allocation
* Keyboard Input
* Filesystem
* Scripting
* Text Editor
* Ring 3/Usermode

## Programs
User programs run in ring 3 and are started with `run <name> [args...]` (`run` alone lists them):
* hello - prints a greeting
* echo - prints its arguments (`-n` for no newline)
* cat - prints files, or what you type until Ctrl-D
* ls - lists directories (`-a` for hidden entries, `-l` for type and size)
* mkdir - creates directories (`-p` for parents)
* rmdir - removes empty directories
* rm - removes files (`-r` for directories, `-f` to ignore missing files)
* touch - creates empty files
* pwd - prints the working directory
* env - prints the environment
* clear - clears the screen
* reboot / poweroff - restarts or turns off the machine
* sh - the shell
* edit - full-screen text editor (arrows, Home/End, PageUp/PageDown; Ctrl+S save, Ctrl+Q quit)
* about - information about PalladiumOS
* sleep - waits for a number of seconds (`sleep 0.5` works)
* kill - sends a signal (`kill -9 PID`, `kill -l`)
* uptime - time since boot
* date - the date and time (`date +%s` for seconds since 1970)
* mv / cp - move and copy files (`cp -r` for directories; `mv` works across filesystems)
* ln - hard links, or symbolic links with `-s`; readlink shows where a link points
* chmod - permissions, octal (`755`) or symbolic (`u+x,go-w`)
* df / du - free disk space, and space used by files (`-h` for K/M/G, `du -s` totals)
* stat - a file's type, size, inode, links, permissions and time
* ps - list processes (PID, parent, state, CPU time, memory, command)
* top - live process view, refreshed every second (`q` quits)
* free - memory use (`-h` for M/G); mount - what's mounted where
* pgrep / pkill - find or signal processes by name (`-l`, `-x`, `pkill -9 name`)
* sync - writes cached data to the disk
* grep - prints matching lines (`-i -v -n -c -q -F`; `.` `*` `^` `$` patterns)
* wc - counts lines, words and bytes (`-l -w -c`)
* head / tail - first or last lines (`-n N`)
* sort - sorts lines (`-r -n -u`)
* uniq - collapses repeated lines (`-c -d`)
* tee - copies input to the screen and to files (`-a` appends)
* yes - repeats a line forever (try `yes | head -n 3`)
* uname - prints system info (`-a`, `-s`, `-n`, `-r`, `-v`, `-m`)

### Writing a Program
Programs are written in Rust in `userland/src/bin/` using `userlib` (syscalls, `print!`/`println!`, arguments, environment, and a heap so `Vec`, `String` and `Box` work).
Every file there is built and bundled into the kernel automatically by `build.rs`:

```rust
#![no_std]
#![no_main]

use userlib::println;

userlib::entry!(main);

fn main(args: userlib::Args) -> i32 {
    println!("Hello from {}", args.get(0).unwrap_or("?"));
    0 // exit status
}
```

Programs are static ELF executables loaded at `0x8000000000`, with a 64 KiB stack (plus a guard page) and the standard System V startup stack (`argc`, `argv`, `envp`, `auxv`).
Files named `test_*` are used by the test suite and hidden from `run`.

## Syscalls
PalladiumOS uses the Linux x86_64 syscall ABI (`rax` = number, arguments in `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`, result in `rax`, negative errno on failure), entered with `int 0x80` (PalladiumOS's own programs) or the `syscall` instruction (what Linux programs and musl use).

| # | Name | Status |
|---|------|--------|
| 0 | read | yes (stdin is line-buffered, Ctrl-D is end-of-file) |
| 1 | write | yes |
| 7 / 271 | poll, ppoll | yes (pipes, the terminal; files are always ready) |
| 9 / 10 / 11 | mmap, mprotect, munmap | anonymous and private file mappings (no writable shared file mappings) |
| 17 / 18 | pread64, pwrite64 | yes |
| 19 / 20 | readv, writev | yes |
| 76 / 77 | truncate, ftruncate | yes |
| 97 / 302 | getrlimit, prlimit64 | reports the limits (setting is ignored) |
| 158 | arch_prctl | FS/GS base (thread-local storage) |
| 218 | set_tid_address | yes |
| 230 | clock_nanosleep | yes (`TIMER_ABSTIME` too) |
| 318 | getrandom | yes (RDRAND when available) |
| 332 | statx | yes |
| 41-55, 288 | socket, connect, accept, sendto, recvfrom, sendmsg, recvmsg, shutdown, bind, listen, getsockname, getpeername, setsockopt, getsockopt, accept4 | `AF_INET` TCP and UDP |
| 40 | sendfile | yes |
| 2 | open | yes (`O_CREAT`, `O_EXCL`, `O_TRUNC`, `O_APPEND`, `O_DIRECTORY`) |
| 3 | close | yes |
| 4 | stat | yes |
| 5 | fstat | yes |
| 6 | lstat | yes (same as stat, no symlinks) |
| 8 | lseek | yes |
| 21 | access | yes |
| 82 | rename | yes (also renameat, renameat2 without flags) |
| 86 | link | yes (also linkat; not on in-memory filesystems) |
| 88 | symlink | yes (also symlinkat) |
| 89 | readlink | yes (also readlinkat) |
| 90 | chmod | yes (also fchmod, fchmodat) |
| 95 | umask | yes |
| 137 | statfs | yes (also fstatfs) |
| 258-263 | mkdirat, newfstatat, unlinkat | yes |
| 280 | utimensat | modification time |
| 22 | pipe | yes |
| 12 | brk | yes (the heap behind `userlib`'s allocator) |
| 13 | rt_sigaction | yes (handlers, `SA_RESTART`, `SA_NODEFER`, `SA_RESETHAND`, `SA_NOCLDSTOP`, `sa_mask`) |
| 14 | rt_sigprocmask | yes |
| 15 | rt_sigreturn | yes |
| 34 | pause | yes |
| 36/38 | getitimer, setitimer | `ITIMER_REAL` |
| 37 | alarm | yes |
| 127 | rt_sigpending | yes |
| 130 | rt_sigsuspend | yes |
| 186 | gettid | yes (`getpid` is the process, `gettid` the thread) |
| 200/234 | tkill, tgkill | yes |
| 16 | ioctl | console: `TCGETS`/`TCSETS` (raw and cooked mode), `TIOCGWINSZ`, `TIOCGPGRP`/`TIOCSPGRP` |
| 24 | sched_yield | yes |
| 32 | dup | yes |
| 33 | dup2 | yes |
| 35 | nanosleep | yes (55 ms timer resolution) |
| 39 | getpid | yes |
| 57 | fork | yes (copies the whole address space) |
| 56 | clone | fork, vfork-style `CLONE_VM \| CLONE_VFORK` (what `posix_spawn`, `system` and `popen` use), and threads (`CLONE_THREAD` with `SETTLS`, `PARENT_SETTID`, `CHILD_SETTID`, `CHILD_CLEARTID`) |
| 202 | futex | `WAIT`, `WAKE` (and `_BITSET`), `REQUEUE`, `CMP_REQUEUE`, timeouts |
| 58 | vfork | yes (same as fork, which POSIX allows) |
| 59 | execve | yes (static ELF executables) |
| 60 | exit | yes |
| 61 | wait4 | yes (`WNOHANG`, `WUNTRACED`, `WCONTINUED`, process groups; no rusage) |
| 62 | kill | yes (pid, process group, or -1 for all; `EPERM` for other users' processes) |
| 92/93/94/260 | chown, fchown, lchown, fchownat | yes |
| 102/104/107/108 | getuid, getgid, geteuid, getegid | yes |
| 105/106/113/114/117/119 | setuid, setgid, setreuid, setregid, setresuid, setresgid | yes (POSIX rules) |
| 115/116 | getgroups, setgroups | yes |
| 118/120 | getresuid, getresgid | yes |
| 72 | fcntl | `F_DUPFD`, `F_GETFD`/`F_SETFD` (close-on-exec), `F_GETFL` |
| 109 | setpgid | yes |
| 111 | getpgrp | yes |
| 121 | getpgid | yes |
| 63 | uname | yes |
| 79 | getcwd | yes |
| 80 | chdir | yes |
| 83 | mkdir | yes |
| 84 | rmdir | yes |
| 87 | unlink | yes |
| 110 | getppid | yes |
| 162 | sync | yes |
| 169 | reboot | yes (restart, power off, halt; syncs first) |
| 217 | getdents64 | yes |
| 228 | clock_gettime | yes (time since boot) |
| 231 | exit_group | yes |
| 257 | openat | yes |
| 292 | dup3 | yes (`O_CLOEXEC`) |
| 293 | pipe2 | yes (`O_CLOEXEC`) |

## Networking
PalladiumOS has TCP/IP: a driver for the RTL8139 network card (what `cargo run` gives QEMU, on its user network), the [smoltcp](https://github.com/smoltcp-rs/smoltcp) stack, and BSD sockets.
* `eth0` is configured by DHCP at boot (QEMU: 10.0.2.15, gateway 10.0.2.2, DNS 10.0.2.3, written to `/etc/resolv.conf`); `lo` (127.0.0.1) works with or without a card. `/etc/hosts` has `localhost`.
* Sockets: `AF_INET` TCP and UDP: `socket`, `bind`, `listen`, `accept`/`accept4`, `connect`, `send`/`recv`(`to`/`from`/`msg`), `shutdown`, `getsockname`/`getpeername`, `setsockopt`/`getsockopt` (`SO_ERROR`), `poll`, non-blocking mode (`SOCK_NONBLOCK`, `O_NONBLOCK`, `FIONBIO`, `MSG_DONTWAIT`), `EPIPE`/`SIGPIPE`. Closing with unread data resets the connection, as on Linux.
* So BusyBox's `wget`, `nslookup`, `nc`, `telnet` and `httpd`, MicroPython's `socket`, and C programs with musl's resolver work: `wget http://example.com/` downloads a page, and the host is reachable at 10.0.2.2.
* Not yet: IPv6, Unix-domain sockets, raw sockets (so no `ping`), more than one card type.

## Threads
Programs can have threads: musl's pthreads (mutexes, condition variables, semaphores, `pthread_join`, `pthread_kill`, thread-local storage) works unchanged, and MicroPython's `_thread` too. A thread shares its process's memory, open files, working directory and signal handlers; it has its own registers, stack, TLS, signal mask and pending signals. `kill` sends to a process (to a thread that isn't blocking the signal), `tgkill` to one thread. `exit` ends a thread, `exit_group` (or a fatal signal) the whole process; a process whose first thread called `pthread_exit` lives until its last thread ends. Limits: one CPU (threads take turns), and only a process's first thread may `execve`.

## C Programs (musl libc)
C programs run on PalladiumOS with an unmodified [musl](https://musl.libc.org) libc: PalladiumOS speaks the Linux x86_64 syscall interface musl was written for, saves the SSE registers programs use, supports thread-local storage, `mmap`, and gives programs the aux vector libc's startup reads.

```sh
tools/build-musl.sh        # downloads musl 1.2.5 (checksum-verified) and builds it into target/musl
cargo run                  # build.rs now compiles every userland-c/*.c into /bin
```

Then run `chello` or `test_libc` (a test of stdio, malloc, files, mmap, fork/exec, signals, time...) in the shell. Add a program by putting `NAME.c` in `userland-c/`, or compile one yourself with `target/musl/bin/palladium-cc prog.c -o prog -lm`. Programs are static position-independent executables (static-PIE): PalladiumOS has no dynamic linker yet. Ordinary static Linux binaries run too, unchanged (even glibc ones built by a Linux host's `gcc -static`): the kernel lives in the higher half (at `0xffffffff80000000`, with all physical memory mapped from `0xffff800000000000`), so the whole lower half, including the usual `0x400000`, is for programs.

### Ported Software
Real-world programs are built from their original source by scripts in `tools/ports/` (after `tools/build-musl.sh`). `tools/build-all.sh` builds musl (if needed) and every port that's missing or out of date (`tools/build-all.sh tcc` rebuilds just that one, `--rebuild` everything, `--run` then boots with `cargo run`). Each downloads a pinned, checksum-verified release and installs into `target/ports/bin`, which goes into `/bin`:

* `tools/ports/lua.sh`: Lua 5.4.7 (`lua` and `luac`). The REPL, scripts (also as `#!/bin/lua`), `io`, `os.execute`, `io.popen` and `os.clock` work.
* `tools/ports/dash.sh`: dash 0.5.12, the Debian Almquist shell, a full POSIX shell (here-documents, `getopts`, `trap`, job control with Ctrl-Z/`fg`/`bg`). Run `dash`, or `dash script.sh`.
* `tools/ports/tcc.sh`: TinyCC, a C compiler that runs inside PalladiumOS. Write a program with `edit hi.c`, then `tcc hi.c -o hi && ./hi`, or `tcc -run hi.c`. musl's headers and libraries are on the system disk at `/usr` (read-only, rebuilt from `target/sysroot` every build as `target/usr.img`), together with tcc's own source: `tcc -o tcc2 /usr/src/tinycc/tcc.c -I/usr/src/tinycc` builds tcc with itself (and the result builds an identical copy). tcc is patched to link statically by default.
* `tools/ports/busybox.sh`: BusyBox 1.38.0, about 400 Unix tools in one program (`vi`, `awk`, `sed`, `find`, `tar`, `gzip`, `diff`, `cmp`, `cut`, `xargs`, `printf`, `less`, `md5sum`, `expr`, the `ash` shell...). Their links are in `/usr/bin` on the system disk, and `PATH` is `/bin:/usr/bin`. Where BusyBox's version is clearly better (`cp date env grep head mv readlink sort tail touch uniq`), `/bin/NAME` links to it and PalladiumOS's own version moves to `/bin/palladium`. The rest of PalladiumOS's tools (`ls`, `ps`, `top`, `free`, `stat`, the account tools, `edit`...) stay the defaults: they match its `/proc`, password format and tests. `busybox NAME` runs BusyBox's version of anything; without BusyBox built, PalladiumOS's own tools are used.
* `tools/ports/micropython.sh`: MicroPython 1.29.0 (the unix port, without threads, FFI or TLS). The `>>>` REPL, scripts (also as `#!/bin/micropython`), files, `os.system`, `time`, `json`, `re`, `hashlib`..., and Ctrl-C as `KeyboardInterrupt`. The heap is 2 MB by default (`micropython -X heapsize=8M` for more).
* `tools/ports/fastfetch.sh`: fastfetch 2.71.0 (needs `cmake` on the host). Shows the system with a PalladiumOS logo (configured in `/etc/xdg/fastfetch/config.jsonc`; the OS name comes from `/etc/os-release`).

The binaries are ordinary Linux static executables too, so they can be run on a Linux host to compare behaviour.

## Processes
Every program runs as a process with its own address space, kernel stack, open files and working directory.
The scheduler is preemptive round-robin: the timer switches between processes running in ring 3.
Programs can start other programs with `fork` + `execve` and wait for them with `wait4` (`userlib::spawn` wraps this).
The kernel shell is task 0: `run` starts a program as its child and waits for it (and anything it started) to finish.
Orphaned processes are handed to the kernel, which reaps them. Up to 32 processes can exist at once.

Paths follow symbolic links (up to 40 per lookup, then `ELOOP`). Files have permission bits (`chmod`, and `umask` for new files) and `execve` needs an execute bit; it also runs `#!` scripts. 

## Users and Permissions
PalladiumOS boots into `login`. A fresh disk has two accounts, `root` and `user` (uid 1000), with no passwords: set them with `passwd`. Logging out returns to login. Ctrl-D at the login prompt opens the kernel shell (which runs as root), and asks for root's password first once root has one. login runs each session as a child and stays in charge, so nothing a user's shell does (like `exit 99`) can open the kernel shell.

* Each process has real, effective and saved user/group IDs and supplementary groups (`fork` copies them). Files have an owner and a group (stored in ext2 inodes).
* Permissions are enforced POSIX-style: read/write/execute for owner, group and others; search (`x`) is needed on every directory in a path; creating, removing and renaming need write permission on the directory; in sticky directories (like `/tmp`, mode 1777) only a file's owner may remove it. Root may read and write anything.
* Only the owner may `chmod` a file; only root may `chown` it to someone else. `access()` checks with the real IDs.
* Setuid/setgid programs run as the file's owner/group (`su` and `passwd` are setuid root); the bits are ignored on `#!` scripts.
* Accounts live in `/etc/passwd`, `/etc/group` and `/etc/shadow` (mode 600). Passwords are hashed with iterated, salted SHA-256 (`$sha256$salt$hash`, PalladiumOS's own format).
* Tools: `login`, `su [-] [user] [-c cmd]`, `passwd [-d] [user]`, `whoami`, `id`, `groups`, `useradd [-u uid] [-G groups] name`, `userdel [-r] name`, `chown user[:group]`, `chgrp`. `ls -l` shows owners, and the prompt shows `user@palladium:~ $` (`#` for root).

Open files work like POSIX: an fd refers to a shared open file description, so after `fork` or `dup` both fds share one file offset. Pipes hold 16 KiB; readers wait for data and get end-of-file when every writer has closed, writers wait for room and get SIGPIPE/`EPIPE` when no reader is left. `O_CLOEXEC` fds are closed by `execve`.

Signals work as in POSIX:
* Handlers (`sigaction`), with the Linux x86_64 signal frame and `rt_sigreturn`, so a C library's handlers work unchanged. A handler can interrupt a program in the middle of a computation (the timer interrupt saves every register), not only at syscalls.
* Signal masks (`sigprocmask`, `sigpending`, `sigsuspend`), `pause`, `alarm` and `setitimer`, `SIGCHLD`.
* Blocking syscalls interrupted by a handler fail with `EINTR`, or restart with `SA_RESTART`. Ignored and stop/continue signals never interrupt them: a `sleep 10` stopped and continued still sleeps 10 s.
* Job control: SIGSTOP/SIGTSTP/SIGTTIN/SIGTTOU stop a process, SIGCONT continues it, and `wait4` reports both. Ctrl-C sends SIGINT, Ctrl-Z SIGTSTP and Ctrl-\ SIGQUIT to the foreground process group. As POSIX says, Ctrl-Z doesn't stop orphaned process groups (like programs the kernel started), which nobody could continue.
* `execve` resets handlers to the default (ignored signals and the mask are kept).
* Not yet: `SA_SIGINFO` details (the siginfo has only the signal number), `sigaltstack`, handlers for faults (a segfault always ends the program).
The console understands the usual ANSI escape sequences (cursor movement, clearing, colors and reverse video), so full-screen programs like `edit` work.

Programs live in `/bin` as ordinary files, installed at boot: `run ls` runs `/bin/ls`, and `run /path/to/program` runs any ELF file.

## Running the Tests
`cargo test` boots each test kernel in QEMU (no window) and reports results over serial.
* `tests/usermode.rs` - ring 3 programs, syscalls, exit statuses and fault handling
* `tests/filesystem.rs` - the filesystem and shell commands
* `tests/ext2.rs` - the ext2 driver, the VFS, `/dev`, the ATA disk driver and the clock
* `tests/heap_allocation.rs`, `tests/stack_overflow.rs`, `tests/should_panic.rs`, `tests/basic_boot.rs` - kernel basics

The tiny user programs used by the tests live in `tests/user_progs` (`./build.sh` rebuilds them).

## Disk and Filesystems
`cargo run` attaches a 32 MiB virtual disk, `target/disk.img`, formatted as **ext2** (the classic Linux filesystem). It's mounted at `/`, so **files you create are saved across reboots**. `build.rs` creates it with `mke2fs` (from e2fsprogs) the first time; delete it to start fresh.

| Path | Filesystem | |
|------|------------|---|
| `/` | ext2 on the disk | saved; falls back to memory if there's no disk |
| `/bin` | memory | the bundled programs, refreshed every boot |
| `/tmp` | memory | cleared on reboot |
| `/dev` | devfs | `null`, `zero`, `tty`, `console` |
| `/proc` | procfs | live process and system information (below) |

The kernel has a small VFS layer: every filesystem implements one interface, and paths are resolved through the mount table. The ext2 driver reads and writes files (including indirect blocks), creates and removes files and directories, and frees a file deleted while open only when it's closed. Linux tools can check and read the disk: `e2fsck -f target/disk.img`, `debugfs -R "ls -l /" target/disk.img`. Run `sync` (or `poweroff`/`reboot`, which sync first) before closing QEMU.

`/proc` works like Linux's: `/proc/PID/` has `status`, `stat`, `cmdline`, and symlinks `cwd`, `exe` and `fd/N` (what each open file is: a path, `/dev/tty` or `pipe:[...]`); `/proc/self` is the reading process. System files: `meminfo`, `uptime`, `loadavg`, `mounts`, `version`, `cpuinfo`. The contents are generated when read, so they're always current.

The date comes from the PC's real-time clock, so file times and `date` are real.

## Booting
The kernel reports each boot step like Linux with systemd: timestamped facts (`[    0.164] CPU: ...`) and a green `[  OK  ]`, yellow `[ WARN ]` or red `[FAILED]` for each step (paging, heap, interrupts, disks, mounts, programs...), then the login prompt. The log is kept: `dmesg` shows it again (`dmesg -n` without colors), and it's in `/proc/kmsg`.

## Instructions for Running the Kernel
* Install QEMU
* Download the Latest Release
* Run this Command: qemu-system-x86_64 -drive format=raw,file=[location of BIN file
* The system is running

## The Shell
After you log in, you get `/bin/sh`, a POSIX-style shell that runs in userland as a normal process.

* Run programs by name (`ls -l /bin`) or by path (`/bin/echo hi`); names are looked up in `$PATH`
* Quoting: `'single'`, `"double with $VARS"`, `\` escapes, `# comments`
* Variables: `x=1`, `$x`, `${x}`, `$?` (last exit status), `$$`, `$#`, `$0`-`$9`, `~`
* Environment: `export NAME=value`, `unset NAME`, `NAME=value command` for one command
* Pipelines: `ls /bin | grep ^s | wc -l` (Ctrl-C stops the whole pipeline)
* Lists: `cmd1; cmd2`, `cmd1 && cmd2`, `cmd1 || cmd2`, background jobs with `cmd &`
* Redirection: `> file`, `>> file`, `< file`, `2> file`, `2>&1`
* Scripts: `sh script.sh args...`, `sh -c 'commands'`, `. file` / `source file`
* Scripts can be run directly: `chmod +x script.sh; ./script.sh` (with a `#!/bin/sh` line, or without one)
* Options: `sh -e` / `set -e` (stop at the first failing command), `sh -x` / `set -x` (print commands as they run)
* Job control: Ctrl-Z stops the running command; `jobs` lists jobs, `fg [%N]` and `bg [%N]` continue them
* Traps: `trap 'commands' INT TERM ...`, `trap '' SIG` (ignore), `trap - SIG` (default), `trap '...' EXIT`
* `help` lists topics (files, text, processes, system, users, programming, busybox, builtins, syntax, keys); `help TOPIC` shows one, `help NAME` describes a command
* Builtins: `cd`, `pwd`, `exit`, `export`, `unset`, `set`, `wait`, `jobs`, `fg`, `bg`, `trap`, `kill` (understands `%1`), `.`, `source`, `help`, `true`, `false`, and `run`/`script` like the kernel shell
* Keys: Up/Down for history, Left/Right/Home/End to edit the line, Tab completes commands and file names, Ctrl-C interrupts the running program (or clears the line), Ctrl-Z stops it, Ctrl-D exits, Ctrl-L clears the screen

Not yet: here-documents (`<<EOF`), aliases, `local`.

### Kernel Shell
Typing `exit` in `sh` (or Ctrl-D) drops to the built-in kernel shell, which is kept as a fallback (background jobs are sent SIGHUP). Type `run sh` to go back. Kernel shell commands:
* echo                - Echos back your Message
* about               - Kernel info
* clear               - Clear the screen
* help                - Show the help message
* reboot              - Reboots the system
* script <file>       - Runs script
* edit <file>         - Open file in the kernel's text editor
* ls <path>           - List directory
* cat <file>          - Print file contents
* mkdir <dir>         - Create directory
* rm <file>           - Remove file or empty directory
* cd  <dir>           - Change directory
* pwd                 - Print working directory
* run <program>       - Run a program from /bin (or a path)

## Credits
Philipp Oppermann for his 'Writing an OS in Rust' guide
