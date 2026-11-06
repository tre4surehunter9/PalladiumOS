.intel_syntax noprefix
# try to write kernel memory directly -> page fault -> status 139
    movabs rax, 0xffff800000000000
    mov qword ptr [rax], 1
