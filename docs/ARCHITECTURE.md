# ClubCraft architecture

ClubCraft is an open-source DJ application in Rust with two halves that share one library:

* **Studio** (DJ.Studio-style): build a mix offline — order tracks harmonically, place
  beat-matched transitions with stem-aware automation, preview, render to WAV/FLAC/MP3.
* **Live** (VirtualDJ-style): 2/4 decks, scrolling stem-coloured waveforms, hot cues, loops,
  sync, keylock, EQ/filter/FX, real-time stem control, automix, MIDI controllers.

Both halves use the same analysis: beat grid, key, sections, and **automatically generated cue
points**. A track analysed for the studio arrives in a live deck with its hot cues already set
(Start, Mix-in, Vocal, Drop, Break, Mix-out ...), and a studio mix can be played live through
automix with the planned mix points.

## Crates

| crate | role |
|---|---|
| `clubcraft-core` | data model (`Track`, `Analysis`, `BeatGrid`, `Key`, `Section`, `Cue`, `Mix`, `Transition`), SQLite `Library`, paths, `StemTrack` progressive stem buffer |
| `clubcraft-audio` | decoding (symphonia) to 44.1 kHz stereo f32 `AudioBuffer`, tag metadata (lofty), resampling |
| `clubcraft-stems` | HTDemucs stem separation in Rust: STFT/iSTFT + CoreML (GPU) backend, ONNX Runtime CPU fallback; segmenting/overlap-add/shifts; stem file cache |
| `clubcraft-analysis` | beat grid, key, loudness, energy, waveform summary, structure (sections) and cue generation — uses stems when present |
| `clubcraft-engine` | real-time audio engine (cpal): decks, mixer, EQ, filter, FX, time-stretch, sync, loops, stems mixing, headphone cue, recording, automix; plus the offline mix renderer |
| `clubcraft` (app) | egui GUI: Library, Studio, Live; background job queue; importers (DJ.Studio, VirtualDJ) and exporters |

Dependency direction: `core` ← `audio` ← (`stems`, `analysis`, `engine`) ← `app`.

## Conventions

* Internal audio: **44 100 Hz, stereo, f32**. Positions in **seconds** of the original track
  unless stated otherwise. Beat grids are constant-tempo (`BeatGrid { bpm, downbeat }`).
* Stem order everywhere: **drums, bass, other ("Melody"), vocals** (`StemKind`), matching Demucs.
* Stem playback uses the *residual trick*: `out = original + Σ (gain_s − 1) · stem_s`.
  With all gains at 1 the output is bit-exact the original; muting vocals subtracts the vocal stem.
* Cached data lives in `~/Music/ClubCraft/` (`library.db`, `Stems/<hash>.stems.*`,
  `Waveforms/<hash>.ccwf`, `Exports/`, `Recordings/`). Track identity across renames is the
  content hash in `Track::hash`.
* Models live in `models/` (exported by `tools/model-export/export_htdemucs.py`):
  `<name>.json` manifest + `<name>[_i].mlmodelc` (CoreML) + `<name>[_i].onnx`.
  Graph inputs: `mix [1,2,343980]` waveform and `spec [1,4,2048,336]` complex-as-channels
  STFT; outputs `spec_out [1,S,4,2048,336]` and `wave_out [1,S,2,343980]`; final stems =
  iSTFT(spec_out) + wave_out. See the Python `Core` module for the exact semantics.

## Interoperability

The DJ.Studio importer (`crates/app/src/interop/djstudio.rs`) reads DJ.Studio's local project and
library JSON read-only; the VirtualDJ exporter (`interop/virtualdj.rs`) merges cues into a
`database.xml`. Both are written from observed file formats only; no code or assets from either
product are used.
