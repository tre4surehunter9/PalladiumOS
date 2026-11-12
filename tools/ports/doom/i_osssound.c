/* Copyright (c) 2026 tre4surehunter9
 * Sound for doomgeneric through OSS (/dev/dsp), for PalladiumOS.
 *
 * Sound effects: DOOM's DMX sound lumps (8-bit, 11025 or 22050 Hz), mixed
 * on 16 channels with their volume and stereo separation.
 *
 * Music: a small synthesizer playing the game's MIDI music (Freedoom's;
 * DOOM's own MUS music is converted to MIDI with mus2mid): Standard MIDI
 * Files of format 0 or 1, with tempo changes, programs, volume, pan,
 * expression and pitch bend. Each General MIDI family gets a waveform
 * (saw for guitars and strings, square for leads and brass, triangle for
 * bass and organs, sine for pads) and an envelope; channel 10's drums are
 * made of noise and falling sine tones. It sounds like an old sound card,
 * not like an orchestra.
 *
 * Both are mixed into 16-bit stereo at 22050 Hz and written ahead by about
 * 100 ms each time the game updates its sound (every tic). */

#include <fcntl.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/soundcard.h>
#include <unistd.h>

#include "deh_str.h"
#include "doomtype.h"
#include "i_sound.h"
#include "memio.h"
#include "mus2mid.h"
#include "w_wad.h"
#include "z_zone.h"

#define RATE 22050
#define NUM_CHANNELS 16
/* How far ahead to write (ms). */
#define AHEAD_MS 100
/* Frames mixed at most per update. */
#define MAX_FRAMES 4096

/* (Settings i_sound.c binds; there is no libsamplerate here.) */
int use_libsamplerate = 0;
float libsamplerate_scale = 0.65f;

void I_InitTimidityConfig(void)
{
}

static int dsp = -1;
static boolean use_sfx_prefix;

/* ---- sound effects ---- */

typedef struct {
    const uint8_t *data; /* unsigned 8-bit samples */
    uint32_t len;
    uint32_t rate;
} sample_t;

typedef struct {
    const sample_t *s;
    uint32_t pos;  /* 16.16 fixed point, in samples */
    uint32_t step; /* 16.16: the sample's rate / RATE */
    int left, right; /* 0-255 */
    sfxinfo_t *sfx;
} channel_t;

static channel_t channels[NUM_CHANNELS];

static snddevice_t sound_devices[] = {
    SNDDEVICE_SB, SNDDEVICE_PAS, SNDDEVICE_GUS, SNDDEVICE_WAVEBLASTER, SNDDEVICE_SOUNDCANVAS, SNDDEVICE_AWE32,
};

static void sfx_lump_name(sfxinfo_t *sfx, char *buf, size_t len)
{
    if (sfx->link != NULL)
        sfx = sfx->link;
    if (use_sfx_prefix)
        snprintf(buf, len, "ds%s", DEH_String(sfx->name));
    else
        snprintf(buf, len, "%s", DEH_String(sfx->name));
}

static int OSS_GetSfxLumpNum(sfxinfo_t *sfx)
{
    char name[16];
    sfx_lump_name(sfx, name, sizeof name);
    return W_CheckNumForName(name);
}

/* A DMX sound lump: format 3, the rate, the length, then the samples with
 * 16 bytes of padding at each end. */
static const sample_t *load_sample(sfxinfo_t *sfx)
{
    if (sfx->link != NULL)
        sfx = sfx->link;
    if (sfx->driver_data != NULL)
        return sfx->driver_data;
    if (sfx->lumpnum < 0)
        sfx->lumpnum = OSS_GetSfxLumpNum(sfx);
    if (sfx->lumpnum < 0)
        return NULL;
    int size = W_LumpLength(sfx->lumpnum);
    const uint8_t *d = W_CacheLumpNum(sfx->lumpnum, PU_STATIC);
    if (size < 8 || d[0] != 3 || d[1] != 0)
        return NULL;
    uint32_t rate = d[2] | d[3] << 8;
    uint32_t len = d[4] | d[5] << 8 | d[6] << 16 | (uint32_t)d[7] << 24;
    if (len > (uint32_t)size - 8 || len <= 48 || rate < 4000)
        return NULL;
    sample_t *s = malloc(sizeof *s);
    s->data = d + 8 + 16;
    s->len = len - 32;
    s->rate = rate;
    sfx->driver_data = s;
    return s;
}

static void set_params(channel_t *c, int vol, int sep)
{
    c->left = ((254 - sep) * vol) / 127;
    c->right = (sep * vol) / 127;
}

static int OSS_StartSound(sfxinfo_t *sfx, int channel, int vol, int sep)
{
    if (channel < 0 || channel >= NUM_CHANNELS)
        return -1;
    const sample_t *s = load_sample(sfx);
    if (s == NULL)
        return -1;
    channel_t *c = &channels[channel];
    c->s = s;
    c->pos = 0;
    c->step = (uint32_t)(((uint64_t)s->rate << 16) / RATE);
    c->sfx = sfx;
    set_params(c, vol, sep);
    return channel;
}

static void OSS_StopSound(int channel)
{
    if (channel >= 0 && channel < NUM_CHANNELS)
        channels[channel].s = NULL;
}

static boolean OSS_SoundIsPlaying(int channel)
{
    return channel >= 0 && channel < NUM_CHANNELS && channels[channel].s != NULL;
}

static void OSS_UpdateSoundParams(int channel, int vol, int sep)
{
    if (channel >= 0 && channel < NUM_CHANNELS)
        set_params(&channels[channel], vol, sep);
}

static void OSS_CacheSounds(sfxinfo_t *sounds, int num)
{
    (void)sounds;
    (void)num;
}

/* ---- the synthesizer ---- */

#define VOICES 32

enum wave { SINE, TRIANGLE, SQUARE, SAW, NOISE, KICK, TOM };

typedef struct {
    int active;
    int chan, note;
    enum wave wave;
    float phase, inc;      /* cycles, cycles per frame */
    float sweep;           /* KICK/TOM: the pitch falls by this per frame */
    float amp;             /* velocity and channel volume */
    float level;           /* the envelope */
    float attack, decay, sustain, release; /* per-frame steps, and the sustain level */
    int releasing;
    uint32_t noise;
    float lp;              /* NOISE: a little filtering */
    float hp;
    unsigned age;
} voice_t;

typedef struct {
    int program;
    float volume, expression, pan; /* 0-1 */
    float bend;                    /* semitones */
} midichan_t;

typedef struct {
    const uint8_t *p, *end;
    uint64_t next; /* the next event's tick */
    uint8_t status;
    int done;
} track_t;

typedef struct {
    uint8_t *data;
    int len;
    int division;
    int ntracks;
} song_t;

static voice_t voices[VOICES];
static midichan_t mchan[16];
static track_t tracks[64];
static song_t *song;
static boolean music_playing, music_paused, music_looping;
static float music_volume = 100.0f / 127.0f;
static double tick_pos;          /* the song's position, in ticks */
static double ticks_per_frame;
static unsigned voice_age;

static uint32_t be32(const uint8_t *p)
{
    return (uint32_t)p[0] << 24 | p[1] << 16 | p[2] << 8 | p[3];
}

static uint32_t varlen(track_t *t)
{
    uint32_t v = 0;
    for (int i = 0; i < 4 && t->p < t->end; i++) {
        uint8_t b = *t->p++;
        v = v << 7 | (b & 0x7f);
        if (!(b & 0x80))
            break;
    }
    return v;
}

static void set_tempo(uint32_t us_per_quarter)
{
    if (us_per_quarter == 0)
        us_per_quarter = 500000;
    ticks_per_frame = (double)song->division * 1e6 / us_per_quarter / RATE;
}

static void reset_channels(void)
{
    for (int i = 0; i < 16; i++) {
        mchan[i].program = 0;
        mchan[i].volume = 100.0f / 127.0f;
        mchan[i].expression = 1.0f;
        mchan[i].pan = 0.5f;
        mchan[i].bend = 0;
    }
}

static void all_voices_off(void)
{
    memset(voices, 0, sizeof voices);
}

/* Start the song from its beginning. */
static void rewind_song(void)
{
    const uint8_t *p = song->data + 14;
    const uint8_t *end = song->data + song->len;
    int n = 0;
    while (n < song->ntracks && n < 64 && p + 8 <= end) {
        uint32_t len = be32(p + 4);
        if (memcmp(p, "MTrk", 4) == 0) {
            track_t *t = &tracks[n++];
            t->p = p + 8;
            t->end = (p + 8 + len <= end) ? p + 8 + len : end;
            t->status = 0;
            t->done = 0;
            t->next = varlen(t);
        }
        p += 8 + len;
    }
    for (int i = n; i < 64; i++)
        tracks[i].done = 1;
    tick_pos = 0;
    set_tempo(500000);
    reset_channels();
}

static float note_freq(int note, float bend)
{
    return 440.0f * powf(2.0f, (note - 69 + bend) / 12.0f);
}

static voice_t *new_voice(void)
{
    voice_t *best = &voices[0];
    for (int i = 0; i < VOICES; i++) {
        voice_t *v = &voices[i];
        if (!v->active)
            return v;
        /* Else the quietest releasing one, or the oldest. */
        if (v->releasing && (!best->releasing || v->level < best->level))
            best = v;
        else if (!best->releasing && v->age < best->age)
            best = v;
    }
    return best;
}

/* An envelope in seconds: attack, decay (to `sustain`), release. */
static void envelope(voice_t *v, float a, float d, float s, float r)
{
    v->attack = 1.0f / (a * RATE);
    v->decay = (1.0f - s) / (d * RATE);
    v->sustain = s;
    v->release = 1.0f / (r * RATE);
}

static void note_on(int ch, int note, int vel)
{
    midichan_t *c = &mchan[ch];
    voice_t *v = new_voice();
    memset(v, 0, sizeof *v);
    v->active = 1;
    v->chan = ch;
    v->note = note;
    v->age = ++voice_age;
    v->amp = vel / 127.0f;
    v->noise = 0x12345 + note * 977;
    if (ch == 9) {
        /* General MIDI drums */
        switch (note) {
        case 35: case 36: /* bass drums */
            v->wave = KICK; v->inc = 140.0f / RATE; v->sweep = 0.99985f;
            envelope(v, 0.001f, 0.18f, 0.0f, 0.05f);
            v->amp *= 1.4f;
            break;
        case 41: case 43: case 45: case 47: case 48: case 50: /* toms */
            v->wave = TOM; v->inc = note_freq(note - 12, 0) / RATE; v->sweep = 0.99993f;
            envelope(v, 0.001f, 0.25f, 0.0f, 0.05f);
            break;
        case 42: case 44: /* closed hi-hats */
            v->wave = NOISE; v->hp = 1;
            envelope(v, 0.001f, 0.05f, 0.0f, 0.02f);
            v->amp *= 0.5f;
            break;
        case 46: /* open hi-hat */
            v->wave = NOISE; v->hp = 1;
            envelope(v, 0.001f, 0.3f, 0.0f, 0.05f);
            v->amp *= 0.45f;
            break;
        case 49: case 52: case 55: case 57: /* crashes */
            v->wave = NOISE; v->hp = 1;
            envelope(v, 0.002f, 0.9f, 0.0f, 0.2f);
            v->amp *= 0.5f;
            break;
        case 51: case 53: case 59: /* rides */
            v->wave = NOISE; v->hp = 1;
            envelope(v, 0.001f, 0.4f, 0.0f, 0.1f);
            v->amp *= 0.3f;
            break;
        case 38: case 40: default: /* snares and the rest */
            v->wave = NOISE;
            envelope(v, 0.001f, 0.16f, 0.0f, 0.05f);
            v->amp *= 0.8f;
            break;
        }
        return;
    }
    v->inc = note_freq(note, c->bend) / RATE;
    int family = c->program / 8;
    switch (family) {
    case 0: /* pianos */
        v->wave = TRIANGLE; envelope(v, 0.003f, 0.8f, 0.3f, 0.2f); break;
    case 1: /* chromatic percussion */
        v->wave = SINE; envelope(v, 0.002f, 0.6f, 0.0f, 0.2f); break;
    case 2: /* organs */
        v->wave = TRIANGLE; envelope(v, 0.01f, 0.05f, 0.9f, 0.1f); break;
    case 3: /* guitars (distortion guitar: DOOM's sound) */
        v->wave = c->program >= 29 ? SQUARE : SAW;
        envelope(v, 0.003f, 1.2f, 0.5f, 0.12f);
        if (c->program >= 29)
            v->amp *= 0.6f;
        break;
    case 4: /* basses */
        v->wave = TRIANGLE; envelope(v, 0.003f, 0.4f, 0.7f, 0.08f); v->amp *= 1.3f; break;
    case 5: case 6: /* strings, ensembles */
        v->wave = SAW; envelope(v, 0.06f, 0.2f, 0.8f, 0.3f); v->amp *= 0.6f; break;
    case 7: case 8: /* brass, reeds */
        v->wave = SQUARE; envelope(v, 0.02f, 0.2f, 0.75f, 0.12f); v->amp *= 0.5f; break;
    case 9: /* pipes */
        v->wave = SINE; envelope(v, 0.03f, 0.1f, 0.85f, 0.15f); break;
    case 10: /* synth leads */
        v->wave = SQUARE; envelope(v, 0.005f, 0.1f, 0.8f, 0.1f); v->amp *= 0.5f; break;
    case 11: /* synth pads */
        v->wave = SINE; envelope(v, 0.2f, 0.3f, 0.8f, 0.5f); break;
    default:
        v->wave = SAW; envelope(v, 0.01f, 0.3f, 0.6f, 0.2f); v->amp *= 0.6f; break;
    }
}

static void note_off(int ch, int note)
{
    for (int i = 0; i < VOICES; i++) {
        voice_t *v = &voices[i];
        if (v->active && !v->releasing && v->chan == ch && v->note == note)
            v->releasing = 1;
    }
}

static void bend_channel(int ch)
{
    for (int i = 0; i < VOICES; i++) {
        voice_t *v = &voices[i];
        if (v->active && v->chan == ch && ch != 9)
            v->inc = note_freq(v->note, mchan[ch].bend) / RATE;
    }
}

/* One event of track `t` (its delta time already read). */
static void track_event(track_t *t)
{
    if (t->p >= t->end) {
        t->done = 1;
        return;
    }
    uint8_t b = *t->p;
    if (b & 0x80) {
        t->p++;
        if (b < 0xf0)
            t->status = b;
    } else {
        b = t->status; /* running status */
        if (b == 0) {
            t->done = 1;
            return;
        }
    }
    int ch = b & 15;
    uint8_t d1 = 0, d2 = 0;
    switch (b & 0xf0) {
    case 0x80: case 0x90: case 0xa0: case 0xb0: case 0xe0:
        if (t->p + 2 > t->end) { t->done = 1; return; }
        d1 = t->p[0] & 0x7f; d2 = t->p[1] & 0x7f; t->p += 2;
        break;
    case 0xc0: case 0xd0:
        if (t->p + 1 > t->end) { t->done = 1; return; }
        d1 = t->p[0] & 0x7f; t->p += 1;
        break;
    }
    switch (b & 0xf0) {
    case 0x80:
        note_off(ch, d1);
        break;
    case 0x90:
        if (d2 == 0)
            note_off(ch, d1);
        else
            note_on(ch, d1, d2);
        break;
    case 0xb0:
        switch (d1) {
        case 7: mchan[ch].volume = d2 / 127.0f; break;
        case 10: mchan[ch].pan = d2 / 127.0f; break;
        case 11: mchan[ch].expression = d2 / 127.0f; break;
        case 120: case 123: /* all sound / notes off */
            for (int i = 0; i < VOICES; i++)
                if (voices[i].chan == ch)
                    voices[i].releasing = 1;
            break;
        case 121: /* reset controllers */
            mchan[ch].expression = 1.0f; mchan[ch].bend = 0; bend_channel(ch);
            break;
        }
        break;
    case 0xc0:
        mchan[ch].program = d1;
        break;
    case 0xe0:
        mchan[ch].bend = (((d2 << 7) | d1) - 8192) / 4096.0f; /* +-2 semitones */
        bend_channel(ch);
        break;
    case 0xf0:
        if (b == 0xff) {
            if (t->p >= t->end) { t->done = 1; return; }
            uint8_t type = *t->p++;
            uint32_t len = varlen(t);
            if (t->p + len > t->end) { t->done = 1; return; }
            if (type == 0x51 && len == 3)
                set_tempo(t->p[0] << 16 | t->p[1] << 8 | t->p[2]);
            if (type == 0x2f) {
                t->done = 1;
                return;
            }
            t->p += len;
        } else if (b == 0xf0 || b == 0xf7) {
            uint32_t len = varlen(t);
            t->p = (t->p + len <= t->end) ? t->p + len : t->end;
        }
        break;
    }
    if (t->p >= t->end)
        t->done = 1;
    else
        t->next += varlen(t);
}

/* Run the song's events up to its current position. */
static void song_events(void)
{
    for (;;) {
        track_t *first = NULL;
        for (int i = 0; i < song->ntracks && i < 64; i++)
            if (!tracks[i].done && (first == NULL || tracks[i].next < first->next))
                first = &tracks[i];
        if (first == NULL) {
            /* The end. */
            if (music_looping) {
                for (int i = 0; i < VOICES; i++)
                    voices[i].releasing = 1;
                rewind_song();
                continue;
            }
            music_playing = false;
            return;
        }
        if ((double)first->next > tick_pos)
            return;
        track_event(first);
    }
}

static float voice_sample(voice_t *v)
{
    float s;
    switch (v->wave) {
    case SINE: s = sinf(v->phase * 6.2831853f); break;
    case TRIANGLE: s = 4.0f * fabsf(v->phase - 0.5f) - 1.0f; break;
    case SQUARE: s = v->phase < 0.5f ? 0.6f : -0.6f; break;
    case SAW: s = 2.0f * v->phase - 1.0f; s *= 0.7f; break;
    case KICK: case TOM:
        s = sinf(v->phase * 6.2831853f);
        v->inc *= v->sweep;
        break;
    case NOISE:
    default:
        v->noise = v->noise * 1103515245u + 12345u;
        s = ((int)(v->noise >> 16 & 0x7fff) - 16384) / 16384.0f;
        if (v->hp) { /* hi-hats and cymbals: the high part */
            float out = s - v->lp;
            v->lp = s;
            s = out * 0.7f;
        } else { /* snares: a softer noise */
            v->lp += (s - v->lp) * 0.5f;
            s = v->lp * 1.5f;
        }
        break;
    }
    v->phase += v->inc;
    if (v->phase >= 1.0f)
        v->phase -= (int)v->phase;
    /* The envelope */
    if (v->releasing) {
        v->level -= v->release;
        if (v->level <= 0) {
            v->active = 0;
            return 0;
        }
    } else if (v->attack > 0) {
        v->level += v->attack;
        if (v->level >= 1.0f) {
            v->level = 1.0f;
            v->attack = 0;
        }
    } else if (v->level > v->sustain) {
        v->level -= v->decay;
        if (v->level <= v->sustain) {
            v->level = v->sustain;
            if (v->sustain <= 0) {
                v->active = 0;
                return 0;
            }
        }
    }
    return s * v->level * v->amp;
}

/* Mix `n` frames of music into `l`/`r` (floats, about -1..1). */
static void render_music(float *l, float *r, int n)
{
    if (song == NULL || !music_playing || music_paused)
        return;
    int done = 0;
    while (done < n && music_playing) {
        int block = n - done < 64 ? n - done : 64; /* events every 64 frames (3 ms) */
        song_events();
        for (int i = 0; i < VOICES; i++) {
            voice_t *v = &voices[i];
            if (!v->active)
                continue;
            midichan_t *c = &mchan[v->chan];
            float gain = c->volume * c->expression * music_volume * 0.22f;
            float pl = c->pan < 0.5f ? 1.0f : 2.0f * (1.0f - c->pan);
            float pr = c->pan > 0.5f ? 1.0f : 2.0f * c->pan;
            for (int k = 0; k < block && v->active; k++) {
                float s = voice_sample(v) * gain;
                l[done + k] += s * pl;
                r[done + k] += s * pr;
            }
        }
        tick_pos += block * ticks_per_frame;
        done += block;
    }
}

/* ---- mixing and writing ---- */

static int16_t outbuf[MAX_FRAMES * 2];
static float music_l[MAX_FRAMES], music_r[MAX_FRAMES];

static void OSS_Update(void)
{
    if (dsp < 0)
        return;
    audio_buf_info info;
    int delay = 0;
    if (ioctl(dsp, SNDCTL_DSP_GETOSPACE, &info) < 0 || ioctl(dsp, SNDCTL_DSP_GETODELAY, &delay) < 0)
        return;
    int want = RATE * 4 * AHEAD_MS / 1000 - delay;
    if (want > info.bytes)
        want = info.bytes;
    int frames = want / 4;
    if (frames > MAX_FRAMES)
        frames = MAX_FRAMES;
    if (frames <= 0)
        return;
    memset(music_l, 0, frames * sizeof(float));
    memset(music_r, 0, frames * sizeof(float));
    render_music(music_l, music_r, frames);
    for (int f = 0; f < frames; f++) {
        int l = (int)(music_l[f] * 32767.0f), r = (int)(music_r[f] * 32767.0f);
        for (int i = 0; i < NUM_CHANNELS; i++) {
            channel_t *c = &channels[i];
            if (c->s == NULL)
                continue;
            int s = (int)c->s->data[c->pos >> 16] - 128;
            l += s * c->left * 3 / 4;
            r += s * c->right * 3 / 4;
            c->pos += c->step;
            if ((c->pos >> 16) >= c->s->len)
                c->s = NULL;
        }
        outbuf[2 * f] = l > 32767 ? 32767 : l < -32768 ? -32768 : l;
        outbuf[2 * f + 1] = r > 32767 ? 32767 : r < -32768 ? -32768 : r;
    }
    if (write(dsp, outbuf, frames * 4) < 0)
        return;
}

static boolean OSS_Init(boolean prefix)
{
    use_sfx_prefix = prefix;
    memset(channels, 0, sizeof channels);
    dsp = open("/dev/dsp", O_WRONLY | O_NONBLOCK);
    if (dsp < 0) {
        printf("I_InitSound: no /dev/dsp: no sound\n");
        return false;
    }
    int frag = 16 << 16 | 10; /* 16 fragments of 1 KiB: 186 ms at most */
    int fmt = AFMT_S16_LE, ch = 2, rate = RATE;
    ioctl(dsp, SNDCTL_DSP_SETFRAGMENT, &frag);
    if (ioctl(dsp, SNDCTL_DSP_SETFMT, &fmt) < 0 || fmt != AFMT_S16_LE || ioctl(dsp, SNDCTL_DSP_CHANNELS, &ch) < 0 ||
        ch != 2 || ioctl(dsp, SNDCTL_DSP_SPEED, &rate) < 0 || rate != RATE) {
        printf("I_InitSound: /dev/dsp doesn't take 16-bit stereo at %d Hz: no sound\n", RATE);
        close(dsp);
        dsp = -1;
        return false;
    }
    printf("I_InitSound: sound effects and music through /dev/dsp (%d Hz)\n", RATE);
    return true;
}

static void OSS_Shutdown(void)
{
    if (dsp >= 0) {
        ioctl(dsp, SNDCTL_DSP_RESET, 0);
        close(dsp);
    }
    dsp = -1;
}

sound_module_t DG_sound_module = {
    sound_devices,
    sizeof sound_devices / sizeof sound_devices[0],
    OSS_Init,
    OSS_Shutdown,
    OSS_GetSfxLumpNum,
    OSS_Update,
    OSS_UpdateSoundParams,
    OSS_StartSound,
    OSS_StopSound,
    OSS_SoundIsPlaying,
    OSS_CacheSounds,
};

/* ---- the music module ---- */

static boolean Music_Init(void)
{
    return true;
}

static void Music_Shutdown(void)
{
    music_playing = false;
    all_voices_off();
}

static void Music_SetVolume(int volume)
{
    music_volume = volume / 127.0f;
}

static void Music_Pause(void)
{
    music_paused = true;
}

static void Music_Resume(void)
{
    music_paused = false;
}

static void *Music_Register(void *data, int len)
{
    uint8_t *midi = NULL;
    int midilen = 0;
    if (len >= 4 && memcmp(data, "MUS\x1a", 4) == 0) {
        /* DOOM's MUS: converted to MIDI. */
        MEMFILE *in = mem_fopen_read(data, len);
        MEMFILE *out = mem_fopen_write();
        if (!mus2mid(in, out)) {
            void *buf;
            size_t buflen;
            mem_get_buf(out, &buf, &buflen);
            midi = malloc(buflen);
            memcpy(midi, buf, buflen);
            midilen = buflen;
        }
        mem_fclose(in);
        mem_fclose(out);
    } else if (len >= 14 && memcmp(data, "MThd", 4) == 0) {
        midi = malloc(len);
        memcpy(midi, data, len);
        midilen = len;
    }
    if (midi == NULL || midilen < 14 || memcmp(midi, "MThd", 4) != 0) {
        free(midi);
        return NULL;
    }
    int division = midi[12] << 8 | midi[13];
    if (division & 0x8000) /* SMPTE time: not supported */
        division = 96;
    song_t *s = malloc(sizeof *s);
    s->data = midi;
    s->len = midilen;
    s->ntracks = midi[10] << 8 | midi[11];
    s->division = division ? division : 96;
    return s;
}

static void Music_Unregister(void *handle)
{
    song_t *s = handle;
    if (s == NULL)
        return;
    if (song == s) {
        song = NULL;
        music_playing = false;
        all_voices_off();
    }
    free(s->data);
    free(s);
}

static void Music_Play(void *handle, boolean looping)
{
    if (handle == NULL)
        return;
    all_voices_off();
    song = handle;
    music_looping = looping;
    music_paused = false;
    rewind_song();
    music_playing = true;
}

static void Music_Stop(void)
{
    music_playing = false;
    all_voices_off();
}

static boolean Music_IsPlaying(void)
{
    return music_playing;
}

static snddevice_t music_devices[] = {
    SNDDEVICE_PAS, SNDDEVICE_GUS, SNDDEVICE_WAVEBLASTER, SNDDEVICE_SOUNDCANVAS, SNDDEVICE_GENMIDI, SNDDEVICE_AWE32,
    SNDDEVICE_SB, SNDDEVICE_ADLIB,
};

music_module_t DG_music_module = {
    music_devices,
    sizeof music_devices / sizeof music_devices[0],
    Music_Init,
    Music_Shutdown,
    Music_SetVolume,
    Music_Pause,
    Music_Resume,
    Music_Register,
    Music_Unregister,
    Music_Play,
    Music_Stop,
    Music_IsPlaying,
    NULL,
};
