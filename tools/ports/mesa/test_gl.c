/* Copyright (c) 2026 tre4surehunter9
 * test_gl: Mesa drawing a texture (the usermode tests run it under X). Built
 * into /usr/bin/glxgears by tools/ports/mesa.sh (main renamed test_gl_main):
 * one copy of Mesa on the disk.
 *
 * A 2x2 checkerboard texture over the whole window (nearest filtering), read
 * back with glReadPixels: each quarter must be its texel. Exits 0 if so. */
#include <GL/gl.h>
#include <GL/glx.h>
#include <X11/Xlib.h>
#include <stdio.h>
int main(void)
{
    Display *d = XOpenDisplay(NULL);
    if (!d) { puts("test_gl: no display"); return 1; }
    int attr[] = { GLX_RGBA, GLX_RED_SIZE, 8, GLX_GREEN_SIZE, 8, GLX_BLUE_SIZE, 8, GLX_DOUBLEBUFFER, None };
    XVisualInfo *vi = glXChooseVisual(d, DefaultScreen(d), attr);
    if (!vi) { puts("test_gl: no visual"); return 1; }
    XSetWindowAttributes swa = { .colormap = XCreateColormap(d, RootWindow(d, vi->screen), vi->visual, AllocNone) };
    Window w = XCreateWindow(d, RootWindow(d, vi->screen), 0, 0, 64, 64, 0, vi->depth, InputOutput, vi->visual, CWColormap, &swa);
    XMapWindow(d, w);
    GLXContext c = glXCreateContext(d, vi, NULL, True);
    glXMakeCurrent(d, w, c);
    unsigned char tex[2][2][3] = { { {255, 0, 0}, {0, 255, 0} }, { {0, 0, 255}, {255, 255, 0} } };
    GLuint t;
    glGenTextures(1, &t);
    glBindTexture(GL_TEXTURE_2D, t);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGB, 2, 2, 0, GL_RGB, GL_UNSIGNED_BYTE, tex);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glEnable(GL_TEXTURE_2D);
    glViewport(0, 0, 64, 64);
    glClearColor(0, 0, 0, 1);
    glClear(GL_COLOR_BUFFER_BIT);
    glBegin(GL_QUADS);
    glTexCoord2f(0, 0); glVertex2f(-1, -1);
    glTexCoord2f(1, 0); glVertex2f(1, -1);
    glTexCoord2f(1, 1); glVertex2f(1, 1);
    glTexCoord2f(0, 1); glVertex2f(-1, 1);
    glEnd();
    glFinish();
    /* (x, y) from the bottom left: row 0 of the texture is the bottom. */
    int pts[4][2] = { {16, 16}, {48, 16}, {16, 48}, {48, 48} };
    int bad = 0;
    for (int i = 0; i < 4; i++) {
        unsigned char px[4];
        glReadPixels(pts[i][0], pts[i][1], 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, px);
        const unsigned char *want = tex[i / 2][i % 2];
        printf("test_gl: (%d,%d) = %d %d %d, want %d %d %d\n", pts[i][0], pts[i][1], px[0], px[1], px[2], want[0], want[1], want[2]);
        bad += px[0] != want[0] || px[1] != want[1] || px[2] != want[2];
    }
    glXSwapBuffers(d, w);
    puts(bad ? "test_gl: WRONG" : "test_gl: ok");
    return bad != 0;
}
