# Mashup finder: research and design

How published systems, commercial tools and DJs put the vocals of one track over the
instrumental of another, and what the finder should do as a result. Sources are papers, patents
and public documentation only (see [clean-room.md](clean-room.md)); no application code was read.

## What the current finder gets wrong

Measured on the library in October 2026 (`examples/mashup_places.rs`, `examples/vocal_probe.rs`):

- **It slides instead of picking units.** Every 4-bar offset of every vocal stretch is scored,
  so phrases start and end wherever the harmony happens to fit, often in the middle of a line.
  Lee et al. (2015) rejected sliding windows for exactly this reason: they "cannot guarantee a
  complete lead unit".
- **It cannot tell singing from leakage.** On stems from the fast `htdemucs` model, a lead synth
  can sit in the vocal stem at full level (Seek Bromance: the vocal stem is 20-25 dB louder
  than the melody stem in the drop). Level, spectral comparison with other stems and
  repetition all fail there. Nakatsuka et al. (2021) report the same: separation artefacts
  "cannot be removed by an amplitude threshold".
- **It guesses the hook from repetition alone.** Verses repeat their melody too. The
  All-In-One model ClubCraft already runs labels verse and chorus per frame, and on the tracks
  checked it marked them cleanly (Come Find Me: verse bars 39-60, chorus 69-84 and 119-148;
  Messy: verse 18-32, chorus 34-48 and 98-112), but only its section boundaries are kept.

## Published systems

| System | Unit | Vocal choice | Placement | Lesson |
|---|---|---|---|---|
| AutoMashUpper, Davies et al. 2014 ([TASLP](https://staff.aist.go.jp/m.goto/PAPER/IEEETASLP201412davies.pdf)) | phrases on downbeats, 2/4/8/16 bars preferred | none (whole songs) | every beat offset × transposition | listeners' main complaints: overlapping vocals, clashing bass; chord changes off the downbeat ruined a top pick |
| Lee et al. ISMIR 2015 ([paper](https://archives.ismir.net/ismir2015/paper/000302.pdf)) | 2ⁿ-bar units | whole lead units, penalised for inaudible beats | units on units | chroma × *harmonic change balance* (busy vocal ↔ static backing); rhythm and spectral balance useless for vocals |
| Huang et al. AAAI 2021 ([arXiv](https://arxiv.org/abs/2103.14208)) | 25-60 s clips | **labelled verse / chorus sections** | downbeat alignment | learned stem compatibility beats AutoMashUpper by far; global stretching drifts |
| Neural Loop Combiner 2020 ([arXiv](https://arxiv.org/abs/2008.02011)) | 1-bar loops | n/a | n/a | compatible ≠ similar; similarity-based matching ranked worst |
| Nakatsuka et al. 2021 ([IEEE Access](https://staff.aist.go.jp/m.goto/PAPER/IEEEACCESS202107nakatsuka.pdf)) | bars | **a vocal activity detector drops bars > 50 % non-vocal** | user-picked sections | amplitude can't separate artefacts from vocals |
| COCOLA 2025 ([arXiv](https://arxiv.org/abs/2404.16969)) | 5 s | n/a | n/a | learned coherence, r = 0.54 with listener scores (already used for re-ranking) |
| AutoMashup 2025 ([arXiv](https://arxiv.org/abs/2508.06516)) | All-In-One sections | **by label: chorus with chorus** | label matching, short sections repeated | compatibility is directional |
| Spotify, US11475867B2 ([patent](https://patents.google.com/patent/US11475867B2/en)) | sections ≥ 8 bars | **repetition (≈ 2 repeats ideal) × mean vocal probability per beat** | boundaries snapped to downbeats, then **moved to nearby downbeats where the vocal probability is low** | the one published recipe for choosing vocal segments and their boundaries |
| HKUST, Horner group 2023-26 ([list](https://cse.hkust.edu.hk/~horner/subpage/pubs.html)) | | | | listeners prefer meeting in the middle in tempo, vocals kept in their key; per-downbeat timing beats global stretch |

No commercial tool publicly documents choosing the vocal section or the spot automatically.
Mixed In Key suggests partners from key, BPM and energy; rekordbox detects vocal regions (and
reviewers note "vocal-like sounds" fool it); DJ.Studio leaves "aligning vocal phrases to bars
and drops" to the user; Rave.dj automates everything and is reviewed as "wildly random"
(sources in the research notes below).

## How DJs do it

- **Chorus or hook first**, at most chorus plus one verse; don't cut or repeat lines, edit the
  instrumental instead ([DJ Booth](https://djbooth.net/pro-audio/how-to-make-the-perfect-mashup/),
  [The Vocal Market](https://thevocalmarket.com/blogs/how-to/how-to-make-house-remix-cover-vocals)).
- **Chorus over the drop**, starting on its first downbeat; verse or bridge over the breakdown;
  no vocal in the drums-only intro or outro of an extended mix
  ([Vocal Market](https://thevocalmarket.com/blogs/how-to/how-to-make-house-remix-cover-vocals),
  [DJ.Studio](https://dj.studio/blog/acapellas-for-djs),
  [Digital DJ Tips](https://www.digitaldjtips.com/how-to-dj-with-acapellas/)).
- **Phrases on 8/16-bar lines**; the downbeat of the phrase goes on bar one, a pickup before it
  stays ([Digital DJ Tips](https://www.digitaldjtips.com/how-to-make-a-mashup/)).
- **Key** same, ±1 on the Camelot wheel or relative; shift 1-2 semitones at most. **Tempo**
  about ±6 % for sung vocals ([Mixed In Key](https://mixedinkey.com/workflows/how-to-use-the-camelot-wheel/),
  [Crossfader](https://wearecrossfader.co.uk/blog/dj-mashup-tutorial/)).
- **Room**: no busy lead or sustained pad in the vocal range under the vocal
  ([isolate.audio](https://isolate.audio/articles/ai-song-mashup)).

## Detectors

- **Singing.** Dedicated detectors reach 86-88 % on mixtures and fail on "pitch-fluctuating
  instruments" and synths ([Lee, Choi & Nam 2018](https://arxiv.org/abs/1806.01180)) — the very
  leakage problem. The reliable fix is a better separation: Mel-RoFormer vocals score
  11.0-11.6 dB SDR against 8.9-9.2 for htdemucs(_ft) and much less bleed
  ([BS/Mel-RoFormer](https://arxiv.org/html/2310.01809v1), [MVSEP](https://mvsep.com/en/algorithms)).
  A small singing detector worth testing later: FireRedVAD's audio event model (speech /
  singing / music, Apache-2.0, 2.2 MB ONNX, [repo](https://github.com/FireRedTeam/FireRedVAD));
  it has no published singing benchmark.
- **Chorus.** Supervised models find about 80 % of chorus frames (SpecTNT, [arXiv](https://arxiv.org/pdf/2205.14700));
  All-In-One reports no chorus figure, and a pop-trained taxonomy transfers poorly to EDM
  *sections* ([EDMFormer](https://arxiv.org/html/2603.08759v1)). Its chorus label is therefore a
  strong prior for the vocal hook, combined with repetition of the vocal itself.
- **Lines.** No off-the-shelf sung-line detector exists; boundaries are moved to downbeats where
  the vocal is quiet (Spotify patent), with pickups of up to a bar kept.

## Design

**Vocal units (track A).**

1. Per bar of A, keep All-In-One's functional label probabilities (cached next to the mashup
   features; computed once for tracks analysed before).
2. Candidate units are whole sung sections: chorus first, then verse / bridge. A unit starts at a
   section start (or 8 bars into a long section) and is 8 or 16 bars long.
3. Boundaries move to nearby downbeats (±1-2 bars) where the vocal is quiet: the half bar before
   the start and the bar after the end. Pickups up to a bar before the start are kept. Units
   that can't get clean boundaries are dropped, not cut.
4. Score = P(chorus) prior × repetition of the vocal (≈ 2+ near-copies) × share of really sung
   bars. Bars labelled inst / solo / break that still have vocal-stem energy count as leakage.
5. Fast-model stems: say so, and offer to separate the track in Best quality.

**Slots (track B).**

1. Section starts only: a chorus unit goes on a drop, a verse unit on a breakdown or groove;
   never the intro or outro.
2. Gates: tempo ±6 % (half / double time), key shift ≤ 2 semitones; B's own vocals in the slot
   near zero.
3. Score: bar-wise consonance at the unit's start only, harmonic change balance, room in the
   vocal band (masking), length fit between unit and section, position in the track.

**Ranking.** Units × slots × key shifts, best per pair of tracks, then COCOLA re-ranking.

**Evaluation.** A short list of tracks with the hook and good spots marked by ear is the only
way to know whether a change helps; `examples/mashup_places.rs` should check against it.
