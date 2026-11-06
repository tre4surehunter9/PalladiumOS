.intel_syntax noprefix
# Fill registers, do a syscall, then exit(0) if every non-rax register survived,
# or exit(N) where N is the 1-based index of the first clobbered register:
#   1=rbx 2=rcx 3=rdx 4=rsi 5=rdi 6=rbp 7=r8 8=r9 9=r10 10=r11 11=r12 12=r13 13=r14 14=r15
    mov rbx, 0x1111
    mov rcx, 0x2222
    mov rdx, 0x3333
    mov rsi, 0x4444
    mov rdi, 0x5555
    mov rbp, 0x6666
    mov r8, 0x7777
    mov r9, 0x8888
    mov r10, 0x9999
    mov r11, 0xaaaa
    mov r12, 0xbbbb
    mov r13, 0xcccc
    mov r14, 0xdddd
    mov r15, 0xeeee
    mov eax, 39   # getpid
    int 0x80
    mov rax, 0x1111
    cmp rbx, rax
    mov eax, 1
    jne 1f
    mov rax, 0x2222
    cmp rcx, rax
    mov eax, 2
    jne 1f
    mov rax, 0x3333
    cmp rdx, rax
    mov eax, 3
    jne 1f
    mov rax, 0x4444
    cmp rsi, rax
    mov eax, 4
    jne 1f
    mov rax, 0x5555
    cmp rdi, rax
    mov eax, 5
    jne 1f
    mov rax, 0x6666
    cmp rbp, rax
    mov eax, 6
    jne 1f
    mov rax, 0x7777
    cmp r8, rax
    mov eax, 7
    jne 1f
    mov rax, 0x8888
    cmp r9, rax
    mov eax, 8
    jne 1f
    mov rax, 0x9999
    cmp r10, rax
    mov eax, 9
    jne 1f
    mov rax, 0xaaaa
    cmp r11, rax
    mov eax, 10
    jne 1f
    mov rax, 0xbbbb
    cmp r12, rax
    mov eax, 11
    jne 1f
    mov rax, 0xcccc
    cmp r13, rax
    mov eax, 12
    jne 1f
    mov rax, 0xdddd
    cmp r14, rax
    mov eax, 13
    jne 1f
    mov rax, 0xeeee
    cmp r15, rax
    mov eax, 14
    jne 1f
    xor eax, eax
1:  mov edi, eax
    mov eax, 60
    int 0x80
