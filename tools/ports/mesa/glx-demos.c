/* Copyright (c) 2026 tre4surehunter9
 * glxgears, glxinfo and test_gl in one program, so the system disk has one copy of
 * Mesa (24 MB) instead of two: which one runs depends on the name it's
 * started by (/usr/bin/glxinfo and test_gl are links to /usr/bin/glxgears). Built by
 * tools/ports/mesa.sh with each demo's main renamed. */
#include <string.h>

int glxgears_main(int argc, char **argv);
int glxinfo_main(int argc, char **argv);
int test_gl_main(void);

int main(int argc, char **argv)
{
    const char *name = strrchr(argv[0], '/');
    name = name ? name + 1 : argv[0];
    if (strcmp(name, "glxinfo") == 0)
        return glxinfo_main(argc, argv);
    if (strcmp(name, "test_gl") == 0)
        return test_gl_main();
    return glxgears_main(argc, argv);
}
