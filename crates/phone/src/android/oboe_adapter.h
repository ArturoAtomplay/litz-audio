#pragma once
#include <stdint.h>

struct LitzAudio;
struct LitzAudioInfo {
    int32_t input_rate, output_rate, input_device, output_device;
    int32_t input_exclusive, output_exclusive, input_low_latency, output_low_latency;
    int32_t input_buffer, output_buffer, input_xruns, output_xruns, error, source;
};
extern "C" {
LitzAudio *litz_audio_open(void *bridge, bool mic, bool playback, int input, int output,
                           int source);
int litz_audio_start(LitzAudio *audio);
void litz_audio_info(LitzAudio *audio, LitzAudioInfo *info);
void litz_audio_close(LitzAudio *audio);
void litz_capture(void *bridge, const float *samples, int frames);
void litz_render(void *bridge, float *samples, int frames);
}
