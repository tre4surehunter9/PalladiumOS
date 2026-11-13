/* Copyright (c) 2026 tre4surehunter9
 * nes: a NES emulator for PalladiumOS: libretro's FCEUmm core (linked in)
 * with this small SDL2 front end: an X window (the picture scaled 2x),
 * sound through SDL (/dev/dsp), the keyboard as the first controller.
 *
 *   nes             a menu of the games (/usr/share/games/nes, and .nes
 *                   files in your home folder or ~/nes): arrows and Enter
 *   nes GAME.nes    that game
 *
 * Keys: arrows, Z = B, X = A, Enter = Start, Right Shift or Tab = Select,
 * Escape: back to the menu (or quit). The menu's letters: misc-fixed 8x13
 * (public domain), made into font.h by bdf2c.py when it's built. */
#include <SDL.h>
#include <dirent.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "libretro.h"
#include "font.h"

static SDL_Window *window;
static SDL_Renderer *renderer;
static SDL_Texture *texture;
static int tex_w, tex_h;
static enum retro_pixel_format pixfmt = RETRO_PIXEL_FORMAT_0RGB1555;
static SDL_AudioDeviceID audio;
static int running = 1;   /* this game (0: back to the menu) */
static int quitting = 0;  /* the whole program (the window closed) */
static const Uint8 *keys;
/* Keys pressed since the last frame, even if already let go (a quick tap
 * between two frames still counts). */
static Uint8 tapped[SDL_NUM_SCANCODES];
static char save_dir[512] = "/tmp";

static void log_cb(enum retro_log_level level, const char *fmt, ...)
{
    if (level < RETRO_LOG_WARN)
        return;
    va_list ap;
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
}

static bool environment(unsigned cmd, void *data)
{
    switch (cmd) {
    case RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: {
        enum retro_pixel_format f = *(const enum retro_pixel_format *)data;
        if (f != RETRO_PIXEL_FORMAT_XRGB8888 && f != RETRO_PIXEL_FORMAT_RGB565 && f != RETRO_PIXEL_FORMAT_0RGB1555)
            return false;
        pixfmt = f;
        return true;
    }
    case RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY:
    case RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY:
        *(const char **)data = save_dir;
        return true;
    case RETRO_ENVIRONMENT_GET_CAN_DUPE:
        *(bool *)data = true;
        return true;
    case RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE:
        *(bool *)data = false;
        return true;
    case RETRO_ENVIRONMENT_GET_LOG_INTERFACE:
        ((struct retro_log_callback *)data)->log = log_cb;
        return true;
    default:
        return false; /* (the core's defaults) */
    }
}

static void video_refresh(const void *data, unsigned w, unsigned h, size_t pitch)
{
    if (!data)
        return; /* the same frame again */
    if (!texture || tex_w != (int)w || tex_h != (int)h) {
        if (texture)
            SDL_DestroyTexture(texture);
        Uint32 f = pixfmt == RETRO_PIXEL_FORMAT_XRGB8888 ? SDL_PIXELFORMAT_ARGB8888
                 : pixfmt == RETRO_PIXEL_FORMAT_RGB565 ? SDL_PIXELFORMAT_RGB565 : SDL_PIXELFORMAT_RGB555;
        texture = SDL_CreateTexture(renderer, f, SDL_TEXTUREACCESS_STREAMING, w, h);
        tex_w = w;
        tex_h = h;
    }
    SDL_UpdateTexture(texture, NULL, data, (int)pitch);
    SDL_RenderClear(renderer);
    SDL_RenderCopy(renderer, texture, NULL, NULL);
    SDL_RenderPresent(renderer);
}

static void audio_sample(int16_t l, int16_t r)
{
    int16_t s[2] = { l, r };
    if (audio)
        SDL_QueueAudio(audio, s, sizeof s);
}

static size_t audio_batch(const int16_t *data, size_t frames)
{
    if (audio)
        SDL_QueueAudio(audio, data, frames * 4);
    return frames;
}

static void input_poll(void)
{
    SDL_Event e;
    memset(tapped, 0, sizeof tapped);
    while (SDL_PollEvent(&e)) {
        if (e.type == SDL_QUIT)
            running = 0, quitting = 1;
        if (e.type == SDL_KEYDOWN && e.key.keysym.sym == SDLK_ESCAPE)
            running = 0;
        if (e.type == SDL_KEYDOWN && e.key.keysym.scancode < SDL_NUM_SCANCODES)
            tapped[e.key.keysym.scancode] = 1;
    }
}

static int down(SDL_Scancode sc)
{
    return keys[sc] || tapped[sc];
}

static int16_t input_state(unsigned port, unsigned device, unsigned index, unsigned id)
{
    (void)index;
    if (port != 0 || device != RETRO_DEVICE_JOYPAD)
        return 0;
    switch (id) {
    case RETRO_DEVICE_ID_JOYPAD_UP: return down(SDL_SCANCODE_UP);
    case RETRO_DEVICE_ID_JOYPAD_DOWN: return down(SDL_SCANCODE_DOWN);
    case RETRO_DEVICE_ID_JOYPAD_LEFT: return down(SDL_SCANCODE_LEFT);
    case RETRO_DEVICE_ID_JOYPAD_RIGHT: return down(SDL_SCANCODE_RIGHT);
    case RETRO_DEVICE_ID_JOYPAD_A: return down(SDL_SCANCODE_X);
    case RETRO_DEVICE_ID_JOYPAD_B: return down(SDL_SCANCODE_Z);
    case RETRO_DEVICE_ID_JOYPAD_START: return down(SDL_SCANCODE_RETURN);
    case RETRO_DEVICE_ID_JOYPAD_SELECT: return down(SDL_SCANCODE_RSHIFT) || down(SDL_SCANCODE_TAB);
    default: return 0;
    }
}

/* ---- the menu ---- */

#define MW 256
#define MH 240
static Uint32 menu_px[MW * MH];

static void fill(int x, int y, int w, int h, Uint32 c)
{
    for (int j = y; j < y + h && j < MH; j++)
        for (int i = x; i < x + w && i < MW; i++)
            if (i >= 0 && j >= 0)
                menu_px[j * MW + i] = c;
}

static void text(int x, int y, const char *s, Uint32 c)
{
    for (; *s && x < MW; s++, x += 8) {
        unsigned ch = (unsigned char)*s;
        if (ch < 32 || ch > 126)
            ch = '?';
        for (int r = 0; r < 13; r++)
            for (int b = 0; b < 8; b++)
                if (font[ch - 32][r] & (0x80 >> b) && x + b < MW && y + r < MH)
                    menu_px[(y + r) * MW + x + b] = c;
    }
}

struct game {
    char path[512];
    char title[33];
};

static struct game games[200];
static int ngames;

/* The bundled games' titles; others: their file name. */
static void title_of(const char *file, char *out)
{
    static const char *known[][2] = {
        { "nova.nes", "Nova the Squirrel" },
        { "thwaite.nes", "Thwaite" },
        { "croom.nes", "Concentration Room" },
    };
    for (unsigned i = 0; i < sizeof known / sizeof known[0]; i++)
        if (strcmp(file, known[i][0]) == 0) {
            snprintf(out, 33, "%s", known[i][1]);
            return;
        }
    snprintf(out, 33, "%s", file);
    char *dot = strrchr(out, '.');
    if (dot && strcasecmp(dot, ".nes") == 0)
        *dot = 0;
}

static void find_games(const char *dir)
{
    DIR *d = opendir(dir);
    if (!d)
        return;
    struct dirent *e;
    while ((e = readdir(d)) && ngames < 200) {
        size_t n = strlen(e->d_name);
        if (n < 5 || strcasecmp(e->d_name + n - 4, ".nes") != 0)
            continue;
        struct game *g = &games[ngames++];
        snprintf(g->path, sizeof g->path, "%s/%s", dir, e->d_name);
        title_of(e->d_name, g->title);
    }
    closedir(d);
}

static int by_title(const void *a, const void *b)
{
    return strcasecmp(((const struct game *)a)->title, ((const struct game *)b)->title);
}

/* The menu until a game is chosen (its index) or the window closes (-1). */
static int menu(SDL_Texture *t, int sel)
{
    const int rows = 12; /* games shown at once */
    for (;;) {
        SDL_Event e;
        while (SDL_PollEvent(&e)) {
            if (e.type == SDL_QUIT)
                return -1;
            if (e.type != SDL_KEYDOWN)
                continue;
            switch (e.key.keysym.sym) {
            case SDLK_UP: sel = sel > 0 ? sel - 1 : ngames - 1; break;
            case SDLK_DOWN: sel = sel < ngames - 1 ? sel + 1 : 0; break;
            case SDLK_PAGEUP: sel = sel > rows ? sel - rows : 0; break;
            case SDLK_PAGEDOWN: sel = sel + rows < ngames ? sel + rows : ngames - 1; break;
            case SDLK_RETURN: case SDLK_x: case SDLK_SPACE:
                if (ngames > 0)
                    return sel;
                break;
            case SDLK_ESCAPE: return -1;
            default: break;
            }
        }
        /* Draw it: a title bar, the list (the chosen one highlighted), the keys. */
        fill(0, 0, MW, MH, 0xff101830);
        fill(0, 0, MW, 22, 0xff2848a0);
        text(8, 5, "PalladiumOS NES", 0xffffffff);
        text(MW - 8 - 8 * 8, 5, ngames == 1 ? "  1 game" : "", 0xffc0d0ff);
        if (ngames != 1) {
            char n[16];
            snprintf(n, sizeof n, "%3d games", ngames);
            text(MW - 8 - 9 * 8, 5, n, 0xffc0d0ff);
        }
        if (ngames == 0) {
            text(16, 60, "No games found.", 0xffffffff);
            text(16, 80, "Put .nes files in your", 0xffc0c0c0);
            text(16, 94, "home folder or ~/nes.", 0xffc0c0c0);
        }
        int first = sel >= rows ? sel - rows + 1 : 0;
        for (int i = first; i < ngames && i < first + rows; i++) {
            int y = 32 + (i - first) * 15;
            if (i == sel) {
                fill(6, y - 1, MW - 12, 15, 0xffe8c040);
                text(12, y, ">", 0xff101830);
            }
            text(24, y, games[i].title, i == sel ? 0xff101830 : 0xffffffff);
        }
        fill(0, MH - 34, MW, 34, 0xff0a1020);
        text(8, MH - 30, "Arrows: choose  Enter: play", 0xffc0d0ff);
        text(8, MH - 16, "Esc: quit (in a game: menu)", 0xffc0d0ff);
        SDL_UpdateTexture(t, NULL, menu_px, MW * 4);
        SDL_RenderClear(renderer);
        SDL_RenderCopy(renderer, t, NULL, NULL);
        SDL_RenderPresent(renderer);
        SDL_Delay(30);
    }
}

/* ---- playing ---- */

/* Play one game until Escape (or the window closes). 0 if it can't load. */
static int play(const char *path, const struct retro_system_av_info *first_av)
{
    struct retro_game_info game = { .path = path };
    if (!retro_load_game(&game)) {
        fprintf(stderr, "nes: %s: can't load it\n", path);
        return 0;
    }
    struct retro_system_av_info av;
    retro_get_system_av_info(&av);
    (void)first_av;
    const char *name = strrchr(path, '/');
    char title[256];
    snprintf(title, sizeof title, "NES: %s", name ? name + 1 : path);
    SDL_SetWindowTitle(window, title);
    SDL_AudioSpec want = { .freq = (int)av.timing.sample_rate, .format = AUDIO_S16SYS, .channels = 2, .samples = 1024 };
    audio = SDL_OpenAudioDevice(NULL, 0, &want, NULL, 0);
    if (audio)
        SDL_PauseAudioDevice(audio, 0);
    else
        fprintf(stderr, "nes: no sound (%s)\n", SDL_GetError());
    /* Frames at the game's rate; the sound queue kept short (0.1 s). */
    const double frame_ms = 1000.0 / (av.timing.fps > 0 ? av.timing.fps : 60.0);
    const Uint32 max_queued = (Uint32)(av.timing.sample_rate * 4 / 10);
    double next = SDL_GetTicks();
    running = 1;
    while (running) {
        retro_run();
        next += frame_ms;
        double now = SDL_GetTicks();
        if (next > now)
            SDL_Delay((Uint32)(next - now));
        else if (now - next > 200)
            next = now; /* (fell behind: don't race to catch up) */
        while (audio && SDL_GetQueuedAudioSize(audio) > max_queued)
            SDL_Delay(2);
    }
    if (audio) {
        SDL_CloseAudioDevice(audio);
        audio = 0;
    }
    retro_unload_game();
    if (texture) {
        SDL_DestroyTexture(texture);
        texture = NULL;
    }
    return 1;
}

int main(int argc, char **argv)
{
    if (argc > 2 || (argc == 2 && argv[1][0] == '-')) {
        fprintf(stderr, "usage: nes [GAME.nes]   (without a game: a menu of them)\n");
        return 1;
    }
    const char *home = getenv("HOME");
    if (home)
        snprintf(save_dir, sizeof save_dir, "%s", home);
    if (SDL_Init(SDL_INIT_VIDEO | SDL_INIT_AUDIO) != 0) {
        fprintf(stderr, "nes: %s\n", SDL_GetError());
        return 1;
    }
    retro_set_environment(environment);
    retro_set_video_refresh(video_refresh);
    retro_set_audio_sample(audio_sample);
    retro_set_audio_sample_batch(audio_batch);
    retro_set_input_poll(input_poll);
    retro_set_input_state(input_state);
    retro_init();
    window = SDL_CreateWindow("NES", SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED, MW * 2, MH * 2, 0);
    renderer = SDL_CreateRenderer(window, -1, SDL_RENDERER_SOFTWARE);
    if (!window || !renderer) {
        fprintf(stderr, "nes: %s\n", SDL_GetError());
        return 1;
    }
    keys = SDL_GetKeyboardState(NULL);
    if (argc == 2) {
        int ok = play(argv[1], NULL);
        retro_deinit();
        SDL_Quit();
        return ok ? 0 : 1;
    }
    /* The menu: the bundled games, then the player's own. */
    find_games("/usr/share/games/nes");
    if (home) {
        char dir[512];
        find_games(home);
        snprintf(dir, sizeof dir, "%s/nes", home);
        find_games(dir);
    }
    qsort(games, ngames, sizeof games[0], by_title);
    SDL_Texture *mt = SDL_CreateTexture(renderer, SDL_PIXELFORMAT_ARGB8888, SDL_TEXTUREACCESS_STREAMING, MW, MH);
    int sel = 0;
    while (!quitting) {
        SDL_SetWindowTitle(window, "NES");
        int chosen = menu(mt, sel);
        if (chosen < 0)
            break;
        sel = chosen;
        play(games[chosen].path, NULL);
    }
    retro_deinit();
    SDL_Quit();
    return 0;
}
