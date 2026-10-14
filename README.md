# ClubCraft

Open-source DJ software for macOS, written in Rust. It combines a **studio** for designing mixes
(in the spirit of DJ.Studio) with **live decks** for performing (in the spirit of VirtualDJ), on
one shared library and analysis. Tracks are separated into stems, analysed, and given cue points
automatically; the studio and the decks both use them.

## Features

**Stems**
- HTDemucs v4 (Meta, MIT) running natively: CoreML on the Apple GPU, with an ONNX Runtime CPU
  fallback. Output matches the PyTorch reference to more than 100 dB.
- Quality presets: Fast (`htdemucs`, about 18× real time on an M1 Pro), High (`htdemucs_ft`, the
  default) and Ultra (fine-tuned with shifts).
- Stems are cached as 8-channel FLAC. A track loaded on a deck before it was separated gets its
  stems progressively while it plays.

**Analysis**
- Beat grid, downbeat, key (Camelot / Open Key), loudness, energy, sections (intro, verse, build,
  drop, break, outro) and up to 8 auto cues: Start, Mix In, Vocal, Build, Drop, Break, second Drop
  or Outro, Mix Out.
- Validated against DJ.Studio and Mixed In Key on 137 tracks: BPM 99 %, beat phase 98.5 %,
  downbeat 96 %, key exact or relative 87 %.

**Studio**
- Harmonic auto-ordering by key, tempo and energy arc, with automatic mix points from the
  analysed structure.
- A two-lane beat-locked timeline with a per-lane mixer (gain, solo/mute, EQ, filter), a tempo
  lane and transition automation drawn on the clips.
- Transitions you can drag by bars, and a transition editor with presets: Stem blend, Bass swap,
  Crossfade, Vocal over, Filter out, Echo out and Cut.
- Preview, render to WAV with a `.cue` sheet, or **Play live**, which sends the mix to the decks.

**Live**
- Decks:
  - transport: CDJ cue logic, 8 hot cues prefilled from the analysis, loops, loop roll, beat jump
    and slip;
  - tempo and key: sync with phase meter, keylock (Signalsmith Stretch), key shift and match key;
  - stems: per-stem gain and pads (acapella / instrumental / drums);
  - mixing: 3-band EQ with kill, filter, and FX (echo, ping-pong, reverb, flanger, filter LFO,
    gate, roll, bitcrusher, brake).
- Mixer: crossfader curves, headphone cue (outputs 3/4 or split cable), recording, a limiter on
  the master.
- Automix from a queue, MIDI controllers with learn mode, and keyboard shortcuts.

**Interop**
- Imports DJ.Studio projects (mixes, mix points, transitions, cues). It only reads DJ.Studio's
  data and never writes to it.
- Exports beat grids, keys, hot cues and automix points to a VirtualDJ `database.xml`, keeping
  the rest of the file as it was.

## Build and run

```sh
# Rust 1.85+; stem models (≈3 GB) need the one-off export described in models/README.md
cargo run --release -p clubcraft
# app bundle with the CoreML models: target/ClubCraft.app
tools/bundle-macos.sh
```

### Windows

ClubCraft builds and runs on Windows 10/11 (x64). Stem separation uses ONNX Runtime with
**DirectML**, so it runs on any DirectX 12 GPU (NVIDIA, AMD or Intel). On first use ClubCraft
checks DirectML's output against the CPU once and remembers the result. Without a working GPU it
falls back to the Fast model on the CPU.

```powershell
# Rust (rustup, MSVC toolchain) + Visual Studio Build Tools (C++)
.\tools\package-windows.ps1        # → dist\ClubCraft\ClubCraft.exe (+ DLLs, models if present)
```

The models are the ONNX exports. Create them with the same Python script, skipping CoreML:
`python export_htdemucs.py --model htdemucs_ft --out ..\..\models --no-coreml` (see
`models/README.md`). Audio goes through WASAPI; headphone cue needs a 4-output audio interface or
the split-cable mode. Every push builds the macOS and Windows versions in CI; the Windows build
is attached to the workflow run as an artifact (without models).

Data lives in `~/Music/ClubCraft` (library database, stem and waveform caches, exports,
recordings). Set `CLUBCRAFT_HOME` to use a different folder.

## Keyboard (Live)

| | Deck A | Deck B |
|---|---|---|
| Cue / Play / Sync | Z / X / C | M / , / . |
| Hot cues | 1–8 | Shift + 1–8 |

Arrow keys nudge the crossfader; Space flips it from side to side.

## Layout

| Crate | Purpose |
|---|---|
| `crates/core` | data model, SQLite library, key/Camelot, planner (ordering, mix points), mix timeline |
| `crates/audio` | decoding (symphonia), tags (lofty), resampling |
| `crates/stems` | HTDemucs inference, STFT, segmenting/overlap-add, stem cache |
| `crates/analysis` | beat grid, key, loudness, structure, cues, waveform summaries |
| `crates/engine` | real-time engine (cpal), decks, mixer, FX, stretcher, conductor (preview/automix/render), MIDI |
| `crates/app` | egui GUI: Home, Library, Studio, Live, Settings, importers/exporters |

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md). `docs/research` contains interoperability
notes on DJ.Studio and VirtualDJ file formats, gathered from the installed apps. No code or
assets from those products are used.

## Licence

MIT OR Apache-2.0. The Demucs weights are MIT (Meta Platforms) and Signalsmith Stretch is MIT.
