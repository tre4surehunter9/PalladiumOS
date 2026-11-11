/* Copyright (c) 2026 tre4surehunter9
 * FvwmTaskBar's "loadable goodies" (the clock, icons that run commands),
 * built into it: they are shared libraries normally, opened with dlopen,
 * which a static program can't. GoodyLoadable.c is compiled with dlopen,
 * dlsym, dlerror and dlclose renamed to these, which find the goodies in a
 * table instead (see tools/ports/fvwm95.sh). */
#include <string.h>
#include <X11/Xlib.h>
#include "GoodyLoadable.h"

extern struct GoodyLoadable ShowDateModuleSymbol;
extern struct GoodyLoadable GoodyModuleSymbol;

static const struct {
    const char *name;
    void *symbol;
} builtin[] = {
    {"ShowDateModuleSymbol", &ShowDateModuleSymbol},
    {"GoodyModuleSymbol", &GoodyModuleSymbol},
};

static const char *error;

/* Every "plugin" file is this program itself. */
void *pal_dlopen(const char *file, int mode)
{
    (void)file;
    (void)mode;
    return (void *)builtin;
}

void *pal_dlsym(void *handle, const char *name)
{
    unsigned i;
    (void)handle;
    for (i = 0; i < sizeof builtin / sizeof builtin[0]; i++) {
        if (strcmp(builtin[i].name, name) == 0) {
            error = NULL;
            return builtin[i].symbol;
        }
    }
    error = "not built into FvwmTaskBar";
    return NULL;
}

char *pal_dlerror(void)
{
    const char *e = error;
    error = NULL;
    return (char *)e;
}

int pal_dlclose(void *handle)
{
    (void)handle;
    return 0;
}
