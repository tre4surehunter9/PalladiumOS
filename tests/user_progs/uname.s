.intel_syntax noprefix
# uname(rsp-400); exit 0 if sysname starts with "Palladiu", 2 if uname failed, 3 on mismatch
    sub rsp, 400
    mov rdi, rsp
    mov eax, 63
    int 0x80
    mov edi, 2
    test rax, rax
    jnz 1f
    mov edi, 3
    movabs rax, 0x756964616c6c6150
    cmp [rsp], rax
    jne 1f
    xor edi, edi
1:  mov eax, 60
    int 0x80
