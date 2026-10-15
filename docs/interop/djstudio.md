# DJ.Studio projects: format and behaviour

What ClubCraft's DJ.Studio importer (`crates/app/src/interop/djstudio.rs`) relies on. This is a
clean-room specification (see [`../clean-room.md`](../clean-room.md)): every statement names its
source, which is one of

- **[data]** an observation of DJ.Studio's own data files in `~/Music/DJ.Studio/Database`
  (read-only), with the file, field and an example value;
- **[export]** a measurement of DJ.Studio's rendered mixes in `~/Music/DJ.Studio/Exports` against
  the source audio files, with the numbers;
- **[doc]** DJ.Studio's public user documentation, with the URL.

Nothing here comes from DJ.Studio's program code. Where a behaviour could not be established, it
says so and states what the importer assumes instead (§7).

The measurement scripts (Python/numpy, run outside the repository) did, in short: decode the
export and the source tracks to mono 44.1 kHz WAV (`afconvert`), find where each source segment
lies in the export by normalised cross-correlation (150 Hz–6 kHz band, 1 s windows, every 4 s of
every track), and estimate the gain of each side of a transition by non-negative least squares on
power spectra (the outgoing and incoming source spectra, placed by the timing model, against the
export's spectrum). Gains of the two sides measured that way share the master limiter's gain
change, so their **ratio** is limiter-free; single-side gains include it.

Exports used (each matched to the project version saved closest before it, by `recordingDate` in
the version and the file's date, see §3.1). Transitions are numbered from 0: T0 joins the first and
the second track.

| Export file | Project version (`projects-table` key) | Transitions |
|---|---|---|
| `Harvard-Square-Mix (3).mp3` (5 Aug 2026 12:34) | `608fffc8…` = `c995b7fd…` (current; identical apart from record keys) | 11 |
| `New winter.mp3` (22 Mar 2026) | `510d0f97…` (the current version differs) | 12 |
| `Selected Deep Summer Copy(1).mp3` (30 May 2025) | `ce5e4bd0…` (current `71f683bb…` differs only in one track's `controlDefaults`) | 12 |
| `Selected Deep Summer(2).mp3` (20 May 2025) | `100fade6…` | 11 |
| `Indian Summer Mix 2.0(1).mp3` (30 Sep 2025) | `feba2b5b…` | 11 |

## 1. Files

- **[data]** Every table is a folder; every record is one JSON file named after its key, either
  directly in the folder (`projects-table/<key>`, `projects-meta-table/<key>`) or in one level of
  bucket folders (`audio-library-table/1d/file_08ab…`, `track-structures-table/26/…`).
  `mix-data-table/<project version>/<mix track id>` and
  `automation-data-table/<project version>/<owner>/<control>` are nested one level deeper.
- **[data]** One record (`projects-table/e1c2fa5b…`) has bytes after the JSON value
  (`Extra data: line 1 column 12256`). The importer reads the first JSON value of a file.
- **[data]** Keys of records that belong to a project are `"<project version>/<id>"`, e.g.
  `mixList[0].key = "c995b7fd-…/e8b6c8bf-…"`; the mix track's data is in
  `mix-data-table/c995b7fd-…/e8b6c8bf-…`.

## 2. Library (`audio-library-table`)

### 2.1 File and tempo line

- **[data]** `externalRec.fileLocationPath` and `externalRec.lastKnownFilePath` hold the audio
  file, e.g. `"/Users/benny/Downloads/Dom Dolla & Go Freek - Define (Club Mix).mp3"`. 141 of 144
  records point to an existing file; the others are two missing downloads and one
  `https://www.beatport.com/…` URL (`kind: "beatport-sdk"`).
- **[data]** `bpmLine` has two points `{time, beatNr, value}`, e.g. for "Define (Club Mix)"
  `(-0.2679 s, 0, 122.2012)` and `(296.7835 s, 604, 122.2012)`. The two points are the first and
  the last beat of the track's (straight) beat grid: beat 604 lies at
  `-0.2679 + 604 × 0.491803 = 296.78 s`, the same as in the project's beat map (§3.2).
- **[data]** `value` is **not** the beat spacing: here the spacing gives 122.000 BPM. For 140 of
  143 records `value = 60 · (N + 1) / (t_last − t_first)` with `N = beatNr_last − beatNr_first`
  (median deviation 0.0001 BPM); the three exceptions have hand-set grids
  (`beatDataSource: "manual"`/`"ai3"`, e.g. "Gravity": `value` 132 exactly). So `value` is
  0.13–0.31 % above the grid tempo (`(N + 1) / N`). It matters for timing (§4.3).

### 2.2 Cues

- **[data]** `cueData.systemCuePoints` holds the mix-in and the mix-out point as beat numbers
  (`beat`), e.g. `[{beat: 32, …}, {beat: 161, …}]`; some records have a third or fourth entry.
  The `time` field is not reliable: "Define (Club Mix)" stores `time: 94.65` for beat 161, which
  the beat grid puts at 78.91 s (the project's copy of the same cue stores 78.915). The importer
  computes cue times from the beat and the tempo line.
- **[data]** `cueData.hotCuePoints` is a list of 12 pads; an empty pad has `beat: -1`, a set one
  e.g. `{beat: 64, time: -1, type: 2, name: ""}` (time is -1 throughout). `memCuePoints` likewise
  (`{beat: 0, …}`). 372 hot cues and 32 memory cues are set in this database.
- The importer stores them as `Imported` cues: the first two system cues as Mix In / Mix Out
  markers, hot cues on their pad (pad index = list index), memory cues as markers. Cues the user
  made in ClubCraft are kept; a pad the user already uses is not overwritten.

### 2.3 Key numbers

- **[data]** `camelotKey` and `noteKey` are equal in all 144 records; `mikKey` uses the same
  numbers (28 of 32 records with a Mixed In Key value agree with `camelotKey`; the other 4 are
  its relative major/minor or a fifth away).
- **[data]** One record carries a key tag: `tag.key = "10A"` (B minor) with `camelotKey = 14`.
  Comparing `camelotKey` with ClubCraft's own key analysis of the same files (28 tracks): numbers
  0–11 match major keys with that pitch class (0 = C, 9 = A, 11 = B; ClubCraft's result is the
  same key or its relative minor), numbers 12–23 match minor keys whose tonic is
  `(n − 12 + 9) mod 12`, i.e. the relative minor of major key `n − 12` (19 → E minor in 5 of 5
  tracks, 22 → G minor in 3 of 4, 17 → D minor in 2 of 2, 12 → A minor). −1 = no key.
  Implemented in `camelot_int_to_key` (`crates/analysis/examples/eval.rs`).

## 3. Projects

### 3.1 Versions

- **[data]** `projects-meta-table/<key>` lists a project's saved versions in order
  (`versions: [{key, name, lastModified}, …]`), plus `name`, `isDeleted`,
  `lastAudioExportFilePath`. Example: "Harvard-Square-Mix" has 10 versions, the last one
  `c995b7fd…`. Each version is a full copy: `projects-table/<version>`, its
  `mix-data-table/<version>/…` and `automation-data-table/<version>/…`, with fresh ids for every
  record.
- The importer takes the last version that exists as the project's current state and its name
  from the meta record. Projects whose current version has no tracks (the "Unbenannt" drafts) are
  skipped. Without a meta table it would take the newest `lastModified` per `metaKey`.
- **[data]** A version records an export it made: `recordingDate`, `recordingLength`
  (1705.77 s for `Harvard-Square-Mix (3).mp3`, whose MP3 lasts 1705.82 s) and
  `recordingTrackList` (`[{time, artist, title, libraryKey}, …]`). The version that carries a
  recording is saved after it, so the version saved just before (`608fffc8…`, 10:31:57 UTC; the
  export was written at 10:34 UTC) is the one exported; here it equals the current one.

### 3.2 Tracks and beats

- **[data]** `mixList` is the track order: `[{key, libraryKey}, …]`, e.g. 12 entries for
  Harvard-Square-Mix. `libraryKey` names the library record (`file_…`, `jingle_…`).
- **[data]** The mix track's record (`mix-data-table/<version>/<id>`) has `mixMap`: one entry
  per beat the mix plays, `{time, duration, trackBeatIx}` (track seconds), ending with an entry of
  duration 0 at the end of the file. For the 12 Harvard tracks the beats are evenly spaced
  (straight line within ±1 ms except the first beat, e.g. 0.491803 s = 122.000 BPM for
  "Define") and agree with ClubCraft's own beat grids of the same files within 1.2–6 ms.
- **[data]** `cueData.systemCuePoints[0].beat` / `[1].beat` are the mix-in and mix-out beats of
  that track in this mix (e.g. "Alone": 0 and 160). They count positions in `mixMap` (equal to
  `trackBeatIx` unless the track has cuts, §3.4).
- **[data]** DJ.Studio's beat data (`track-structures-table/…`, `beatData[].nr`) numbers the
  first beat −1: `nr` = `trackBeatIx` − 1. Cues sit mostly on `trackBeatIx` 1, 33, 129, 161, 417,
  449 … (`nr` 0, 32, 128, …); the importer takes beat 1 as ClubCraft's bar anchor.
- **[data]** `trackOutputGain` (dB) per mix track, e.g. −3.33 for "Define"; `pitch` (0 in all
  observed records).
- **[export]** Track `i + 1` starts at its mix-in beat when track `i` reaches its mix-out beat:
  the cue sheet `Harvard-Square-Mix.cue` lists each track at the start of the transition into it
  (`INDEX 01 01:19:13` = 79.17 s for track 2; predicted 79.18 s), `recordingTrackList.time` at
  its end (94.86 s; predicted 94.86–94.89 s depending on the tempo model, §4).

### 3.3 Transitions

- **[data]** `autoEffects` holds one entry per transition, attached to its **outgoing** track
  (`mixTrackKey`): Harvard-Square-Mix has 11 for 12 tracks, the first track has one and the last
  none. Fields used: `duration` (beats: 16, 32, 64, 128; also 29, 36, 48 … in other projects),
  `locked`, `effects` (list of `{effectName, effectParams}`), `bpmLinePos` / `bpmLineLen` (always
  0.75 / 0.5 in the 1727 entries of this database), `isTrackEffect`, `isDisabled`.
- **[data]** The curves of a transition are in `automation-data-table/<version>/<effect id>_0/`
  for the outgoing ("primary") and `…_1/` for the incoming ("secondary") track, one file per
  control: `{controlName, points: [{time, value, flags, skew, slope}]}`, time in beats from the
  start of the transition. Example (Harvard T1, 16 beats): `_0/virtual-stems-vocals`
  `(0, 1), (2.73, 1), (16, 0.15), (16, 0)`. The same curves appear in the outgoing track's own
  lane (`automation-data-table/<version>/<mix track id>/virtual-stems-vocals`
  `(162.73, 1), (176, 0.15), (176, 0)`) shifted by its mix-out beat 160; for the incoming side
  shifted by its mix-in beat (Harvard T9 incoming bass: 122.04 in the transition = beat 155.04 =
  33 + 122.04 of "Sun In My Eyes").
- **[data]** Value ranges: `virtual-stems-*` 0…1, `volume` −31.6…+6 (dB), `eq-low` 0.04…1
  (linear). `skew`/`slope` are 0 in all 18 849 points that have them.
- **[doc]** Outside the transition frame's left and right handles the track concerned is muted
  or stopped (paraphrased; the documentation is summarised, not quoted, throughout).
  (https://help.dj.studio/en/articles/8106059-transition-editor)

### 3.4 Cuts

- **[data]** A cut shows as a jump in `trackBeatIx` along `mixMap`: "Sleepless (Extended Mix)"
  in Indian Summer Mix 2.0 goes from position 128 (`trackBeatIx` 128, 60.57 s) to position 129
  (`trackBeatIx` 145, 68.67 s), with `cutData: [{beat: 1009}]`.
- The importer plays such a track as two parts joined by a one-beat hard cut that lands on the
  beat after the jump. The meaning of `cutData.beat` was not established.

### 3.5 Sample-lane clips

- **[data]** `jingleList` entries with `isClip: true` are clips on the sample lanes, e.g.
  "Morning Comes (Extended) vocals HQ" (`type: 9`) with its own mix record
  (`mixDataKey`): `libraryKey` = the track "Morning Comes", `mixMap` over `trackBeatIx` 161–414
  (254 beats) plus the end marker at 749 (the end of the file), automation
  `virtual-stems-vocals-hq: 1`, and an anchor `linkMixRecordKey` (a mix
  track) / `linkMixRecordCue` (index into that track's `systemCuePoints`) /
  `linkMixRecordCueKey` (the cue's `cueKey`). The clip's first beat plays at the anchor cue.
- **[data]** In Harvard-Square-Mix the four clips are vocal stems of tracks of the mix. Three of
  them are anchored so that they play in step with the same track ("Morning Comes" from its beat
  161 anchored at its own beat 161, "Sun In My Eyes" from beat 29 anchored where it is at beat 29,
  "Make Me Feel" from beat 180 anchored at its mix-out); the fourth ("Running Back", beats
  277–314) is placed 32 beats off its own position. In Selected Deep Summer Copy all four clips
  are overlays of other tracks (e.g. "Daylight vocals", 96 beats, over another track's
  vocal-muted part, §5.4).
- **[export]** The clips play: in Harvard-Square-Mix "Morning Comes"' own vocal lane is 0 from
  beat 161 to the end; a first ClubCraft render without the clip (and without stems) was
  7.6–13.5 dB below the export in the 10 s after the transition into it (T4); with stems and the
  clip merged as below the difference there is −2.0 … +0.5 dB.
- The importer merges a clip into its own track where the two play in step (the vocal stem gain
  of that track, and of the transitions it plays in, is raised by the clip's gain); any part of a
  clip that does not play along with its own track (overlays on other tracks) is reported and not
  imported.

## 4. Timing

### 4.1 Start

- **[export]** The mix starts at the first track's first beat (`mixMap[0].time`, which is before
  the file start: −0.27 s for "Define"): with that origin every track's position in the export is
  a constant offset of 12–14 ms (Harvard, New winter; 67 ms Selected Deep Summer; 1 ms Indian
  Summer Mix 2.0), the same before and after the first transition. Starting at the mix-in cue
  instead would move the first transition by its beat count (15.5 s for "Define").

### 4.2 Between transitions

- **[export]** Outside transitions each track plays at its own speed: the offset of a track in
  the export stays constant to ±0.1 ms over its whole solo part (e.g. "Define" +14.1 ms in 19
  windows from 5 s to 77 s; "Only One" −465.3 ms in 54 windows over 212 s).

### 4.3 Tempo inside a transition

- **[doc]** A transition's tempo change can run over the whole frame ("Full") or start halfway
  through it ("Second Half" / "Half"); the BPM transition modes are Off, Free, Half and Full
  (https://help.dj.studio/en/articles/8106059-transition-editor,
  https://help.dj.studio/en/articles/8261956-bpm-controls-tips-and-faqs). `bpmLinePos` 0.75 with
  `bpmLineLen` 0.5 (§3.3) is a ramp over the second half; no other setting occurs in this
  database.
- **[export]** Measured as the change of the incoming track's offset against the outgoing
  track's (both at native speed before and after), every transition is **shorter** than L beats
  at the grid tempos held / ramped over the second half: by 15–88 ms (Harvard: 25.9, 15.4, 27.0,
  49.8, 19.9, 32.4, 40.8, 64.1, 40.4, 87.8, 75.9 ms), i.e. the tracks play 0.11–0.28 % fast
  inside the window. Inside the window both tracks drift at about −1.2 to −2.6 ms/s (cross-
  correlation of each source in 0.5 s windows), from its start, in both halves; Harvard T9
  (128 beats, 133 → 133 BPM, so no tempo change at all) is 87.8 ms shorter.
- Other ramp shapes do not explain it (spread of the needed speed factor over the 11 Harvard
  transitions: 0.51 ‰ with the second-half ramp, 6–20 ‰ with a full-length ramp, a jump, or
  holding either tempo).
- **[export]** Running the transition at the tracks' tempo-lane values (§2.1, `value`), held over
  the first half and ramped (linearly in beats) to the incoming track's value over the second
  half, predicts these durations with an rms error of 9.7 ms over 54 transitions of five exports
  (grid tempos: rms 41.9 ms, mean −37.4 ms, i.e. always too long). Over the Harvard mix the
  grid-tempo model would place the last track 465 ms late.
- The importer stores the two values as `Transition::tempo` (`[outgoing, incoming]`), which
  ClubCraft's timeline uses for the transition's tempo segments; both tracks then run at
  `value / grid tempo` inside the window, as measured.

### 4.4 Incoming track after a tempo change

- **[export]** The remaining error is proportional to the tempo change: after a transition from
  tempo `a` to `c` the incoming track sits `D · (c / a − 1)` later than beat-exact, with
  D = 0.256 s (least squares over the same 54 transitions; rms 9.7 → 5.4 ms). Apart from a few
  ± pairs (one transition +11…+14 ms, the next −11…−12 ms, around one particular file each:
  "Sun In My Eyes", "Gravity", one track of Selected Deep Summer) the residual is within ±1.6 ms.
  Those pairs are a constant offset of one file (about 576 samples ≈ 13 ms, one MP3 granule), so
  they come from decoding, not from the mix.
- The importer moves the incoming track's mix-in point by that lag.

### 4.5 Not explained

- **[export]** Indian Summer Mix 2.0, T6 ("I Love You" → "Cages", 128 beats from
  beat 272 of a 325-beat track, so the outgoing track ends inside the window) is 606 ms longer
  than predicted. Transitions longer than the remaining outgoing track are imported like the
  others.

## 5. Transition curves

ClubCraft lanes: volume gain = value², EQ knob gain = (2·knob)² below flat, stem gain linear,
x in beats from the transition start.

### 5.1 Volume crossfade (`AE_CrossFade`)

- **[data]** `effectParams: {level, primaryLevel, range}`, e.g. `(-20, -20, 0.5)` in 978 and
  `(-20, -20, 1)` in 147 entries; also `level` 6, 0, −8.36, −11.85 … with `primaryLevel` −20.
- **[doc]** The Crossfade preset lowers the value for the first track and raises it for the second
  with equal strength (transition editor article above).
- **[export]** With `range` 0.5 the incoming track fades in over the **first half** and the
  outgoing out over the **second half**; with `range` 1 both over the whole window. Limiter-free
  ratio incoming/outgoing, median of 10 transitions with (−20, −20, 0.5), against
  amplitude-linear ramps from −20 dB (0.1) to full and back:

  | x (fraction of window) | 0.02 | 0.11 | 0.23 | 0.36 | 0.48 | 0.58 | 0.70 | 0.83 | 0.95 |
  |---|---|---|---|---|---|---|---|---|---|
  | measured in − out (dB) | −16.8 | −13.6 | −6.6 | −2.1 | −1.9 | +1.0 | +3.6 | +7.2 | +13.2 |
  | model in − out (dB) | −18.3 | −11.5 | −5.9 | −2.4 | −0.3 | +1.4 | +4.0 | +8.2 | +16.9 |

  The single-side gains (relative to the solo level, so including the limiter) fit the same
  model with the limiter holding the overlap 2.5–4.5 dB down, e.g. incoming at x = 0.23:
  −7.5 dB measured, −5.8 dB model − 2.5 dB. With level −11.85 dB (Indian Summer Mix 2.0, T8)
  the incoming side starts at −10.2 dB and reaches −1.5 dB at x = 0.45 (model −9.6 … −0.7 dB),
  confirming that `level` is the incoming start level in dB and `primaryLevel` the outgoing end
  level. For `range` 1 (4 transitions) the ratio crosses 0 dB at x ≈ 0.6 and the outgoing side
  stays 2–5 dB above the linear model in the second half: it fades slower. Rendering Selected
  Deep Summer Copy (no limiting in that export, §6) with a quarter-cosine fade-out for `range` 1
  brings its range-1 transition (T1) from −1.17 dB to −0.64 dB mean difference inside the window;
  for `range` 0.5 the linear fade-out fits better (T5: mean |difference| 0.44 dB linear, 0.61 dB
  cosine; T4, T6, T9: equal within 0.05 dB).
- The importer: incoming volume ramps (linear in amplitude) from `level` dB at 0 to full at
  `range` × L, outgoing from full at (1 − `range`) × L to `primaryLevel` dB at L, linearly when
  `range` ≤ 0.5 and along a quarter cosine (in amplitude) when the fades overlap. Levels above
  0 dB are clamped to full. No crossfade effect = both full for the whole window (**[export]**
  Harvard T0, a stems-only transition: the incoming side is within 1.5 dB of its solo level from
  the first beat).

### 5.2 Bass (`AE_Bass_Swap`, `AE_Bass_CrossFade`)

- **[data]** `AE_Bass_Swap {level: 0.04, primaryLevel: 0.04, beats: 0, effectCenter: 0.5}`
  (`effectCenter` also 0.46, 0.6, 0.67 …; `beats` always 0); `AE_Bass_CrossFade {level: 0.04,
  primaryLevel: 0.04, range: 1}`.
- **[doc]** The bass swap sits in the middle of the transition by default and can be dragged; a
  gap of one, two or four beats can be left out at the swap (transition editor article).
- **[export]** Low band (30–180 Hz) of 10 transitions with a centred bass swap: the incoming low
  band is 20–70 dB down until x = 0.48 and at its solo level from x = 0.52; the outgoing one falls
  to −23 … −35 dB at x = 0.52 (gain 0.04 = −28 dB, plus its volume fade). With
  `AE_Bass_CrossFade` the incoming low band rises gradually (−10 dB at x = 0.35, −4.8 at 0.55,
  −1.5 at 0.85) and the outgoing one falls (−4.5 at 0.35, −7.9 at 0.55, −12 at 0.65).
- The importer: low EQ swap at `effectCenter` × L (the `beats` gap centred on it), the killed side
  at gain `level` / `primaryLevel`; the bass crossfade as amplitude-linear ramps like §5.1.

### 5.3 Volume swap (`AE_Swap`)

- **[data]** `{level: -50, primaryLevel: -50, beats: 0, effectCenter: 0.46 | 0.47}` (36 entries,
  only in the Indian Summer projects).
- **[export]** Indian Summer Mix 2.0, T1: the incoming track is 28–60 dB down until beat 15 of
  32 (x = 0.47) and full from beat 17; the outgoing track is 5–14 dB down after the swap, not
  −50 dB. Not resolved (the outgoing estimate may be unreliable there).
- The importer: a step at `effectCenter` for both sides, to `level` / `primaryLevel` dB.

### 5.4 Stems

- **[doc]** Stem separation splits drums, bass, melody and vocals, and each stem's transition can
  be set on its own; each track has a mute button per stem.
  (https://help.dj.studio/en/articles/8106059-transition-editor,
  https://help.dj.studio/en/articles/9112447-stem-separation)
- **[data]** One effect per stem: `AE_Stems{Drums,Bass,Melody,Vocals}_{Full, Full_Manual,
  Crossfade, Crossfade_Manual, Swap}` with `primaryEnabled` (outgoing), `secondaryEnabled`
  (incoming), `effectCenter`, `primaryEffectCenter`, `customSwapPoint`,
  `primaryCustomSwapPoint`. Hand-drawn curves are the side's `virtual-stems-*` automation (§3.3).
- **[export]** A stem switched off for a side is silent for the whole window, whatever its
  points: Harvard T0 (outgoing drums, bass, melody off, no points): the outgoing low band drops
  to −24 … −43 dB from the first beat while its mid band (vocals) stays at −4 … −8 dB; Harvard
  T1 (outgoing bass and melody off, with points ramping 1 → 0 over 16 beats): the outgoing low
  band is −5 … −8 dB from the first beat (kick drums only), not following the ramp.
- **[export]** Inside the window the transition's points win over the track's own lane: Harvard
  T3, outgoing stems "full until beat 64" in the transition but 0 from beat 456 (= 40 beats in) in
  the track's lane: the outgoing mid band is still −5 dB at beat 41–45 and fades with the volume
  crossfade.
- **[export]** Track effects (`autoEffects` with `isTrackEffect: true`): Selected Deep Summer
  Copy, "AE_StemsVocals_Full" with `primaryEnabled: false`, `effectOffset` 353, `duration` 76 on
  a track: the export's mid band (300–3000 Hz) relative to the source is −2.9 dB before track beat
  353 and after 429 and −4.4 … −10.8 dB in between. So the offset is in track beats and a
  disabled stem is muted for `duration` beats.
- The importer: switched off → lane 0; points → lane from the points; no points and a `_Full`
  effect → no lane (the track's own stem lanes apply, ClubCraft's rule for stems a transition
  leaves alone). Track-level `virtual-stems-*` curves become the track's stem lanes (track beats →
  seconds).
- Not established: the parametric forms. The importer assumes `_Full` = incoming stem from
  `effectCenter`, outgoing until `primaryEffectCenter` (0 and 1 = the whole window);
  `_Crossfade` = equal-power crossfade of the stem between `effectCenter` and
  `primaryEffectCenter` (the shape is measured, below; the use of the two centres is assumed);
  `_Swap` = steps at those points. `customSwapPoint`, `primaryCustomSwapPoint` and the
  `virtual-stems-on` control are not used.
- **[export]** Stem crossfades are equal-power (sine / cosine of a quarter turn), not linear:
  rendering Selected Deep Summer Copy with linear stem crossfades leaves its stem-crossfade
  transitions T3, T7, T11 at −2.16, −2.59, −0.45 dB mean difference inside the window (too quiet
  in the middle, where two linear halves sum to −3 dB); with equal-power crossfades −0.32, −1.27,
  +0.47 dB. **[doc]** Each stem lane has a handle per track, the outgoing one at the right (end)
  edge and the incoming one at the left (start) edge; dragging it changes the volume at once; the
  Crossfade preset fades all stems, the drums a little longer. **[data]** In such transitions
  `primaryEffectCenter` is 1 for drums and 0.75 for the other stems (Selected Deep Summer Copy
  T3).

### 5.5 Hand-drawn volume and EQ

- **[data]** A side's `volume` points are dB (e.g. `(0, −8.94), (15.26, −8.94), (25, −8.94)`),
  `eq-low` points linear gain (`(0, 0.04), (10.7, 1), (32, 1)`). The importer uses them when an
  `AE_ManualVolume` effect is present (volume) or always (EQ), replacing the generated lane of
  that side.

## 6. Levels

- **[data]** `trackOutputGain` (dB) of a mix track equals `20·log10(autoGain)` of its library
  record in 156 of 163 mix tracks (all 12 in Harvard-Square-Mix; e.g. −3.33 dB and
  `autoGain` 0.6815).
- **[data]** Every project has a master limiter: `controlDefaults` `production_effects_limiter_
  enabled 1`, `…_ceiling −0.2`, `…_gain −2`, some also `…_envelope_mode 1`.
- **[data]** In the 2025 versions (Selected Deep Summer, Indian Summer Mix) `trackOutputGain` is 0
  for every track; the 2026 versions carry the library's `autoGain`.
- **[export]** Selected Deep Summer Copy(1): every track plays at exactly −3.0 dB relative to its
  source in all bands (40–150 Hz, 300–3000 Hz, 4–12 kHz) throughout its solo part, so no
  normalisation and no limiting. A ClubCraft render with a −3 dB master gain is within −0.25 dB
  of it overall.
- **[export]** Harvard-Square-Mix (3) is not `trackOutputGain` × source: in loud passages it is
  0.9 dB below the source in 6 of 12 tracks whatever their `trackOutputGain` (−1.2 … −4.1 dB), and
  quiet passages get 0–4 dB more gain than loud ones: a limiter driven well above its threshold.
  Overall the export is 2.0 dB louder (RMS) than a ClubCraft render with `trackOutputGain` and
  ClubCraft's default limiter (ceiling −1 dB); with +3 dB into the limiter and a −0.2 dB ceiling
  the difference is +0.36 dB. The overlap of a transition is held 2–4 dB lower than ClubCraft's
  limiter holds it (§8).
- The importer sets each track's gain so that ClubCraft's player applies `trackOutputGain` in
  total (`gain_db = trackOutputGain − ClubCraft's loudness gain` at import time; a track analysed
  only after the import gets ClubCraft's loudness gain on top until the project is imported
  again). The master stage is a render setting, not part of the mix, and is not changed.

## 7. Not established / not imported

- Sample-lane clips that do not play along with their own track (overlays, §3.5), and clips of
  tracks that are not in the mix.
- `cutData`; variable beat maps (Winter Mix, one track deviates 37 ms from a straight line) play
  on a straight grid.
- Tempo automation (`bpm` lanes: empty in all observed projects), fixed mix tempo
  (`mixBpmFixedMode`: false everywhere; imported as `TempoMode::Fixed(mixBpmStart)`), tempo
  ramps other than "second half".
- The parametric stem effects (§5.4), `AE_Swap` for the outgoing side (§5.3), level values above
  0 dB in `AE_CrossFade`, filter / echo effects (none occur in this database).
- DJ.Studio's master limiter (§6) and its time-stretching quality.
- Indian Summer Mix 2.0 T6 timing (§4.5).

## 8. Validation

Ignored tests in `crates/app/src/interop/djstudio.rs`: `real_database` (imports the database
into the library at `CLUBCRAFT_HOME`), `import_one_version` (one saved version, for exports made
before later edits), `separate_mix_tracks` (stems, fast model) and `render_imported_mix`
(renders to WAV; `DJS_MASTER_DB`, `DJS_CEILING_DB` set the master stage). All 12 projects of
the database import (141 tracks, 681 cues). Renders were compared with the exports by
onset-envelope cross-correlation (20 s windows every 100 s, ±250 ms searched) and RMS loudness
in 0.5 s windows from 10 s before to 10 s after each transition, after removing the median level
difference. All tracks had ClubCraft stems.

| | Harvard-Square-Mix (3) | New winter (version `510d0f97…`) | Selected Deep Summer Copy(1) |
|---|---|---|---|
| timing offset (median) | −11.1 ms | −7.9 ms | −51.9 ms |
| timing spread over the mix | 4.7 ms (17 windows, 50–1650 s) | 4.7 ms (18 windows) | 2.3 ms (18 windows) |
| drift (fit) | 1.2 ms / 1000 s | 0.7 ms / 1000 s | 1.0 ms / 1000 s |
| level (RMS) render − export | −2.0 dB (default master) | −1.9 dB | −0.25 dB (−3 dB master) |
| loudness envelope, mean abs difference per transition | 0.70 – 2.11 dB | 0.66 – 2.06 dB | 0.13 – 1.02 dB |

The constant offsets include the MP3 encoder and decoder delays (§4.1). The cue-sheet starts of
New winter (`New winter.cue`, 1/75 s resolution) are 1–14 ms before the render's.

Remaining loudness differences:

- Harvard / New winter: inside crossfades the render's level rises 1–4 dB more relative to the
  tracks around it than the export's (e.g. Harvard T6 and T10: export −1.3 / +0.7 dB, render
  +2.6 / +3.6 dB). The same crossfade type in Selected Deep Summer Copy, whose export has no
  limiting, agrees within 0.3 dB (T5, T9), so the difference is DJ.Studio's master limiter (§6).
- Track levels between transitions differ by up to ±2 dB in Harvard / New winter (the limiter's
  level-dependent gain, §6).
- Selected Deep Summer Copy T10 (+2.25 dB inside; relative to the tracks around it the export's
  middle is 2.7 dB lower than the render's): parametric
  stem effects with `primaryEffectCenter` 0.5 and a degenerate melody crossfade (§5.4, not
  established); T7 (−1.27 dB) and T1 (−0.64 dB) likewise involve parametric stem effects.
- Clips that are not merged (§3.5).
