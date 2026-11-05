; hello: a tiny ring-3 test program for PalladiumOS.
; Uses the Linux x86_64 syscall ABI:
;   rax = syscall number, args in rdi, rsi, rdx, r10, r8, r9, result in rax.
; Build: nasm -f bin hello.asm -o hello.bin
; The code is position-independent (RIP-relative), so the load address doesn't matter.
BITS 64

SYS_WRITE equ 1
SYS_EXIT  equ 60
STDOUT    equ 1

start:
    mov eax, SYS_WRITE
    mov edi, STDOUT         ; fd
    lea rsi, [rel msg]      ; buf
    mov edx, msglen         ; count
    int 0x80

    mov eax, SYS_EXIT
    xor edi, edi            ; status 0
    int 0x80

msg:    db "Hello from ring 3!", 10
msglen  equ $ - msg
