/* Copyright (c) 2026 tre4surehunter9
 * _dl_find_object for musl: the build machine's libgcc_eh (GCC's exception
 * unwinder, built for glibc) finds a function's unwind tables with this
 * glibc 2.35 function, which musl doesn't have. Here it is on musl's
 * dl_iterate_phdr: the loaded object that holds `pc`, its extent, and its
 * .eh_frame_hdr (the PT_GNU_EH_FRAME segment), as glibc returns them. */
#define _GNU_SOURCE
#include <link.h>
#include <stdint.h>

struct dl_find_object {
    unsigned long long dlfo_flags;
    void *dlfo_map_start;
    void *dlfo_map_end;
    struct link_map *dlfo_link_map;
    void *dlfo_eh_frame;
    unsigned long long __dlfo_reserved[7];
};

struct search {
    uintptr_t pc;
    struct dl_find_object *out;
    int found;
};

static int look(struct dl_phdr_info *info, size_t size, void *data)
{
    struct search *s = data;
    uintptr_t lo = UINTPTR_MAX, hi = 0;
    const ElfW(Phdr) *eh = 0;
    int inside = 0;
    (void)size;
    for (int i = 0; i < info->dlpi_phnum; i++) {
        const ElfW(Phdr) *p = &info->dlpi_phdr[i];
        if (p->p_type == PT_LOAD) {
            uintptr_t start = info->dlpi_addr + p->p_vaddr, end = start + p->p_memsz;
            if (start < lo)
                lo = start;
            if (end > hi)
                hi = end;
            if (s->pc >= start && s->pc < end)
                inside = 1;
        } else if (p->p_type == PT_GNU_EH_FRAME) {
            eh = p;
        }
    }
    if (!inside)
        return 0;
    s->out->dlfo_flags = 0;
    s->out->dlfo_map_start = (void *)lo;
    s->out->dlfo_map_end = (void *)hi;
    s->out->dlfo_link_map = 0;
    s->out->dlfo_eh_frame = eh ? (void *)(info->dlpi_addr + eh->p_vaddr) : 0;
    s->found = 1;
    return 1;
}

int _dl_find_object(void *pc, struct dl_find_object *result)
{
    struct search s = { (uintptr_t)pc, result, 0 };
    dl_iterate_phdr(look, &s);
    return s.found && result->dlfo_eh_frame ? 0 : -1;
}
