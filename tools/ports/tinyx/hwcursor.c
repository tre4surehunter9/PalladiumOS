/* Copyright (c) 2026 tre4surehunter9
 * PalladiumOS: the mouse pointer drawn by the graphics device (a virtio
 * GPU's hardware cursor), for Xfbdev. tools/ports/tinyx.sh copies this into
 * kdrive/fbdev and puts these functions in the card's table (initCursor...);
 * kdrive then uses them instead of drawing the pointer into the picture
 * itself (mi's software cursor), which it still does where the kernel says
 * no (no virtio GPU: the ioctls fail).
 *
 * The kernel's side: two /dev/fb0 ioctls of PalladiumOS's (src/vfs.rs):
 * an image (64x64 ARGB, premultiplied alpha, and its hot spot) and a
 * position (with visible or hidden). X gives a cursor as ARGB, or as the
 * classic two bitmaps (source and mask, one bit a pixel) and two colours,
 * turned into ARGB here. */
#include <sys/ioctl.h>
#include <string.h>
#include "mipointer.h"
#include "cursorstr.h"
#include "servermd.h"

#define PAL_FBIO_CURSOR_IMAGE 0x46F0
#define PAL_FBIO_CURSOR_MOVE 0x46F1
#define PAL_CURSOR 64

struct pal_cursor_image {
	unsigned int hot_x, hot_y;
	unsigned int argb[PAL_CURSOR * PAL_CURSOR];
};

struct pal_cursor_move {
	int x, y;
	unsigned int visible;
};

static int pal_fd(ScreenPtr pScreen)
{
	KdScreenPriv(pScreen);
	FbdevPriv *priv = pScreenPriv->card->driver;
	return priv->fd;
}

static int pal_shown; /* a cursor is set (not NULL) */

static void pal_move(ScreenPtr pScreen, int x, int y, int visible)
{
	struct pal_cursor_move m = { x, y, visible };
	ioctl(pal_fd(pScreen), PAL_FBIO_CURSOR_MOVE, &m);
}

/* A colour's 16-bit channels as 8-bit ARGB, opaque. */
static unsigned int pal_rgb(unsigned short r, unsigned short g, unsigned short b)
{
	return 0xff000000u | (unsigned)(r >> 8) << 16 | (unsigned)(g >> 8) << 8 | (b >> 8);
}

static void pal_image(ScreenPtr pScreen, CursorPtr pCurs)
{
	static struct pal_cursor_image img;
	CursorBitsPtr bits = pCurs->bits;
	int w = bits->width < PAL_CURSOR ? bits->width : PAL_CURSOR;
	int h = bits->height < PAL_CURSOR ? bits->height : PAL_CURSOR;
	memset(&img, 0, sizeof img);
	img.hot_x = bits->xhot < PAL_CURSOR ? bits->xhot : PAL_CURSOR - 1;
	img.hot_y = bits->yhot < PAL_CURSOR ? bits->yhot : PAL_CURSOR - 1;
	if (bits->argb) {
		for (int y = 0; y < h; y++)
			memcpy(&img.argb[y * PAL_CURSOR], &bits->argb[y * bits->width], w * 4);
	} else {
		unsigned int fore = pal_rgb(pCurs->foreRed, pCurs->foreGreen, pCurs->foreBlue);
		unsigned int back = pal_rgb(pCurs->backRed, pCurs->backGreen, pCurs->backBlue);
		int stride = BitmapBytePad(bits->width);
		for (int y = 0; y < h; y++) {
			for (int x = 0; x < w; x++) {
				int byte = y * stride + x / 8;
#if BITMAP_BIT_ORDER == LSBFirst
				int bit = 1 << (x % 8);
#else
				int bit = 0x80 >> (x % 8);
#endif
				if (bits->mask[byte] & bit)
					img.argb[y * PAL_CURSOR + x] = (bits->source[byte] & bit) ? fore : back;
			}
		}
	}
	ioctl(pal_fd(pScreen), PAL_FBIO_CURSOR_IMAGE, &img);
}

static Bool pal_realize(ScreenPtr pScreen, CursorPtr pCurs)
{
	return TRUE;
}

static Bool pal_unrealize(ScreenPtr pScreen, CursorPtr pCurs)
{
	return TRUE;
}

static void pal_set(ScreenPtr pScreen, CursorPtr pCurs, int x, int y)
{
	if (!pCurs) {
		pal_shown = 0;
		pal_move(pScreen, x, y, 0);
		return;
	}
	pal_image(pScreen, pCurs);
	pal_shown = 1;
	pal_move(pScreen, x, y, 1);
}

static void pal_moveto(ScreenPtr pScreen, int x, int y)
{
	pal_move(pScreen, x, y, pal_shown);
}

static miPointerSpriteFuncRec pal_sprite = {
	pal_realize, pal_unrealize, pal_set, pal_moveto,
};

Bool fbdevInitCursor(ScreenPtr pScreen)
{
	/* Does the kernel draw a pointer for this screen? (Hidden meanwhile.) */
	struct pal_cursor_move m = { 0, 0, 0 };
	if (ioctl(pal_fd(pScreen), PAL_FBIO_CURSOR_MOVE, &m) != 0)
		return FALSE; /* no: mi's software cursor */
	return miPointerInitialize(pScreen, &pal_sprite, &kdPointerScreenFuncs, FALSE);
}

void fbdevEnableCursor(ScreenPtr pScreen)
{
}

void fbdevDisableCursor(ScreenPtr pScreen)
{
	/* (Leaving: no pointer over the console.) */
	pal_move(pScreen, 0, 0, 0);
}

void fbdevFiniCursor(ScreenPtr pScreen)
{
	pal_move(pScreen, 0, 0, 0);
}
