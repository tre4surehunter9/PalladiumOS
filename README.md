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
Ring 3 programs live in `src/user_progs` and are run with `run <name>`:
* hello - prints a message and exits
* crash - checks that bad pointers return EFAULT and that a segfault only kills the program

## Syscalls
PalladiumOS uses the Linux x86_64 syscall ABI (`rax` = number, arguments in `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`, result in `rax`, negative errno on failure), entered with `int 0x80`.

| # | Name | Status |
|---|------|--------|
| 0 | read | not yet (returns -ENOSYS) |
| 1 | write | stdout/stderr only |
| 24 | sched_yield | yes |
| 39 | getpid | yes (always 1) |
| 60 | exit | yes |
| 63 | uname | yes |
| 231 | exit_group | yes |

## Building from Source
The repo includes a `rust-toolchain.toml`, so `rustup` installs the right nightly toolchain and components automatically.
* Install [rustup](https://rustup.rs) and QEMU
* Install bootimage: `cargo install bootimage`
* Build: `cargo bootimage`
* Run: `cargo run` (boots it in QEMU)

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
