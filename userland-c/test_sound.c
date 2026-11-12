/* Copyright (c) 2026 tre4surehunter9
 * test_sound: checks the sound card through OSS: /dev/mixer's volumes,
 * /dev/dsp's formats, rates and buffer ioctls, and that the card plays
 * what is written at the right speed (a tone of 0.75 s takes about that
 * long to play). Exits 0, or the line number of the first failed check. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <math.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/soundcard.h>
#include <time.h>
#include <unistd.h>

#define CHECK(c) do { if (!(c)) { fprintf(stderr, "test_sound: check failed at line %d: %s (errno %d)\n", __LINE__, #c, errno); return __LINE__; } } while (0)

static double now(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static int mixer(void)
{
    int m = open("/dev/mixer", O_RDWR);
    CHECK(m >= 0);
    int v = 0;
    CHECK(ioctl(m, OSS_GETVERSION, &v) == 0 && v >= 0x030000);
    int devs = 0;
    CHECK(ioctl(m, SOUND_MIXER_READ_DEVMASK, &devs) == 0);
    CHECK(devs & SOUND_MASK_VOLUME && devs & SOUND_MASK_PCM);
    v = 60 | 40 << 8;
    CHECK(ioctl(m, SOUND_MIXER_WRITE_VOLUME, &v) == 0);
    v = 0;
    CHECK(ioctl(m, SOUND_MIXER_READ_VOLUME, &v) == 0 && v == (60 | 40 << 8));
    v = 100 | 100 << 8;
    CHECK(ioctl(m, SOUND_MIXER_WRITE_VOLUME, &v) == 0 && ioctl(m, SOUND_MIXER_WRITE_PCM, &v) == 0);
    mixer_info info;
    CHECK(ioctl(m, SOUND_MIXER_INFO, &info) == 0 && info.name[0] != 0);
    CHECK(ioctl(m, TCGETS + 0, NULL) == -1 && errno == ENOTTY);
    close(m);
    return 0;
}

/* A sine tone: 16-bit stereo. */
static int16_t *tone(int rate, double seconds, size_t *bytes)
{
    size_t frames = rate * seconds;
    int16_t *b = malloc(frames * 4);
    for (size_t i = 0; i < frames; i++)
        b[2 * i] = b[2 * i + 1] = (int16_t)(8000 * sin(2 * M_PI * 440 * i / rate));
    *bytes = frames * 4;
    return b;
}

static int dsp(void)
{
    int d = open("/dev/dsp", O_WRONLY);
    CHECK(d >= 0);
    CHECK(open("/dev/dsp", O_WRONLY) == -1 && errno == EBUSY);
    int fmts = 0;
    CHECK(ioctl(d, SNDCTL_DSP_GETFMTS, &fmts) == 0 && fmts & AFMT_S16_LE && fmts & AFMT_U8);
    int f = AFMT_S16_LE, ch = 2, rate = 22050;
    CHECK(ioctl(d, SNDCTL_DSP_SETFMT, &f) == 0 && f == AFMT_S16_LE);
    CHECK(ioctl(d, SNDCTL_DSP_CHANNELS, &ch) == 0 && ch == 2);
    CHECK(ioctl(d, SNDCTL_DSP_SPEED, &rate) == 0 && rate == 22050);
    int blk = 0;
    CHECK(ioctl(d, SNDCTL_DSP_GETBLKSIZE, &blk) == 0 && blk > 0);
    audio_buf_info space;
    CHECK(ioctl(d, SNDCTL_DSP_GETOSPACE, &space) == 0);
    CHECK(space.fragsize > 0 && space.fragstotal >= 2 && space.bytes >= space.fragsize);
    struct pollfd p = { .fd = d, .events = POLLOUT };
    CHECK(poll(&p, 1, 0) == 1 && p.revents & POLLOUT);

    /* 0.75 s of tone: write, then wait until it has played. */
    size_t n;
    int16_t *t = tone(rate, 0.75, &n);
    double start = now();
    CHECK(write(d, t, n) == (ssize_t)n);
    count_info played;
    CHECK(ioctl(d, SNDCTL_DSP_GETOPTR, &played) == 0);
    int delay = 0;
    CHECK(ioctl(d, SNDCTL_DSP_GETODELAY, &delay) == 0 && delay > 0 && delay <= (int)n);
    CHECK(ioctl(d, SNDCTL_DSP_SYNC, 0) == 0);
    double took = now() - start;
    fprintf(stderr, "test_sound: 0.75 s of sound played in %.2f s\n", took);
    /* (Real time, give or take: QEMU's silent sound output catches up in
     * bursts after the host was busy.) */
    CHECK(took > 0.4 && took < 2.0);
    count_info after;
    CHECK(ioctl(d, SNDCTL_DSP_GETOPTR, &after) == 0 && after.bytes >= (int)n * 9 / 10);
    CHECK(ioctl(d, SNDCTL_DSP_GETODELAY, &delay) == 0 && delay == 0);

    /* Non-blocking: a big write takes what fits and returns at once. */
    int on = 1;
    CHECK(ioctl(d, SNDCTL_DSP_NONBLOCK, 0) == 0);
    (void)on;
    free(t);
    t = tone(rate, 3.0, &n);
    start = now();
    ssize_t w = write(d, t, n);
    CHECK(w > 0 && w < (ssize_t)n && now() - start < 0.3);
    CHECK(write(d, t, n) == -1 && errno == EAGAIN);
    CHECK(ioctl(d, SNDCTL_DSP_RESET, 0) == 0);
    CHECK(ioctl(d, SNDCTL_DSP_GETODELAY, &delay) == 0 && delay == 0);
    free(t);

    /* Small fragments (as games ask for): 4 of 1024 bytes. */
    int frag = 4 << 16 | 10;
    CHECK(ioctl(d, SNDCTL_DSP_SETFRAGMENT, &frag) == 0);
    CHECK(ioctl(d, SNDCTL_DSP_GETOSPACE, &space) == 0 && space.fragsize == 1024 && space.fragstotal == 4);
    close(d);

    /* The default format (8 kHz, unsigned 8-bit, mono), and resampling-free
     * odd rates: half a second of a square wave. */
    d = open("/dev/dsp", O_WRONLY);
    CHECK(d >= 0);
    rate = 11025;
    CHECK(ioctl(d, SNDCTL_DSP_SPEED, &rate) == 0 && rate == 11025);
    uint8_t sq[5512];
    for (size_t i = 0; i < sizeof sq; i++)
        sq[i] = (i / 25) % 2 ? 160 : 96;
    start = now();
    CHECK(write(d, sq, sizeof sq) == sizeof sq);
    CHECK(ioctl(d, SNDCTL_DSP_SYNC, 0) == 0);
    took = now() - start;
    CHECK(took > 0.2 && took < 1.5);
    close(d);
    /* Closed: it can be opened again at once. */
    d = open("/dev/dsp", O_WRONLY);
    CHECK(d >= 0);
    close(d);
    return 0;
}

int main(void)
{
    if (access("/dev/dsp", F_OK) != 0 || open("/dev/mixer", O_RDONLY) < 0) {
        printf("no sound card: skipped\n");
        return 0;
    }
    int r;
    if ((r = mixer()) || (r = dsp()))
        return r;
    printf("test_sound: all checks passed\n");
    return 0;
}
