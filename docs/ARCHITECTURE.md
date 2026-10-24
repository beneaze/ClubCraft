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
* Stem playback uses the *residual trick*: `out = r·original + Σ (gain_s − r) · stem_s` with `r`
  the mean of the four gains, i.e. the stems plus `r` times the part no stem holds
  (`original − Σ stems`, 21–34 dB below the track with HTDemucs). With all gains at 1 the output is
  bit-exact the original; with all at 0 it's silence.
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
Design and sources: [mashup-research.md](mashup-research.md). The finder works in whole units,
never sliding windows: a sung phrase of A goes on a section start of B. It searches both ways
round for the chosen track, which plays first in a mix either way (`MashupOrder`): its vocals
over the other tracks' instrumentals (old vocals in the new song), or the other tracks' vocals
over its instrumental (new vocals in the old song; every candidate needs its vocal map, computed
once; structure labels are used where cached, a phrase without them gets a neutral prior). With
the instrumental first, it ends with the transition, so a late spot in it is fine, while the
vocal track plays on after its phrase, which therefore must not sit just before its outro.

* **Vocal units** (`mashup::vocal_units`): sung sections of A by the All-In-One structure model's
  labels (`analysis::labels`, chorus / verse / bridge per bar, in `<data>/Mashup/<hash>.ccfl`;
  computed on the first search of a vocal track, ~30 s, or on analysis when the model runs).
  Each is tried at its start and every 8 bars in, and wherever a sung passage starts after a
  pause of a beat, at the full and half length, with the start moved by up to two bars to where
  a line starts. Quality = chorus prior (a chorus, or a verse running into one: "the chorus and
  one verse") × hook × real singing × not a loop × clean boundaries × 0.95 when it doesn't start
  with its section; a unit that would cut a line scores 0.
  The line data comes from A's vocal map (`analysis::vocals`, per 16th note, in
  `<data>/Mashup/<hash>.ccvm`):
  * singing = vocal stem level, minus leakage (a quieter copy of the melody or drum stem's
    spectrum) and minus bars the model calls instrumental;
  * line breaks = two quiet 16ths, quiet meaning a 9 dB dip under the surrounding beat and a
    half (reverb never lets the stem go silent); a line start needs singing on 3 of its first 4
    16ths (a sidechain pump ducks the one after each kick); a line may start up to two beats
    either side of the bar (pickups), and must end by a beat after the unit;
  * loop = the unit matches itself 1-2 bars later (vocal chops, or synths the separation put
    into the vocal stem); hook = its best near-copy elsewhere in the track.
  * The fast separation models leave whole synth leads in the vocal stem, which no picker can
    tell from singing; the finder offers to separate such a track in Best quality.
* **Slots** in B: section starts (and the middle of sections twice as long as the unit), and
  where B's own vocal comes back after two quiet bars; never an intro or outro. B's vocal stem is
  muted in the mashup and its arrangement was made to carry a vocal there, so a vocal swap is
  a strong spot (a hand-made Take It Back × Do You Still Care mashup enters exactly where Do You
  Still Care's breakdown vocal comes back). Placement = the bars' carry (drops and grooves need
  drums and bass, a breakdown doesn't) × the role fit (chorus → drop 1, groove 0.75, breakdown
  0.55; verse → groove or breakdown 1, drop 0.8) × entry (section start or B's vocal entry) ×
  room (0.75 + 0.25 × share of the slot where B sings itself) × position (not the first 18 %,
  not so late that B has nothing left to play).
* **Score** = fit × (0.15 + 0.85·placement) × (0.4 + 0.6·phrase) − tempo cost − 0.06 per
  semitone of key shift (listeners prefer vocals in their own key).
  * **Fit**: per bar, a directional interval-consonance table applied to the two chroma vectors
    (`CONSONANCE`), with bass weight 0.45; calibrated to 0..1 (random ≈ 0.5, own instrumental
    ≈ 0.65), combined as 0.7·mean + 0.3·weakest 4 bars, minus masking and (lightly, since only
    separation bleed of it is heard) B's own vocals.
  * `examples/mashup_eval.rs` measures the harmony: own pairing vs its ±1-2 semitone shifts AUC
    0.93, vs random placements 0.78. Chroma cosine scores 0.84 / 0.72, beat resolution doesn't
    help. `examples/mashup_places.rs` lists the units and where the best matches land;
    `examples/mashup_pair.rs` explains one pair part by part and where it ranks;
    `examples/vocal_probe.rs` prints a track's vocal map (`ALLIN1=1`: with the model's labels).
* **Search**: up to 6 units × the slots of every track with stems × key shifts, in parallel.
  Sections come from the analysis (`MashupFeatures::set_sections`), not from the feature file,
  so they are always current.
* **Re-ranking**: COCOLA (`analysis::cocola`).
  * The librosa front end (STFT, median-filter HPSS, Slaney mel, dB) is ported to Rust and matches
    librosa within 0.003 dB (`tests/cocola_ref.rs`), and the ONNX embeddings match PyTorch within 1e-5.
  * Two 5 s snippets per candidate: the vocals stretched and pitch-shifted with Signalsmith, as the
    mix plays them, and the instrumental (drums + bass + melody).
  * Fit = 0.6 rule + 0.4 learned (AUC vs random: 0.78 rule, 0.79 learned, 0.84 blend); placement
    and phrase then scale it as above.
* **Result**: vocals first, a `TransitionStyle::Mashup`: the instrumentals swap within the first
  bar, the outgoing vocals sing for the whole transition, the incoming vocals return on its last
  beat. Instrumental first, a `TransitionStyle::MashupIn`: the outgoing vocals stop, the incoming
  vocals sing for the whole transition over the outgoing instrumental, and the instrumentals swap
  in its last bar. Previews play on the studio decks through a separate
  `MixPlayer` (`LiveEngine::preview_player`).
