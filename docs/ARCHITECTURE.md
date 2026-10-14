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
| `clubcraft` (app) | egui GUI: Library, Studio, Live; background job queue; importers (DJ.Studio, VirtualDJ) and exporters; `clubcraft mcp` (MCP server on stdio, `mcp.rs`) |

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

## MCP server

`clubcraft mcp` speaks JSON-RPC 2.0 over stdio (newline-delimited; protocol 2025-06-18, also
2025-03-26 / 2024-11-05). Tool calls run on their own threads with their own SQLite connection,
report `notifications/progress` when the client sends a progress token, and stop on
`notifications/cancelled`. Separation, deep analysis and rendering take `jobs::heavy()`, an
in-process mutex plus an OS file lock on `<data>/heavy.lock` shared with the app. After every write
the server touches `<data>/.library-changed`; the app polls its mtime once per second and reloads
the library, and the open mix too if it has no unsaved edits.

## Mashup finder

`clubcraft-analysis::mashup` holds the features and the scorer, `crates/app/src/mashup.rs` the
search job and `mashup_view.rs` the window.

* **Features**: per bar of the beat grid, from the stems: chroma of the vocal, bass and melody
  stems (16384-point STFT, A440, log-compressed, salient notes only), the RMS level of every stem
  and the melody's level in the vocal band (300 Hz to 3.4 kHz). They're stored in
  `<data>/Mashup/<hash>.ccmf` (about 6 kB per track), written by the analysis when stems exist,
  and computed on demand for older tracks.
* **Score** for "vocals of A, bars a..a+n, shifted k semitones, over the instrumental of B from
  bar b":
  * per bar, a directional interval-consonance table applied to the two chroma vectors
    (`CONSONANCE`), with bass weight 0.45;
  * calibrated to 0..1 (random ≈ 0.5, own instrumental ≈ 0.65), combined as 0.7·mean + 0.3·weakest
    4 bars;
  * minus masking, leftover vocals of B, tempo cost (quadratic within the tolerance) and key cost,
    plus a phrase bonus.
  * `examples/mashup_eval.rs` measures it: own pairing vs its ±1-2 semitone shifts AUC 0.93,
    vs random placements 0.78. Chroma cosine scores 0.84 / 0.72, beat resolution doesn't help.
* **Search**: every vocal phrase (default: the 3 strongest) × every phrase-aligned bar of every
  track with stems × key shifts, in parallel. That's under 1 ms for 17 tracks.
* **Re-ranking**: COCOLA (`analysis::cocola`).
  * The librosa front end (STFT, median-filter HPSS, Slaney mel, dB) is ported to Rust and matches
    librosa within 0.003 dB (`tests/cocola_ref.rs`), and the ONNX embeddings match PyTorch within 1e-5.
  * Two 5 s snippets per candidate: the vocals stretched and pitch-shifted with Signalsmith, as the
    mix plays them, and the instrumental (drums + bass + melody).
  * Final score = 0.6 rule + 0.4 learned (AUC vs random: 0.78 rule, 0.79 learned, 0.84 blend).
* **Result**: a `TransitionStyle::Mashup`: the outgoing instrumental and the incoming instrumental
  swap within the first bar, the outgoing vocals sing for the whole transition, the incoming
  vocals return on its last beat. Previews play on the studio decks through a separate
  `MixPlayer` (`LiveEngine::preview_player`).
