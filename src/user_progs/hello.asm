BITS 64
ORG 0x666666660000

start:
    mov rax, 1          ; syscall number: write
    mov rdi, msg        ; arg1: pointer to the string
    mov rsi, msglen     ; arg2: length
    int 0x80

    mov rax, 0          ; syscall number: exit
    mov rdi, 0          ; arg1: exit code
    int 0x80

msg: db "Hi from syscall!", 0
msglen: equ $-msg-1
