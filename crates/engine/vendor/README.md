Vendored, unmodified headers of [Signalsmith Stretch](https://github.com/Signalsmith-Audio/signalsmith-stretch)
and [Signalsmith Linear](https://github.com/Signalsmith-Audio/linear) (MIT, see the LICENSE files),
taken from the `signalsmith-stretch` 0.1.3 crate. They are compiled directly (see `csrc/stretch.cpp`,
`build.rs`) so the engine can enable `splitComputation` (spreads the FFT work evenly over blocks,
avoiding CPU spikes on the audio thread) and Apple Accelerate FFTs on macOS, neither of which the
Rust wrapper crate exposes.
