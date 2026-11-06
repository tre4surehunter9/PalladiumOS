.intel_syntax noprefix
# exit(-write(5, buf, 1)) -> status 9 (EBADF)
    mov eax, 1
    mov edi, 5
    lea rsi, [rip]
    mov edx, 1
    int 0x80
    neg rax
    mov rdi, rax
    mov eax, 60
    int 0x80
