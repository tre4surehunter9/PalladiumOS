/* Copyright (c) 2026 tre4surehunter9
 * test_fb: checks /dev/fb0, the console's graphics mode, virtual-terminal
 * and keyboard ioctls, and /dev/input/mice. Exits 0, or the line number of
 * the first failed check. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/fb.h>
#include <linux/kd.h>
#include <linux/keyboard.h>
#include <linux/vt.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(c) do { if (!(c)) { fprintf(stderr, "test_fb: check failed at line %d: %s (errno %d)\n", __LINE__, #c, errno); return __LINE__; } } while (0)

static int framebuffer(void)
{
    int fb = open("/dev/fb0", O_RDWR);
    CHECK(fb >= 0);
    struct fb_var_screeninfo var;
    struct fb_fix_screeninfo fix;
    CHECK(ioctl(fb, FBIOGET_VSCREENINFO, &var) == 0 && ioctl(fb, FBIOGET_FSCREENINFO, &fix) == 0);
    CHECK(var.xres >= 640 && var.yres >= 400 && (var.bits_per_pixel == 24 || var.bits_per_pixel == 32));
    CHECK(fix.line_length >= var.xres * var.bits_per_pixel / 8 && fix.smem_len >= fix.line_length * var.yres);
    CHECK(fix.visual == FB_VISUAL_TRUECOLOR && var.red.length == 8 && var.green.offset == 8);
    CHECK(ioctl(fb, FBIOPUT_VSCREENINFO, &var) == 0);
    /* Map it, draw a line of pixels, see it through read(). */
    uint8_t *p = mmap(NULL, fix.smem_len, PROT_READ | PROT_WRITE, MAP_SHARED, fb, 0);
    CHECK(p != MAP_FAILED);
    size_t row = 10 * fix.line_length;
    memset(p + row, 0x5a, 64);
    uint8_t back[64];
    CHECK(pread(fb, back, 64, row) == 64 && back[0] == 0x5a && back[63] == 0x5a);
    CHECK(munmap(p, fix.smem_len) == 0);
    CHECK(mmap(NULL, fix.smem_len + 65536, PROT_READ, MAP_SHARED, fb, 0) == MAP_FAILED); /* too big */
    close(fb);
    return 0;
}

static int console(void)
{
    int t0 = open("/dev/tty0", O_WRONLY);
    CHECK(t0 >= 0);
    int vt = -1;
    CHECK(ioctl(t0, VT_OPENQRY, &vt) == 0 && vt == 1);
    close(t0);
    int c = open("/dev/tty1", O_RDWR | O_NDELAY);
    CHECK(c >= 0);
    struct vt_stat st;
    CHECK(ioctl(c, VT_GETSTATE, &st) == 0 && st.v_active == 1);
    struct vt_mode vm;
    CHECK(ioctl(c, VT_GETMODE, &vm) == 0);
    vm.mode = VT_PROCESS;
    CHECK(ioctl(c, VT_SETMODE, &vm) == 0 && ioctl(c, VT_ACTIVATE, 1) == 0 && ioctl(c, VT_WAITACTIVE, 1) == 0);
    /* Graphics mode and back. */
    int mode;
    CHECK(ioctl(c, KDSETMODE, KD_GRAPHICS) == 0 && ioctl(c, KDGETMODE, &mode) == 0 && mode == KD_GRAPHICS);
    CHECK(ioctl(c, KDSETMODE, KD_TEXT) == 0 && ioctl(c, KDGETMODE, &mode) == 0 && mode == KD_TEXT);
    /* The key map: 'a' (keycode 30), shifted 'A', Up (103), F1 (59). */
    struct kbentry e = { 0, 30, 0 };
    CHECK(ioctl(c, KDGKBENT, &e) == 0 && KTYP(e.kb_value) == KT_LETTER && KVAL(e.kb_value) == 'a');
    e.kb_table = 1;
    CHECK(ioctl(c, KDGKBENT, &e) == 0 && KVAL(e.kb_value) == 'A');
    e.kb_table = 0; e.kb_index = 103;
    CHECK(ioctl(c, KDGKBENT, &e) == 0 && e.kb_value == K_UP);
    e.kb_index = 59;
    CHECK(ioctl(c, KDGKBENT, &e) == 0 && e.kb_value == K_F1);
    /* Keyboard modes. */
    int kb;
    CHECK(ioctl(c, KDGKBMODE, &kb) == 0 && kb == K_XLATE);
    CHECK(ioctl(c, KDSKBMODE, K_MEDIUMRAW) == 0 && ioctl(c, KDGKBMODE, &kb) == 0 && kb == K_MEDIUMRAW);
    CHECK(ioctl(c, KDSKBMODE, K_XLATE) == 0);
    CHECK(ioctl(c, KDSETLED, 0) == 0);
    close(c);
    return 0;
}

static int mouse(void)
{
    int m = open("/dev/input/mice", O_RDWR | O_NONBLOCK);
    CHECK(m >= 0);
    unsigned char buf[8];
    while (read(m, buf, sizeof buf) > 0) {} /* whatever moved meanwhile */
    unsigned char getid = 0xF2;
    CHECK(write(m, &getid, 1) == 1 && read(m, buf, 2) == 2 && buf[0] == 0xFA && buf[1] == 0x00);
    /* The IntelliMouse sequence: then the ID is 3. */
    unsigned char seq[] = { 0xF3, 200, 0xF3, 100, 0xF3, 80 };
    CHECK(write(m, seq, sizeof seq) == sizeof seq && read(m, buf, sizeof buf) == 6);
    CHECK(write(m, &getid, 1) == 1 && read(m, buf, 2) == 2 && buf[1] == 0x03);
    unsigned char reset = 0xFF;
    CHECK(write(m, &reset, 1) == 1 && read(m, buf, 3) == 3 && buf[1] == 0xAA);
    CHECK(read(m, buf, 1) == -1 && errno == EAGAIN);
    close(m);
    return 0;
}

int main(void)
{
    int r;
    if ((r = framebuffer()) || (r = console()) || (r = mouse()))
        return r;
    printf("test_fb: all checks passed\n");
    return 0;
}
