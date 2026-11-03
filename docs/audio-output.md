# Audio outputs

In Settings → Audio output, choose a master device and a headphone routing mode.
Changes are staged until **Apply**; **Discard changes** restores the last applied settings.

The existing single-device modes remain available: Auto uses master 1/2 and cue 3/4
when the device exposes four outputs; Off uses master only; Split sends mono master
left and mono cue right. Explicit channel pairs must not overlap.

## Two devices

Choose **Separate device (stereo cue)** and select a headphone device. Master and
cue use channels 1/2 of their respective devices. Any two devices in the list can
be selected; there are still only two active output destinations.

Both devices must support stereo F32 at the same nominal sample rate and requested
buffer size. Auto chooses common 44.1 kHz, then common 48 kHz. An explicit rate is
required, not a preference which may silently fall back. Drivers can deliver
different callback lengths despite a common buffer request; the settings show the
observed lengths for both streams. Selecting Device default leaves buffer sizing
to each driver.

Devices are saved by ID. Existing settings using names are migrated when resolved;
duplicate names require selecting an ID again. An explicitly selected missing
master is never replaced by the system default. If headphones cannot open at
startup, the selected master can run alone with a visible warning.

## Alignment

Only the master callback advances decks and records the master bus. A bounded,
preallocated SPSC queue carries stereo cue frames to the headphone callback. A
32-tap windowed-sinc resampler slowly adjusts its ratio according to queue fill,
compensating independent device clock drift without changing the master signal.
The cue queue targets at least 20 ms or two callbacks, whichever is larger.

Each cue frame carries the undelayed master presentation estimate in a shared
monotonic epoch. The cue callback compares this with its own driver presentation
estimate, accounting for the queue and resampler lookahead. These estimates are
smoothed; the earlier output receives a fractional delay, with a slewing delay tap
to avoid discontinuities. Added delay is bounded to two seconds per output.

**Alignment correction** ranges from −500 to +500 ms. Positive means the headphones
lag further than estimated, so master receives more delay; negative moves the
balance towards delaying headphones. The correction is bound to the resolved
device pair, rate and buffer. Changing these resets the correction.

When driver timestamps are unavailable, the UI says so and only the manual
correction is applied. Driver estimates do not measure the entire analog or
acoustic path. Verify alignment by listening; precise analog calibration requires
an external measurement. In particular, aligning a slow headphone output increases
the latency from a control action to master playback.

## Failure and recording

Cue underflow fades to silence and waits to refill; overflow discards stale cue
data and fades back in. Separate underrun/overflow counters are shown. A failed
headphone stream does not stop master and is never routed to the speakers.
A failed master stream stops advancing decks. Use Apply to retry or change devices.

Configuration is validated before stopping an existing stream. The core is lent
to a callback only after all new streams have opened and started; destroying the
callback returns that same core even if the device no longer calls it. Failed
changes attempt to restore the old output. If restoration fails, the core stays
parked with deck state retained for another Apply.

Recording taps the original master before output alignment delays. Sample-rate
changes are rejected while recording; stop recording before changing the rate.

## Verification

Automated tests cover settings compatibility, ID selection, format negotiation,
core ownership on callback destruction, independent clock control at ±1000 ppm,
fractional delay and correction sign, stereo cue resampling and rebuffering.
The clock-control simulation runs a virtual hour without playing audio.

For a silent hardware probe, first list IDs, then pass a master/cue pair:

```sh
cargo run -p clubcraft-engine --example output_probe -j 4
cargo run -p clubcraft-engine --example output_probe -j 4 -- "master-id" "cue-id"
```

The probe loads no tracks, sets both output gains to their minimum, checks that
both callbacks run, rejects a missing device and reopens the pair while checking
that core settings survive.

Hardware acceptance still requires checking both outputs, disconnect/reconnect,
and analog alignment on Windows, macOS and Linux. Any hardware test must keep
master volume at zero. Software timing estimates alone cannot establish analog
alignment accuracy.
