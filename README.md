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
* uname - prints system info (`-a`, `-s`, `-n`, `-r`, `-v`, `-m`)
* crash - checks that bad pointers return EFAULT and that a segfault only kills the program

### Writing a Program
Programs are written in Rust in `userland/src/bin/` using `userlib` (syscalls, `print!`/`println!`, arguments).
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

Programs are static ELF executables loaded at `0x40000000`, with a 64 KiB stack (plus a guard page) and the standard System V startup stack (`argc`, `argv`, `envp`, `auxv`).
Files named `test_*` are used by the test suite and hidden from `run`.

## Syscalls
PalladiumOS uses the Linux x86_64 syscall ABI (`rax` = number, arguments in `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`, result in `rax`, negative errno on failure), entered with `int 0x80`.

| # | Name | Status |
|---|------|--------|
| 0 | read | yes (stdin is line-buffered, Ctrl-D is end-of-file) |
| 1 | write | yes |
| 2 | open | yes (`O_CREAT`, `O_EXCL`, `O_TRUNC`, `O_APPEND`, `O_DIRECTORY`) |
| 3 | close | yes |
| 4 | stat | yes |
| 5 | fstat | yes |
| 6 | lstat | yes (same as stat, no symlinks) |
| 8 | lseek | yes |
| 24 | sched_yield | yes |
| 39 | getpid | yes (always 1) |
| 60 | exit | yes |
| 63 | uname | yes |
| 79 | getcwd | yes |
| 80 | chdir | yes |
| 83 | mkdir | yes |
| 84 | rmdir | yes |
| 87 | unlink | yes |
| 217 | getdents64 | yes |
| 231 | exit_group | yes |
| 257 | openat | yes |

Each program gets its own file descriptors (0, 1 and 2 are the console) and a copy of the shell's working directory. Files left open are closed when it exits.

## Running the Tests
`cargo test` boots each test kernel in QEMU (no window) and reports results over serial.
* `tests/usermode.rs` - ring 3 programs, syscalls, exit statuses and fault handling
* `tests/filesystem.rs` - the filesystem and shell commands
* `tests/heap_allocation.rs`, `tests/stack_overflow.rs`, `tests/should_panic.rs`, `tests/basic_boot.rs` - kernel basics

The tiny user programs used by the tests live in `tests/user_progs` (`./build.sh` rebuilds them).

## Instructions for Running the Kernel
* Install QEMU
* Download the Latest Release
* Run this Command: qemu-system-x86_64 -drive format=raw,file=[location of BIN file
* The system is running

## Commands
* echo                - Echos back your Message
* about               - Kernel info
* clear               - Clear the screen
* help                - Show the help message
* reboot              - Reboots the system
* script <file>       - Runs script
* edit <file>         - Open file in the text editor
* ls <path>           - List directory
* cat <file>          - Print file contents
* mkdir <dir>         - Create directory
* rm <file>           - Remove file or empty directory
* cd  <dir>           - Change directory
* pwd                 - Print working directory
* run <program>       - Run a Ring 3 program

## Credits
Philipp Oppermann for his 'Writing an OS in Rust' guide
