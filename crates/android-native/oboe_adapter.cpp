#include "oboe_adapter.h"
#include <algorithm>
#include <array>
#include <atomic>
#include <jni.h>
#include <memory>
#include <oboe/FullDuplexStream.h>
#include <oboe/Oboe.h>

extern "C" JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM *, void *) { return JNI_VERSION_1_6; }
struct Faults {
    std::atomic<int> error{0};
};
struct Events : oboe::AudioStreamErrorCallback, oboe::AudioStreamRoutingCallback {
    std::shared_ptr<Faults> faults;
    std::atomic<int> inputId{0}, outputId{0};
    std::atomic<bool> armed{false};
    explicit Events(std::shared_ptr<Faults> f) : faults(std::move(f)) {}
    bool onError(oboe::AudioStream *stream, oboe::Result) override {
        faults->error.fetch_or(stream->getDirection() == oboe::Direction::Input ? 1 : 2);
        return true; // owner stops output before closing a nonblocking input
    }
    void onRoutingChanged(oboe::AudioStream *stream, const int32_t *ids, int32_t count) override {
        bool in = stream->getDirection() == oboe::Direction::Input;
        int expected = in ? inputId.load() : outputId.load();
        if (armed.load() && (count != 1 || (expected != 0 && ids[0] != expected)))
            faults->error.fetch_or(in ? 1 : 2);
    }
};
struct LitzAudio : oboe::FullDuplexStream {
    void *bridge;
    std::shared_ptr<oboe::AudioStream> input, output;
    std::shared_ptr<Faults> faults = std::make_shared<Faults>();
    std::shared_ptr<Events> events = std::make_shared<Events>(faults);
    std::array<float, 2048> scratch{};
    int32_t outputXruns = 0;

    explicit LitzAudio(void *b) : bridge(b) {}
    // Like oboe::LatencyTuner: start at two bursts, add one per xrun.
    void tuneOutput(oboe::AudioStream *stream) {
        auto xruns = stream->getXRunCount();
        if (!xruns || xruns.value() <= outputXruns)
            return;
        outputXruns = xruns.value();
        int32_t next = stream->getBufferSizeInFrames() + stream->getFramesPerBurst();
        if (next <= stream->getBufferCapacityInFrames())
            stream->setBufferSizeInFrames(next);
    }
    oboe::DataCallbackResult onAudioReady(oboe::AudioStream *stream, void *data,
                                          int32_t frames) override {
        if (faults->error.load() != 0) {
            if (stream == output.get())
                std::fill_n(static_cast<float *>(data), frames * 2, 0.f);
            return oboe::DataCallbackResult::Stop;
        }
        if (stream == output.get())
            tuneOutput(stream);
        if (stream == input.get()) {
            litz_capture(bridge, static_cast<float *>(data), frames);
        } else if (input) {
            auto result = oboe::FullDuplexStream::onAudioReady(stream, data, frames);
            if (result == oboe::DataCallbackResult::Stop)
                faults->error.fetch_or(1);
            return result;
        } else {
            litz_render(bridge, static_cast<float *>(data), frames);
        }
        return oboe::DataCallbackResult::Continue;
    }
    oboe::DataCallbackResult onBothStreamsReady(const void *capture, int captured, void *render,
                                                int frames) override {
        if (captured > 0)
            litz_capture(bridge, static_cast<const float *>(capture), captured);
        // Hardware clocks can differ. After upstream's normal read, consume a
        // bounded remainder so a faster input does not accumulate stale samples.
        auto available = input->getAvailableFrames();
        if (!available) {
            faults->error.fetch_or(1);
            return oboe::DataCallbackResult::Stop;
        }
        if (available.value() > 0) {
            auto result =
                input->read(scratch.data(),
                            std::min(available.value(), static_cast<int32_t>(scratch.size())), 0);
            if (!result) {
                faults->error.fetch_or(1);
                return oboe::DataCallbackResult::Stop;
            }
            if (result.value() > 0)
                litz_capture(bridge, scratch.data(), result.value());
        }
        litz_render(bridge, static_cast<float *>(render), frames);
        return oboe::DataCallbackResult::Continue;
    }
    oboe::Result open(bool mic, bool playback, int inId, int outId, int source) {
        events->inputId = inId;
        events->outputId = outId;
        auto make = [&](bool isInput, int id, std::shared_ptr<oboe::AudioStream> &s) {
            oboe::AudioStreamBuilder b;
            // No API is forced: Oboe uses OpenSL ES where AAudio is missing or broken.
            b.setDirection(isInput ? oboe::Direction::Input : oboe::Direction::Output)
                ->setPerformanceMode(oboe::PerformanceMode::LowLatency)
                ->setSharingMode(oboe::SharingMode::Exclusive)
                ->setFormat(oboe::AudioFormat::Float)
                ->setChannelCount(isInput ? 1 : 2)
                ->setDeviceId(id)
                ->setInputPreset(static_cast<oboe::InputPreset>(source))
                ->setUsage(oboe::Usage::Media)
                ->setContentType(oboe::ContentType::Music)
                ->setErrorCallback(events)
                ->setRoutingCallback(events);
            // Open at native hardware rate first. On a non-48k route use Oboe's
            // explicit native-rate software converter, not Android's slow mixer
            // rate conversion. The wire/engine clock remains 48k.
            if (!isInput || !playback)
                b.setDataCallback(this);
            if (isInput && output)
                b.setBufferCapacityInFrames(output->getBufferCapacityInFrames() * 2);
            auto tryOpen = [&]() {
                s.reset();
                auto result = b.openStream(s);
                if (result != oboe::Result::OK) {
                    s.reset();
                    b.setSharingMode(oboe::SharingMode::Shared);
                    result = b.openStream(s);
                }
                return result;
            };
            auto result = tryOpen();
            if (result != oboe::Result::OK)
                return result;
            if (s->getSampleRate() != 48000) {
                s->close();
                s.reset();
                b.setSampleRate(48000)->setSampleRateConversionQuality(
                    oboe::SampleRateConversionQuality::Medium);
                result = tryOpen();
                if (result != oboe::Result::OK)
                    return result;
            }
            if (s->getSampleRate() != 48000 || s->getFormat() != oboe::AudioFormat::Float ||
                s->getChannelCount() != (isInput ? 1 : 2) || (id != 0 && s->getDeviceId() != id))
                return oboe::Result::ErrorInvalidFormat;
            if (!isInput)
                s->setBufferSizeInFrames(s->getFramesPerBurst() * 2);
            return oboe::Result::OK;
        };
        if (playback) {
            auto r = make(false, outId, output);
            if (r != oboe::Result::OK)
                return r;
        }
        if (mic) {
            auto result = make(true, inId, input);
            if (result != oboe::Result::OK)
                return result;
        }
        if (input && output) {
            setSharedInputStream(input);
            setSharedOutputStream(output);
        }
        return oboe::Result::OK;
    }
    ~LitzAudio() {
        // stop() waits for the callback, unlike requestStop(). Never close the
        // nonblocking input until no output callback can reference it.
        if (output) {
            output->stop();
            output->close();
        }
        if (input) {
            input->stop();
            input->close();
        }
    }
};
extern "C" LitzAudio *litz_audio_open(void *b, bool mic, bool playback, int in, int out,
                                      int source) {
    auto audio = std::make_unique<LitzAudio>(b);
    if (audio->open(mic, playback, in, out, source) != oboe::Result::OK)
        return nullptr;
    return audio.release();
}
extern "C" int litz_audio_start(LitzAudio *a) {
    a->events->armed.store(true);
    if ((a->input && a->events->inputId.load() != 0 &&
         a->input->getDeviceId() != a->events->inputId.load()) ||
        (a->output && a->events->outputId.load() != 0 &&
         a->output->getDeviceId() != a->events->outputId.load()) ||
        a->faults->error.load() != 0)
        return -1;
    if (a->input && a->output)
        return static_cast<int>(a->start());
    auto r = a->input ? a->input->requestStart() : oboe::Result::OK;
    if (r == oboe::Result::OK && a->output)
        r = a->output->requestStart();
    return static_cast<int>(r);
}
extern "C" void litz_audio_close(LitzAudio *a) { delete a; }
extern "C" void litz_audio_info(LitzAudio *a, LitzAudioInfo *i) {
    *i = {};
    i->error = a->faults->error.load();
    i->source = a->input ? static_cast<int32_t>(a->input->getInputPreset()) : 0;
    auto fill = [](const std::shared_ptr<oboe::AudioStream> &s, int32_t &rate, int32_t &device,
                   int32_t &exclusive, int32_t &low, int32_t &buffer, int32_t &xruns) {
        if (!s)
            return;
        rate = s->getSampleRate();
        device = s->getDeviceId();
        exclusive = s->getSharingMode() == oboe::SharingMode::Exclusive;
        low = s->getPerformanceMode() == oboe::PerformanceMode::LowLatency;
        buffer = s->getBufferSizeInFrames();
        auto x = s->getXRunCount();
        xruns = x ? x.value() : -1;
    };
    fill(a->input, i->input_rate, i->input_device, i->input_exclusive, i->input_low_latency,
         i->input_buffer, i->input_xruns);
    fill(a->output, i->output_rate, i->output_device, i->output_exclusive, i->output_low_latency,
         i->output_buffer, i->output_xruns);
}
extern "C" int litz_jni_length(void *e, void *s) {
    return static_cast<JNIEnv *>(e)->GetStringLength(static_cast<jstring>(s));
}
extern "C" int litz_jni_exception(void *e) {
    return static_cast<JNIEnv *>(e)->ExceptionCheck() ? 1 : 0;
}
extern "C" const uint16_t *litz_jni_chars(void *e, void *s) {
    return static_cast<JNIEnv *>(e)->GetStringChars(static_cast<jstring>(s), nullptr);
}
extern "C" void litz_jni_release(void *e, void *s, const uint16_t *c) {
    static_cast<JNIEnv *>(e)->ReleaseStringChars(static_cast<jstring>(s), c);
}
extern "C" void *litz_jni_string(void *e, const uint16_t *s, int length) {
    return static_cast<JNIEnv *>(e)->NewString(s, length);
}
