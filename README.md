<p align="center">
  <img alt="ClubCraft" src="docs/images/icon.png" width="128">
</p>

<h1 align="center">ClubCraft</h1>

<p align="center">
  <b>DJ software with studio-grade stem separation: plan mixes like DJ.Studio, play them like VirtualDJ, rebuilt in Rust.</b><br>
  Separate tracks into vocals, melody, bass and drums, let the analysis find the beats, keys, drops and mix points,<br>
  design beat-matched transitions in the studio, then take the same mix out on the live decks.
</p>

<p align="center">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-b7410e?style=flat-square&logo=rust">
  <img alt="macOS · Windows" src="https://img.shields.io/badge/macOS%20%C2%B7%20Windows-native-2f7bf5?style=flat-square">
  <img alt="Stems: HTDemucs + Mel-Band RoFormer" src="https://img.shields.io/badge/stems-HTDemucs%20%2B%20RoFormer-5ac87a?style=flat-square">
  <img alt="License: MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-3a3a3a?style=flat-square">
  <img alt="Status: early alpha" src="https://img.shields.io/badge/status-early%20alpha-d69e2e?style=flat-square">
</p>

<br>

<p align="center">
  <img src="docs/images/transition.jpg" alt="The ClubCraft studio: twelve tracks on two beat-matched lanes with stem-coloured waveforms and Camelot key badges, and the transition editor below showing the vocals, melody, bass and drums curves of the outgoing and incoming track over eight bars" width="100%">
  <br>
  <sub>A 28-minute mix in the studio, with the stem blend between two tracks open in the transition editor.</sub>
</p>

<p align="center">
  <a href="#features">Features</a> ·
  <a href="#everything-in-the-box">Everything in the box</a> ·
  <a href="#stems-without-compromise">Stems</a> ·
  <a href="#analysis-that-knows-your-music">Analysis</a> ·
  <a href="#under-the-hood">Under the hood</a> ·
  <a href="#get-started">Get started</a> ·
  <a href="#downloads">Downloads</a>
</p>

<br>

<table>
  <tr>
    <td width="25%" valign="top">
      <h3>🎚️ Studio-grade stems</h3>
      Meta's HTDemucs and Mel-Band RoFormer run natively on the Apple GPU (CoreML) or any DX12 GPU (DirectML). On real tracks they match the PyTorch reference to more than 100 dB.
    </td>
    <td width="25%" valign="top">
      <h3>🧠 Cues that find themselves</h3>
      Every track gets a beat grid, key, energy, sections and eight hot cues: Start, Mix In, Vocal, Build, Drop, Break and Mix Out. You don't place a single marker.
    </td>
    <td width="25%" valign="top">
      <h3>🔁 Studio ↔ Live</h3>
      One library and one analysis. A mix designed in the studio plays on the live decks with the same mix points, and its auto cues are already on the pads.
    </td>
    <td width="25%" valign="top">
      <h3>⚡ Native and light</h3>
      A lock-free Rust audio engine: about 1.5 % CPU per deck with keylock, stems and FX. Mixes stream track by track, so a 28-minute export stays under 300 MB of memory.
    </td>
  </tr>
</table>

<br>

## Features

Every screenshot here is the real app at work on a mix imported from DJ.Studio. ClubCraft captures them itself through its developer control channel.

<table>
  <tr>
    <td width="50%" valign="top">
      <img src="docs/images/studio.jpg" alt="The studio timeline: two deck lanes with mixer headers (fader, solo, mute, EQ and filter knobs), stem-coloured clips with key badges, transition markers on their own strip and the tempo lane gliding between 122 and 130 BPM" width="100%">
      <br>
      <sub>Two lanes, each with its own mixer, and the tempo gliding from 122 to 130 BPM across the mix.</sub>
      <h3>Design the whole set</h3>
      Drop tracks in and <b>✨ Harmonize</b> orders them by Camelot key, tempo and energy arc, using simulated annealing over the whole set. Mix-in and mix-out points come from the analysed structure, and transition lengths fit the intro and outro of each pair.
      <br><br>
      Slide tracks and transitions by bars, collapse lanes, solo or mute them, shape a lane with its EQ and filter, and render the result to WAV with a cue sheet, faster than real time.
    </td>
    <td width="50%" valign="top">
      <img src="docs/images/live.jpg" alt="The live view: two scrolling stem-coloured waveforms in phase, deck A playing Define (Club Mix) and deck B synced to 122 BPM, both with eight auto-generated hot cues (Start, Mix In, Vocal, Break, Drop, Break, Drop, Mix Out), the mixer in the middle and the browser below" width="100%">
      <br>
      <sub>Two decks phase-locked at 122 BPM, every hot cue placed by the analysis.</sub>
      <h3>Then play it live</h3>
      VirtualDJ-style decks: CDJ cue logic, eight hot cues, loops and loop rolls, beat jump, slip, keylock, key shift and match key, sync with a phase meter, stem knobs and pads (acapella, instrumental, drums only), three-band EQ with kill, filter and beat-synced FX.
      <br><br>
      <b>Play live ▶</b> sends a studio mix to the decks and automix plays it at its planned mix points. Or drag tracks into the automix queue.
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <img src="docs/images/library.jpg" alt="The library sorted by BPM: tracks with stems marked by a green dot, BPM, Camelot key, energy, length and the number of generated cues" width="100%">
      <br>
      <sub>The library: BPM, key, energy and cues for every analysed track.</sub>
      <h3>A library that analyses itself</h3>
      Drop a folder in and every track is analysed within about a second. Stems are separated in the background, one job at a time, so your Mac stays responsive. Sort and search by artist, title, BPM, key or energy, and drag tracks onto a deck or into a mix.
      <br><br>
      Imports your <b>DJ.Studio</b> projects, with their mix points, transitions and cues, and exports beat grids, keys, hot cues and automix points to <b>VirtualDJ</b>.
    </td>
    <td width="50%" valign="top">
      <img src="docs/images/home.jpg" alt="The home page: a New mix card (start empty, from selected tracks, a playlist or auto-built from the library), library statistics with quick actions, and recent mixes shown as cards with a strip of key colours" width="100%">
      <br>
      <sub>The home page: new mix, quick actions and recent mixes with their key colours.</sub>
      <h3>Start anywhere</h3>
      Start a mix from scratch, from selected tracks, from a playlist, or let ClubCraft auto-build one from analysed tracks in a BPM range. Recent mixes show their length, track count and harmonic journey at a glance.
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <img src="docs/images/settings.jpg" alt="The Settings window on the Decks tab: cue button mode, hot cue behaviour, where a loaded track is parked, default pitch range, keylock, quantize and sync on load, and what to reset on load" width="100%">
      <br>
      <sub>Settings ▸ Decks, one of nine tabs (⌘, from the menu bar).</sub>
      <h3>Behaves the way you play</h3>
      Cue modes (CDJ, hold, cue-up-play), hot cue behaviour, load position, pitch ranges, crossfader and fader curves, auto gain and limiter, key notation (Camelot, Open Key or musical), waveform colours, BPM range, stem quality, default transitions and export formats.
    </td>
    <td width="50%" valign="top">
      <h3>Transitions you can read</h3>
      Stem blend, Bass swap, Crossfade, Vocal over, Filter out, Echo out and Cut, from 2 to 32 bars. The editor lines up the outgoing and incoming track on a bar ruler, with one clearly labelled band per stem, EQ band, filter or echo. Draw a curve with the mouse and it plays back sample-accurately in the preview, the export and on the live decks.
      <br><br>
      Tracks without stems yet automatically fall back to a bass-swap blend, so a transition never just piles two tracks on top of each other.
      <h3>Keyboard first</h3>
      <b>Studio:</b> Space play · ← → bar (⇧ 8 bars) · [ ] transitions · F follow · + − zoom · ⌘S save.<br>
      <b>Live:</b> Z X C and M , . cue/play/sync · 1–8 hot cues (⇧ deck B) · ← → crossfader.<br>
      MIDI controllers map with MIDI learn.
    </td>
  </tr>
</table>

<br>

## Everything in the box

<table>
  <tr>
    <td width="33%" valign="top">
      <h4>🎚️ Stems</h4>
      Fast (htdemucs), High (htdemucs_ft, four fine-tuned models) and Best (Mel-Band RoFormer vocals + htdemucs_ft) · CoreML GPU on macOS, DirectML on Windows, ONNX Runtime CPU fallback · progressive separation while a track plays · 8-channel FLAC stem cache · residual playback, so unity stems are bit-exact with the original
    </td>
    <td width="33%" valign="top">
      <h4>🧠 Analysis</h4>
      Constant-tempo beat grid fitted to about 1 ms · downbeat and phrase alignment · key detection (EDM-tuned profiles) · EBU R128 loudness and auto gain · energy 1–10 · sections (intro, verse, build, drop, break, outro) · 8 hot cues and phrase markers · optional All-In-One structure model
    </td>
    <td width="33%" valign="top">
      <h4>🗺️ Studio</h4>
      Harmonic ordering (key, BPM, energy arc) · automatic mix points · beat-locked timeline with a tempo glide or a fixed BPM · per-lane gain, solo, mute, EQ and filter · 7 transition styles plus custom curves · lazy, memory-bounded preview · WAV export (16/24/32-bit) with a cue sheet
    </td>
  </tr>
  <tr>
    <td width="33%" valign="top">
      <h4>🎛️ Decks</h4>
      Two live decks (plus two for the studio preview) · CDJ, hold and cue-up-play cue modes · 8 hot cues · loops from 1/32 to 32 beats, loop roll, slip · beat jump · keylock (Signalsmith Stretch) and key shift · sync with a phase meter · jog and scratch · brake and spinback
    </td>
    <td width="33%" valign="top">
      <h4>🔊 Mixer and FX</h4>
      3-band isolator EQ with kill · resonant one-knob filter · crossfader curves · headphone cue on outputs 3/4 or a split cable · master limiter · recording · echo, ping-pong, reverb, flanger, filter LFO, gate, roll, bitcrusher, brake · per-stem echo-out
    </td>
    <td width="33%" valign="top">
      <h4>🔗 Interop</h4>
      DJ.Studio project and cue import (read-only) · VirtualDJ <code>database.xml</code> export and import (merges safely and keeps a backup) · MIDI learn mappings in JSON · native macOS menu bar
    </td>
  </tr>
</table>

<br>

## Stems without compromise

Stem quality is the heart of ClubCraft. The separation runs the original networks: no lighter re-trained model, no cloud service.

- **Faithful to the reference.** The neural cores of HTDemucs and Mel-Band RoFormer are exported once (`tools/model-export`). The STFT, segmenting, overlap-add, shift trick and bag-of-models weighting are reimplemented in Rust to match `demucs.apply` exactly. On real house tracks, full-pipeline output matches Python Demucs at **110–128 dB** on CoreML, so any difference is inaudible.
- **Best quality.** Mel-Band RoFormer separates the vocals, about 2.7 dB better than htdemucs_ft on public benchmarks. htdemucs_ft then splits the remaining instrumental into drums and bass, and melody is what's left. The four stems still sum back to the mix exactly.
- **Speed** on an M1 Pro, for a 5-minute track:

  | Quality | Models | Time |
  |---|---|---|
  | Fast | htdemucs | ≈ 17 s |
  | High | htdemucs_ft (4 models) | ≈ 80 s |
  | Best | Mel-Band RoFormer + htdemucs_ft | ≈ 3.5 min |

  A track loaded on a deck before it was separated gets usable stems under the playhead in about a second.
- **Light on memory.** Stems are held as 16-bit samples with one scale per stem, heavy jobs never overlap, and idle models are released. A full Best separation peaks at about 2 GB.

## Analysis that knows your music

The analysis was tuned and checked against DJ.Studio's and Mixed In Key's results for 137 real tracks (`cargo run --release -p clubcraft-analysis --example eval`).

| | Result |
|---|---|
| BPM within 0.05 | 99.3 % |
| Beat phase within 15 ms | 98.5 % |
| Downbeat (bar phase) | 96.3 % |
| Key exact or relative | 86.9 % |
| Time per track (without the deep structure model) | ≈ 0.1–0.5 s |

The cue generator places Mix In at least 32 beats before the first drop or vocal, on a phrase line. Mix Out goes on the outro, or 32 to 64 beats before the music ends. Drops land on their downbeat. The studio aligns each incoming Mix In with the outgoing Mix Out, which is why mixes line up without any manual work.

## Under the hood

- **Six crates:** `core` (data model, SQLite library, planner, beat-locked mix timeline), `audio` (decoding, tags, resampling), `stems`, `analysis`, `engine` (real-time audio, mix conductor, renderer, MIDI) and `app` (egui).
- **Real-time engine:** a lock-free command queue in, a triple-buffered state snapshot out, and nothing allocated or locked on the audio thread. Stems are mixed before the stretcher, so each deck needs only one time-stretcher.
- **One conductor:** the studio preview, live automix and offline render all run the same code path, so what you hear in the preview is exactly what you export.
- **Exact timing:** tempo is piecewise linear in mix beats and integrated analytically, which keeps overlapping tracks beat-locked to within a millisecond.
- **Clean-room:** ClubCraft is independent. Behaviour and file formats of DJ.Studio and VirtualDJ were only studied for interoperability (importing projects, exporting cues). No code, models or assets from either product are used.
- **Tested:** 74 unit and integration tests, plus evaluations against Python Demucs and a 137-track analysis corpus.

## Get started

```sh
git clone https://github.com/beneaze/ClubCraft
cd ClubCraft
cargo run --release -p clubcraft          # the app
cargo test -p clubcraft-core -p clubcraft-engine
tools/bundle-macos.sh                     # target/ClubCraft.app, models included
```

The separation models (≈ 3 GB) aren't in git. Export them once from the official checkpoints with the script in [`tools/model-export`](tools/model-export); [`models/README.md`](models/README.md) has the commands. On Windows, use `.\tools\package-windows.ps1`.

Your library, stems, waveforms, exports and recordings live in `~/Music/ClubCraft` (`CLUBCRAFT_HOME` changes that).

> [!IMPORTANT]
> **Status:** ClubCraft is in early alpha. The studio, live decks, stems and analysis work end to end and are tested, but expect rough edges. Back up your music folders, and keep VirtualDJ closed while ClubCraft writes to its database.

## Downloads

**[Latest release: v0.1](../../releases/tag/v0.1)**

| Platform | File | Notes |
|---|---|---|
| macOS 13+ (Apple Silicon) | `ClubCraft-0.1-macos-arm64.dmg` | All CoreML stem models bundled. Ad-hoc signed: right-click › Open the first time |
| Windows 10/11 (x64) | `ClubCraft-0.1-windows-x64.zip` | `ClubCraft.exe`, `DirectML.dll` and the ONNX stem models |

Development builds: `tools/bundle-macos.sh` (macOS), and the `ClubCraft-windows-x64` artifact of every [CI run](../../actions) (Windows, without models).

## License

ClubCraft is dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE). The Demucs weights are MIT (Meta Platforms). The Mel-Band RoFormer checkpoint (Kimberley Jensen) and ZFTurbo's implementation are MIT, and so is Signalsmith Stretch. DJ.Studio and VirtualDJ are trademarks of their respective owners, and ClubCraft is not affiliated with either.
