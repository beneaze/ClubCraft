// Minimal C ABI around SignalsmithStretch<float> for clubcraft-engine (interleaved stereo I/O).
#include "signalsmith-stretch/signalsmith-stretch.h"
#include <cstddef>

namespace {
struct Interleaved {
    float *data;
    int channels;
    struct Channel {
        float *data;
        int c, stride;
        float &operator[](size_t i) { return data[i * stride + c]; }
        const float &operator[](size_t i) const { return data[i * stride + c]; }
    };
    Channel operator[](size_t c) { return Channel{data, (int)c, channels}; }
};
struct CcStretch {
    signalsmith::stretch::SignalsmithStretch<float> s;
    int channels;
};
}

extern "C" {
void *cc_stretch_create(int channels, int block, int interval, int split) {
    auto h = new CcStretch;
    h->channels = channels;
    h->s.configure(channels, block, interval, split != 0);
    return h;
}
void cc_stretch_destroy(void *h) { delete static_cast<CcStretch *>(h); }
void cc_stretch_reset(void *h) { static_cast<CcStretch *>(h)->s.reset(); }
int cc_stretch_input_latency(void *h) { return static_cast<CcStretch *>(h)->s.inputLatency(); }
int cc_stretch_output_latency(void *h) { return static_cast<CcStretch *>(h)->s.outputLatency(); }
void cc_stretch_set_transpose(void *h, float factor) { static_cast<CcStretch *>(h)->s.setTransposeFactor(factor); }
void cc_stretch_seek(void *hp, const float *input, int frames, double rate) {
    auto h = static_cast<CcStretch *>(hp);
    Interleaved in{const_cast<float *>(input), h->channels};
    h->s.seek(in, frames, rate);
}
void cc_stretch_process(void *hp, const float *input, int in_frames, float *output, int out_frames) {
    auto h = static_cast<CcStretch *>(hp);
    Interleaved in{const_cast<float *>(input), h->channels};
    Interleaved out{output, h->channels};
    h->s.process(in, in_frames, out, out_frames);
}
}
