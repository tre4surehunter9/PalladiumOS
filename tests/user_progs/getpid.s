.intel_syntax noprefix
# exit(getpid()) -> status 1
    mov eax, 39
    int 0x80
    mov rdi, rax
    mov eax, 60
    int 0x80
