; crash: tests that the kernel survives bad user programs.
;   1. write() with a kernel pointer must fail with -EFAULT (-14)
;   2. reading address 0 must kill the program (Segmentation fault), not the kernel
; Build: nasm -f bin crash.asm -o crash.bin
BITS 64

start:
    mov eax, 1                      ; SYS_WRITE
    mov edi, 1                      ; stdout
    mov rsi, 0xffff800000000000     ; kernel address: not allowed
    mov edx, 8
    int 0x80
    cmp rax, -14                    ; -EFAULT?
    jne .fault
    mov eax, 1
    mov edi, 1
    lea rsi, [rel msg]
    mov edx, msglen
    int 0x80
.fault:
    mov rax, [0]                    ; unmapped -> page fault in ring 3
    ud2

msg:    db "write(kernel ptr) returned -EFAULT, good. Now segfaulting...", 10
msglen  equ $ - msg
