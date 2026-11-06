.intel_syntax noprefix
# exit(-write(1, 0xffff800000000000, 8)) -> status 14 (EFAULT)
    mov eax, 1
    mov edi, 1
    movabs rsi, 0xffff800000000000
    mov edx, 8
    int 0x80
    neg rax
    mov rdi, rax
    mov eax, 60
    int 0x80
