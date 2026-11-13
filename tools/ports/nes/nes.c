/* Copyright (c) 2026 tre4surehunter9
 * nes: a NES emulator for PalladiumOS: libretro's FCEUmm core (linked in)
 * with this small SDL2 front end: an X window (the picture scaled 2x),
 * sound through SDL (/dev/dsp), the keyboard as the first controller.
 *
 *   nes GAME.nes
 *
 * Keys: arrows, Z = B, X = A, Enter = Start, Right Shift or Tab = Select,
 * Escape quits. */
#include <SDL.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "libretro.h"

static SDL_Window *window;
static SDL_Renderer *renderer;
static SDL_Texture *texture;
static int tex_w, tex_h;
static enum retro_pixel_format pixfmt = RETRO_PIXEL_FORMAT_0RGB1555;
static SDL_AudioDeviceID audio;
static int running = 1;
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
        if (e.type == SDL_QUIT || (e.type == SDL_KEYDOWN && e.key.keysym.sym == SDLK_ESCAPE))
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

int main(int argc, char **argv)
{
    if (argc != 2) {
        fprintf(stderr, "usage: nes GAME.nes   (games: /usr/share/games/nes)\n");
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
    struct retro_game_info game = { .path = argv[1] };
    if (!retro_load_game(&game)) {
        fprintf(stderr, "nes: %s: can't load it\n", argv[1]);
        return 1;
    }
    struct retro_system_av_info av;
    retro_get_system_av_info(&av);
    const char *name = strrchr(argv[1], '/');
    char title[256];
    snprintf(title, sizeof title, "NES: %s", name ? name + 1 : argv[1]);
    window = SDL_CreateWindow(title, SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED,
                              av.geometry.base_width * 2, av.geometry.base_height * 2, 0);
    renderer = SDL_CreateRenderer(window, -1, SDL_RENDERER_SOFTWARE);
    if (!window || !renderer) {
        fprintf(stderr, "nes: %s\n", SDL_GetError());
        return 1;
    }
    SDL_AudioSpec want = { .freq = (int)av.timing.sample_rate, .format = AUDIO_S16SYS, .channels = 2, .samples = 1024 };
    audio = SDL_OpenAudioDevice(NULL, 0, &want, NULL, 0);
    if (audio) {
        SDL_PauseAudioDevice(audio, 0);
        fprintf(stderr, "nes: sound through %s at %d Hz\n", SDL_GetCurrentAudioDriver(), want.freq);
    } else {
        fprintf(stderr, "nes: no sound (%s)\n", SDL_GetError());
    }
    keys = SDL_GetKeyboardState(NULL);
    /* Frames at the game's rate; the sound queue kept short (0.1 s). */
    const double frame_ms = 1000.0 / (av.timing.fps > 0 ? av.timing.fps : 60.0);
    const Uint32 max_queued = (Uint32)(av.timing.sample_rate * 4 / 10);
    double next = SDL_GetTicks();
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
    retro_unload_game();
    retro_deinit();
    SDL_Quit();
    return 0;
}
