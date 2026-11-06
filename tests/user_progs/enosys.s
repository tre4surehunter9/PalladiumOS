.intel_syntax noprefix
# exit(-syscall(999)) -> status 38 (ENOSYS)
    mov eax, 999
    int 0x80
    neg rax
    mov rdi, rax
    mov eax, 60
    int 0x80
