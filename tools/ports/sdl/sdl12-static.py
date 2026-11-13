# Copyright (c) 2026 tre4surehunter9
# Patches sdl12-compat (the SDL 1.2 API on SDL2) to use an SDL2 linked into
# the same static program instead of loading libSDL2 with dlopen (which
# static programs can't). Both define SDL_Init and the like, so SDL2's
# SDL_* symbols are renamed SDL2x_* in the copy of libSDL2.a it links
# (sdl.sh, with objcopy), and SDL12COMPAT_STATIC_SDL2 makes the loader look
# them up in a table built from SDL20_syms.h (the list it loads by name).
# Run with sdl12-compat's src/SDL12_compat.c.
import sys
p = sys.argv[1]
s = open(p).read()
if "SDL12COMPAT_STATIC_SDL2" in s:
    sys.exit(0)
old = "#elif defined(__unix__)\n    #include <dlfcn.h>\n    #define SDL20_LIBNAME \"libSDL2-2.0.so.0\""
assert s.count(old) == 1, "loader not found"
new = '''#elif defined(SDL12COMPAT_STATIC_SDL2)
    /* PalladiumOS: SDL2 is linked in, its SDL_ functions renamed SDL2x_. */
    #include <dlfcn.h>
    #define SDL20_REQUIRED_VER SDL_VERSIONNUM(2,0,7)
    static int Loaded_SDL20 = 0;
    #define SDL20_SYM(rc,fn,params,args,ret) extern void SDL2x_##fn(void);
    #include "SDL20_syms.h"
    static const struct { const char *name; void (*addr)(void); } SDL20_static_syms[] = {
    #define SDL20_SYM(rc,fn,params,args,ret) { "SDL_" #fn, SDL2x_##fn },
    #include "SDL20_syms.h"
        { NULL, NULL }
    };
    static void *LookupSDL20Static(const char *sym)
    {
        int i;
        for (i = 0; SDL20_static_syms[i].name; i++) {
            if (strcmp(SDL20_static_syms[i].name, sym) == 0) {
                return (void *) SDL20_static_syms[i].addr;
            }
        }
        return NULL;
    }
    #define LoadSDL20Library() (Loaded_SDL20 = 1)
    #define LookupSDL20Sym(sym) LookupSDL20Static(sym)
    #define CloseSDL20Library() { Loaded_SDL20 = 0; }
''' + old
s = s.replace(old, new)
open(p, "w").write(s)
