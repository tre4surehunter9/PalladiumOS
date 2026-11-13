/* Copyright (c) 2026 tre4surehunter9
 * libao's OSS output plugin (src/plugins/oss/ao_oss.c), built into the
 * library as a built-in driver, "oss": a static libao can't load plugins.
 * Its ao_plugin_* functions are renamed ao_oss_* and listed in an
 * ao_functions table, as libao's own built-in drivers (ao_wav.c) are.
 * Included at the end of ao_null.c by tools/ports/vorbis.sh. */
#define ao_plugin_test ao_oss_test
#define ao_plugin_driver_info ao_oss_driver_info
#define ao_plugin_device_init ao_oss_device_init
#define ao_plugin_set_option ao_oss_set_option
#define ao_plugin_open ao_oss_open
#define ao_plugin_play ao_oss_play
#define ao_plugin_close ao_oss_close
#define ao_plugin_device_clear ao_oss_device_clear
#define ao_plugin_file_extension ao_oss_file_extension
#define HAVE_SYS_SOUNDCARD_H 1
#include "plugins/oss/ao_oss.c"

ao_functions ao_oss = {
	ao_oss_test,
	ao_oss_driver_info,
	ao_oss_device_init,
	ao_oss_set_option,
	ao_oss_open,
	ao_oss_play,
	ao_oss_close,
	ao_oss_device_clear,
	NULL /* a live output: no file extension */
};
