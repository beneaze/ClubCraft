//! A DJ deck: pure DSP, no I/O. Usable offline (render, tests) and on the audio thread.
//!
//! ## Signal path
//! source (original + stem residuals) → [stem echo] → varispeed (Hermite) **or** time stretcher
//! (Signalsmith Stretch, for keylock / key shift) → declick crossfades → output.
//! Stems are mixed *before* stretching, so one stretcher per deck is enough.
//!
//! ## Positions and latency
//! [`Deck::position`] is the **heard** position. In keylock mode the stretcher has a fixed
//! latency (input + output latency, ~120 ms); the deck feeds the stretcher *ahead* of the playhead
//! by exactly that latency, so keylocked and varispeed decks agree on what is heard, sync works
//! across both, and jumps (hot cues, beat jumps) are immediate: on a discontinuity the stretcher is
//! reset, primed with pre-roll and pre-run, and the old audio is crossfaded out.
//!
//! All buffers are preallocated in [`Deck::new`]; no method allocates or frees except
//! [`Deck::load`] / [`Deck::unload`], which *return* the old track so the caller can drop it
//! elsewhere.

use crate::dsp::{hermite, Smoother, MAX_BLOCK};
use crate::state::{CueMode, DeckState, HotCue, HotCueMode, LoopState, PitchRange, HOT_CUES};
use crate::track::LoadedTrack;
use clubcraft_core::{BeatGrid, CueKind, StemKind, STEM_CHUNK_FRAMES};
use crate::stretch::Stretcher;
use std::sync::Arc;

/// Declick crossfade length (frames).
pub const XF_LEN: usize = 128;
/// Maximum |speed| (source seconds per output second) for scratching / varispeed.
pub const MAX_SPEED: f64 = 16.0;
/// Above this speed keylock falls back to varispeed.
pub const MAX_STRETCH_SPEED: f64 = 4.0;
const SRC_CAP: usize = MAX_BLOCK * 18 + 16;
const FEED_CAP: usize = 16384;
/// Seconds of track per jog revolution (33⅓ rpm vinyl).
pub const SECONDS_PER_REV: f64 = 1.8;
const STEM_RAMP: f32 = 1.0 / 1024.0;
const STEM_LOOKAHEAD: i64 = 2048;
/// Pitch bend amount at full deflection.
pub const PITCH_BEND_MAX: f64 = 0.04;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Path {
    Silent,
    Vari,
    Stretch,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Motion {
    Normal,
    /// Speed ramps linearly to 0.
    Brake { speed: f64, decel: f64 },
    /// Fast reverse spin that decays to 0.
    Spinback { speed: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Preview {
    Cue,
    HotCue(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Pending {
    /// Jump to the frame position when the quantize boundary is reached.
    Jump(f64),
    /// Jump + engage a saved loop of `beats`.
    Loop(f64, f64),
}

#[derive(Clone, Copy, Debug, Default)]
struct LoopRegion {
    defined: bool,
    active: bool,
    roll: bool,
    /// Frames (fractional).
    start: f64,
    end: f64,
    beats: Option<f64>,
}

impl LoopRegion {
    fn len(&self) -> f64 {
        self.end - self.start
    }
}

/// Echo applied to selected stems (e.g. vocal echo-out). Runs in the domain the deck is
/// rendering in (source frames before the stretcher, output frames in varispeed).
struct StemEcho {
    buf: Vec<[f32; 2]>,
    w: usize,
    delay: f32,
    feedback: f32,
    beats: f32,
    energy: f32,
}

impl StemEcho {
    fn new(cap: usize) -> Self {
        StemEcho { buf: vec![[0.0; 2]; cap], w: 0, delay: 22050.0, feedback: 0.6, beats: 0.5, energy: 0.0 }
    }

    #[inline]
    fn process(&mut self, x: [f32; 2]) -> [f32; 2] {
        let cap = self.buf.len();
        let d = self.delay.clamp(1.0, (cap - 2) as f32);
        let rp = self.w as f32 + cap as f32 - d;
        let i0 = rp.floor() as usize % cap;
        let i1 = (i0 + 1) % cap;
        let fr = rp - rp.floor();
        let a = self.buf[i0];
        let b = self.buf[i1];
        let y = [a[0] + (b[0] - a[0]) * fr, a[1] + (b[1] - a[1]) * fr];
        self.buf[self.w] = [x[0] + y[0] * self.feedback, x[1] + y[1] * self.feedback];
        self.w = (self.w + 1) % cap;
        self.energy = self.energy * 0.9995 + (y[0].abs() + y[1].abs() + x[0].abs() + x[1].abs());
        y
    }

    fn silent(&self) -> bool {
        self.energy < 1e-4
    }

    fn clear(&mut self) {
        self.buf.iter_mut().for_each(|v| *v = [0.0; 2]);
        self.energy = 0.0;
    }
}

/// Audio source: original track + stem residuals with smoothed gains.
///
/// `out = r·original + Σ (g − r)·stem`, with `r` the mean of the stem gains `g`. That is
/// `Σ g·stem + r·(original − Σ stem)`: the part of the track no stem holds (21–34 dB below it
/// with HTDemucs) follows the stems on average, so all stems at 0 is silence and all at 1 is the
/// original, bit-exact.
struct Source {
    track: Option<Arc<LoadedTrack>>,
    frames: usize,
    gains: [Smoother; 4],
    sends: [Smoother; 4],
    amt: f32,
}

impl Source {
    /// Writes `count` interleaved frames starting at `lo` (may be outside the track → silence).
    /// `adv` = output frames this read stands for (drives the gain smoothing).
    fn fill(&mut self, lo: i64, count: usize, dst: &mut [f32], mut send: Option<&mut [f32]>, forward: bool, adv: usize) {
        let dst = &mut dst[..count * 2];
        let Some(track) = self.track.as_ref() else {
            dst.fill(0.0);
            if let Some(s) = send {
                s[..count * 2].fill(0.0);
            }
            return;
        };
        let audio = &track.audio;
        let n = self.frames as i64;
        let mut g0 = [0f32; 4];
        let mut g1 = [0f32; 4];
        let mut s0 = [0f32; 4];
        let mut s1 = [0f32; 4];
        for s in 0..4 {
            let (a, b) = self.gains[s].advance(adv);
            g0[s] = a;
            g1[s] = b;
            let (a, b) = self.sends[s].advance(adv);
            s0[s] = a;
            s1[s] = b;
        }
        let gains_unity = g0.iter().chain(g1.iter()).all(|g| *g == 1.0);
        let want_send = send.is_some() && s0.iter().chain(s1.iter()).any(|g| *g != 0.0);
        if let Some(s) = send.as_deref_mut() {
            s[..count * 2].fill(0.0);
        }
        let stems = track.stems.as_ref().filter(|_| !gains_unity || want_send);
        let inv = if count > 1 { 1.0 / (count - 1) as f32 } else { 0.0 };
        let mut f = lo;
        let end = lo + count as i64;
        let mut j = 0usize;
        while f < end {
            if f < 0 || f >= n {
                let k = if f < 0 { (end.min(0) - f) as usize } else { (end - f) as usize };
                dst[j * 2..(j + k) * 2].fill(0.0);
                f += k as i64;
                j += k;
                continue;
            }
            let ci = f as usize / STEM_CHUNK_FRAMES;
            let cend = (((ci + 1) * STEM_CHUNK_FRAMES) as i64).min(n).min(end);
            let k = (cend - f) as usize;
            let fi = f as usize;
            for q in 0..k {
                dst[(j + q) * 2] = audio.left[fi + q];
                dst[(j + q) * 2 + 1] = audio.right[fi + q];
            }
            if let Some(st) = stems {
                match st.chunk(ci) {
                    Some(chunk) => {
                        // ramp towards "on" unless the next region (in read direction) is missing
                        let look = if forward { f + k as i64 + STEM_LOOKAHEAD } else { f - STEM_LOOKAHEAD };
                        let look_ok = look < 0 || look >= n || st.is_chunk_ready(look as usize / STEM_CHUNK_FRAMES);
                        let target = if look_ok { 1.0 } else { 0.0 };
                        let base = ci * STEM_CHUNK_FRAMES;
                        for q in 0..k {
                            let s = chunk.frame(fi + q - base);
                            if self.amt != target {
                                self.amt = if self.amt < target { (self.amt + STEM_RAMP).min(1.0) } else { (self.amt - STEM_RAMP).max(0.0) };
                            }
                            let t = (j + q) as f32 * inv;
                            if !gains_unity && self.amt > 0.0 {
                                let g: [f32; 4] = std::array::from_fn(|k| g0[k] + (g1[k] - g0[k]) * t);
                                let rest = (g[0] + g[1] + g[2] + g[3]) * 0.25;
                                // the change from the original: (r − 1)·original + Σ (g − r)·stem
                                let mut l = (rest - 1.0) * dst[(j + q) * 2];
                                let mut r = (rest - 1.0) * dst[(j + q) * 2 + 1];
                                for st_i in 0..4 {
                                    l += (g[st_i] - rest) * s[st_i * 2];
                                    r += (g[st_i] - rest) * s[st_i * 2 + 1];
                                }
                                dst[(j + q) * 2] += self.amt * l;
                                dst[(j + q) * 2 + 1] += self.amt * r;
                            }
                            if want_send {
                                if let Some(sb) = send.as_deref_mut() {
                                    let mut l = 0f32;
                                    let mut r = 0f32;
                                    for st_i in 0..4 {
                                        let g = s0[st_i] + (s1[st_i] - s0[st_i]) * t;
                                        l += g * s[st_i * 2];
                                        r += g * s[st_i * 2 + 1];
                                    }
                                    sb[(j + q) * 2] = self.amt * l;
                                    sb[(j + q) * 2 + 1] = self.amt * r;
                                }
                            }
                        }
                    }
                    None => self.amt = 0.0,
                }
            }
            f += k as i64;
            j += k;
        }
    }
}

/// One deck. See the module docs.
pub struct Deck {
    sr_out: f64,
    track_sr: f64,
    src_per_out: f64,
    src: Source,
    load_serial: u64,
    // transport (frames, heard)
    pos: f64,
    playing: bool,
    preview: Option<Preview>,
    motion: Motion,
    reverse: bool,
    scratching: bool,
    scratch_target: f64,
    scratch_vel: f64,
    vinyl_mode: bool,
    nudge: f64,
    bend: f64,
    slip_mode: bool,
    slip_pos: Option<f64>,
    // tempo
    pitch_slider: f32,
    pitch_range: PitchRange,
    tempo_override: Option<f64>,
    keylock: bool,
    key_shift: f32,
    // cues
    cue_mode: CueMode,
    hotcue_mode: HotCueMode,
    cue_pos: f64,
    hot_cues: [Option<HotCue>; HOT_CUES],
    held_cue: bool,
    quantize: bool,
    quantize_beats: f64,
    pending: Option<(f64, Pending)>,
    // loops
    lp: LoopRegion,
    loop_in_mark: Option<f64>,
    auto_beats: f64,
    // stems
    stem_level: [f32; 4],
    stem_mute: [bool; 4],
    stem_auto: [f32; 4],
    stem_send: [f32; 4],
    stem_echo: StemEcho,
    echo_out_release: Option<(usize, f64)>,
    // rendering
    cur_path: Path,
    cur_rate: f64,
    vari_gain: Smoother,
    stretch: Stretcher,
    stretch_live: bool,
    in_lat: usize,
    out_lat: usize,
    preroll: usize,
    transpose: f32,
    feed: i64,
    feed_xf_old: i64,
    feed_xf_rem: usize,
    src_buf: Vec<f32>,
    send_buf: Vec<f32>,
    feed_buf: Vec<f32>,
    feed_send: Vec<f32>,
    discard: Vec<f32>,
    xf_tmp: Vec<f32>,
    xf_buf: Vec<f32>,
    xf_tail: Vec<f32>,
    xf_idx: usize,
    xf_active: bool,
    xf_w: [f32; XF_LEN],
}

impl Deck {
    /// A deck rendering at `output_rate` Hz (tracks are resampled on the fly through the speed).
    pub fn new(output_rate: u32) -> Self {
        let track_sr = clubcraft_core::SAMPLE_RATE as f64;
        let stretch = Self::make_stretch(track_sr);
        let in_lat = stretch.input_latency();
        let out_lat = stretch.output_latency();
        let mut xf_w = [0f32; XF_LEN];
        for (i, w) in xf_w.iter_mut().enumerate() {
            *w = 0.5 - 0.5 * (std::f32::consts::PI * (i + 1) as f32 / XF_LEN as f32).cos();
        }
        let sr = output_rate as f64;
        Deck {
            sr_out: sr,
            track_sr,
            src_per_out: track_sr / sr,
            src: Source {
                track: None,
                frames: 0,
                gains: [Smoother::new(1.0, 0.012, sr as f32); 4],
                sends: [Smoother::new(0.0, 0.02, sr as f32); 4],
                amt: 0.0,
            },
            load_serial: 0,
            pos: 0.0,
            playing: false,
            preview: None,
            motion: Motion::Normal,
            reverse: false,
            scratching: false,
            scratch_target: 0.0,
            scratch_vel: 0.0,
            vinyl_mode: true,
            nudge: 0.0,
            bend: 0.0,
            slip_mode: false,
            slip_pos: None,
            pitch_slider: 0.0,
            pitch_range: PitchRange::default(),
            tempo_override: None,
            keylock: true,
            key_shift: 0.0,
            cue_mode: CueMode::default(),
            hotcue_mode: HotCueMode::default(),
            cue_pos: 0.0,
            hot_cues: [None; HOT_CUES],
            held_cue: false,
            quantize: true,
            quantize_beats: 1.0,
            pending: None,
            lp: LoopRegion::default(),
            loop_in_mark: None,
            auto_beats: 4.0,
            stem_level: [1.0; 4],
            stem_mute: [false; 4],
            stem_auto: [1.0; 4],
            stem_send: [0.0; 4],
            stem_echo: StemEcho::new(4 * 48000 * 2),
            echo_out_release: None,
            cur_path: Path::Silent,
            cur_rate: 0.0,
            vari_gain: Smoother::new(1.0, 0.005, sr as f32),
            stretch,
            stretch_live: false,
            in_lat,
            out_lat,
            preroll: (0.12 * track_sr) as usize + (0.03 * track_sr) as usize,
            transpose: 1.0,
            feed: 0,
            feed_xf_old: 0,
            feed_xf_rem: 0,
            src_buf: vec![0.0; SRC_CAP * 2],
            send_buf: vec![0.0; SRC_CAP * 2],
            feed_buf: vec![0.0; FEED_CAP * 2],
            feed_send: vec![0.0; FEED_CAP * 2],
            discard: vec![0.0; 8192 * 2],
            xf_tmp: vec![0.0; XF_LEN * 2],
            xf_buf: vec![0.0; XF_LEN * 2],
            xf_tail: vec![0.0; XF_LEN * 2],
            xf_idx: 0,
            xf_active: false,
            xf_w,
        }
    }

    fn make_stretch(track_sr: f64) -> Stretcher {
        // Signalsmith "default" preset geometry (120 ms blocks, 30 ms interval: best quality)
        // with split computation so the FFT work is spread evenly over audio blocks.
        Stretcher::new(2, (track_sr * 0.12) as usize, (track_sr * 0.03) as usize, true)
    }

    /// Change the output sample rate (allocates nothing, but call it off the audio thread or
    /// while the deck is stopped).
    pub fn set_output_rate(&mut self, output_rate: u32) {
        self.sr_out = output_rate as f64;
        self.src_per_out = self.track_sr / self.sr_out;
        self.stretch_live = false;
    }

    pub fn output_rate(&self) -> u32 {
        self.sr_out as u32
    }

    // ------------------------------------------------------------------ loading

    /// Load a track (parks on its start/first-beat cue, stopped). Returns the previous track so
    /// the caller can drop it off the audio thread.
    pub fn load(&mut self, track: Arc<LoadedTrack>) -> Option<Arc<LoadedTrack>> {
        self.capture_tail();
        let sr = track.sample_rate() as f64;
        if (sr - self.track_sr).abs() > 0.5 {
            // A track at a non-standard rate: rate conversion happens through the speed; the
            // stretcher keeps its 44.1 kHz configuration (it only sees samples).
            self.track_sr = sr;
            self.src_per_out = sr / self.sr_out;
        }
        self.src.frames = track.frames();
        self.hot_cues = [None; HOT_CUES];
        for c in &track.cues {
            if let Some(s) = c.slot {
                if (s as usize) < HOT_CUES && self.hot_cues[s as usize].is_none() {
                    self.hot_cues[s as usize] = Some(HotCue { pos: c.pos, kind: c.kind, color: c.color, loop_beats: c.loop_beats });
                }
            }
        }
        self.cue_pos = track.start_position();
        self.pos = self.cue_pos * self.track_sr;
        self.playing = false;
        self.preview = None;
        self.motion = Motion::Normal;
        self.reverse = false;
        self.scratching = false;
        self.slip_pos = None;
        self.pending = None;
        self.lp = LoopRegion::default();
        self.loop_in_mark = None;
        self.stem_level = [1.0; 4];
        self.stem_mute = [false; 4];
        self.stem_send = [0.0; 4];
        self.echo_out_release = None;
        self.stem_echo.clear();
        for g in self.src.gains.iter_mut() {
            g.reset(1.0);
        }
        for g in self.src.sends.iter_mut() {
            g.reset(0.0);
        }
        self.apply_stem_gains();
        self.src.amt = 0.0;
        self.key_shift = 0.0;
        self.stretch_live = false;
        self.cur_path = Path::Silent;
        self.load_serial += 1;
        self.src.track.replace(track)
    }

    /// Eject the track. Returns it for deferred dropping.
    pub fn unload(&mut self) -> Option<Arc<LoadedTrack>> {
        self.capture_tail();
        self.playing = false;
        self.preview = None;
        self.cur_path = Path::Silent;
        self.src.frames = 0;
        self.lp = LoopRegion::default();
        self.pending = None;
        self.slip_pos = None;
        self.hot_cues = [None; HOT_CUES];
        self.src.track.take()
    }

    pub fn track(&self) -> Option<&Arc<LoadedTrack>> {
        self.src.track.as_ref()
    }

    pub fn is_loaded(&self) -> bool {
        self.src.track.is_some()
    }

    pub fn load_serial(&self) -> u64 {
        self.load_serial
    }

    // ------------------------------------------------------------------ queries

    pub fn grid(&self) -> Option<BeatGrid> {
        self.src.track.as_ref().and_then(|t| t.grid)
    }

    /// Heard position (s).
    pub fn position(&self) -> f64 {
        self.pos / self.track_sr
    }

    pub fn duration(&self) -> f64 {
        self.src.frames as f64 / self.track_sr
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Tempo ratio from the pitch slider or the sync/automation override (no bend/nudge).
    pub fn tempo(&self) -> f64 {
        self.tempo_override.unwrap_or(1.0 + self.pitch_slider as f64 * self.pitch_range.fraction())
    }

    /// Tempo ratio set by the slider alone.
    pub fn slider_tempo(&self) -> f64 {
        1.0 + self.pitch_slider as f64 * self.pitch_range.fraction()
    }

    pub fn effective_bpm(&self) -> Option<f64> {
        self.grid().map(|g| g.bpm * self.tempo() * (1.0 + self.bend))
    }

    /// Fractional beat index at the heard position.
    pub fn beat_position(&self) -> Option<f64> {
        self.grid().map(|g| g.beat_at(self.position()))
    }

    pub fn keylock(&self) -> bool {
        self.keylock
    }

    /// Beat length in track frames (120 BPM when there is no grid).
    fn beat_frames(&self) -> f64 {
        self.grid().map(|g| g.beat_len()).unwrap_or(0.5) * self.track_sr
    }

    /// Current playback speed (source seconds per output second, signed).
    pub fn speed(&self) -> f64 {
        if self.scratching {
            return self.scratch_vel;
        }
        match self.motion {
            Motion::Brake { speed, .. } => {
                if self.reverse {
                    -speed
                } else {
                    speed
                }
            }
            Motion::Spinback { speed } => speed,
            Motion::Normal => {
                if self.playing {
                    let s = self.base_speed();
                    if self.reverse {
                        -s
                    } else {
                        s
                    }
                } else {
                    0.0
                }
            }
        }
    }

    fn base_speed(&self) -> f64 {
        (self.tempo() * (1.0 + self.bend) + self.nudge).max(0.0)
    }

    // ------------------------------------------------------------------ tempo

    /// Pitch slider −1..1 (scaled by the pitch range).
    pub fn set_pitch_slider(&mut self, v: f32) {
        self.pitch_slider = v.clamp(-1.0, 1.0);
    }

    pub fn pitch_slider(&self) -> f32 {
        self.pitch_slider
    }

    /// Set the slider so the tempo equals `ratio` (clamped to the range).
    pub fn set_tempo(&mut self, ratio: f64) {
        let r = self.pitch_range.fraction();
        self.pitch_slider = (((ratio - 1.0) / r) as f32).clamp(-1.0, 1.0);
    }

    pub fn set_pitch_range(&mut self, r: PitchRange) {
        let tempo = self.slider_tempo();
        self.pitch_range = r;
        self.set_tempo(tempo);
    }

    pub fn pitch_reset(&mut self) {
        self.pitch_slider = 0.0;
    }

    /// Temporary pitch bend, −1..1 of [`PITCH_BEND_MAX`]. Set back to 0 on release.
    pub fn set_pitch_bend(&mut self, v: f32) {
        self.bend = v.clamp(-1.0, 1.0) as f64 * PITCH_BEND_MAX;
    }

    /// Sync / automation tempo (overrides the slider). `None` returns control to the slider.
    pub fn set_tempo_override(&mut self, ratio: Option<f64>) {
        self.tempo_override = ratio.map(|r| r.clamp(0.0, MAX_STRETCH_SPEED));
    }

    pub fn tempo_override(&self) -> Option<f64> {
        self.tempo_override
    }

    pub fn set_keylock(&mut self, on: bool) {
        self.keylock = on;
    }

    /// Key shift in semitones (−12..12), applied by the stretcher.
    pub fn set_key_shift(&mut self, semis: f32) {
        self.key_shift = semis.clamp(-12.0, 12.0);
    }

    pub fn key_shift(&self) -> f32 {
        self.key_shift
    }

    // ------------------------------------------------------------------ transport

    pub fn play(&mut self) {
        if !self.is_loaded() {
            return;
        }
        if self.preview.is_some() {
            // play pressed while previewing a cue: keep playing after release
            self.preview = None;
            return;
        }
        if self.pos >= self.src.frames as f64 {
            return;
        }
        self.playing = true;
        self.motion = Motion::Normal;
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.preview = None;
        self.motion = Motion::Normal;
        self.pending = None;
    }

    pub fn play_pause(&mut self) {
        if self.playing && self.preview.is_none() {
            self.pause()
        } else {
            self.play()
        }
    }

    /// Pause and return to the cue point.
    pub fn stop(&mut self) {
        self.pause();
        let c = self.cue_pos * self.track_sr;
        self.jump(c);
    }

    /// Play-stutter: when playing, restart from the cue point.
    pub fn play_stutter(&mut self) {
        if self.playing {
            let c = self.cue_pos * self.track_sr;
            self.jump(c);
        } else {
            self.play();
        }
    }

    /// Seek to `t` seconds (heard position).
    pub fn seek(&mut self, t: f64) {
        let f = (t * self.track_sr).clamp(0.0, self.src.frames as f64);
        self.jump(f);
    }

    pub fn set_cue_mode(&mut self, m: CueMode) {
        self.cue_mode = m;
    }

    pub fn set_hotcue_mode(&mut self, m: HotCueMode) {
        self.hotcue_mode = m;
    }

    pub fn set_quantize(&mut self, on: bool) {
        self.quantize = on;
    }

    pub fn set_quantize_beats(&mut self, beats: f64) {
        self.quantize_beats = beats.clamp(1.0 / 16.0, 8.0);
    }

    pub fn cue_point(&self) -> f64 {
        self.cue_pos
    }

    /// Snap a frame position to the quantize grid (if quantize is on and there is a grid).
    fn snap(&self, f: f64) -> f64 {
        match (self.quantize, self.grid()) {
            (true, Some(g)) => g.snap(f / self.track_sr, self.quantize_beats) * self.track_sr,
            _ => f,
        }
    }

    /// Next quantize boundary after the playhead (frames).
    fn next_boundary(&self) -> Option<f64> {
        let g = self.grid()?;
        let q = self.quantize_beats;
        let b = g.beat_at(self.position()) / q;
        let mut nb = b.ceil();
        if nb - b < 1e-6 {
            nb += 0.0;
        }
        Some(g.time_of_beat(nb * q) * self.track_sr)
    }

    pub fn set_cue(&mut self) {
        self.cue_pos = self.snap(self.pos) / self.track_sr;
    }

    pub fn goto_cue(&mut self) {
        let c = self.cue_pos * self.track_sr;
        self.jump(c);
    }

    /// Cue button pressed (behaviour per [`CueMode`]).
    pub fn cue_down(&mut self) {
        if !self.is_loaded() {
            return;
        }
        self.held_cue = true;
        let cue = self.cue_pos * self.track_sr;
        match self.cue_mode {
            CueMode::Cdj => {
                if self.playing && self.preview.is_none() {
                    self.pause();
                    self.jump(cue);
                } else if (self.pos - cue).abs() < 0.002 * self.track_sr {
                    self.playing = true;
                    self.motion = Motion::Normal;
                    self.preview = Some(Preview::Cue);
                } else {
                    let s = self.snap(self.pos);
                    self.cue_pos = s / self.track_sr;
                    if (s - self.pos).abs() > 1.0 {
                        self.jump(s);
                    }
                }
            }
            CueMode::Hold => {
                self.jump(cue);
                self.playing = true;
                self.motion = Motion::Normal;
                self.preview = Some(Preview::Cue);
            }
            CueMode::Cup => {
                self.pause();
                self.jump(cue);
            }
        }
    }

    pub fn cue_up(&mut self) {
        if !self.held_cue {
            return;
        }
        self.held_cue = false;
        let cue = self.cue_pos * self.track_sr;
        match self.cue_mode {
            CueMode::Cdj | CueMode::Hold => {
                if self.preview == Some(Preview::Cue) {
                    self.pause();
                    self.jump(cue);
                }
            }
            CueMode::Cup => self.play(),
        }
    }

    // ------------------------------------------------------------------ hot cues

    pub fn hot_cue(&self, i: usize) -> Option<HotCue> {
        self.hot_cues.get(i).copied().flatten()
    }

    /// Set hot cue `i` at `pos` (s), or at the (quantized) playhead when `None`.
    pub fn set_hot_cue(&mut self, i: usize, pos: Option<f64>) {
        if i >= HOT_CUES || !self.is_loaded() {
            return;
        }
        let p = pos.unwrap_or_else(|| self.snap(self.pos) / self.track_sr);
        let loop_beats = if pos.is_none() && self.lp.active && !self.lp.roll { self.lp.beats } else { None };
        let p = if loop_beats.is_some() { self.lp.start / self.track_sr } else { p };
        let kind = if loop_beats.is_some() { CueKind::Loop } else { CueKind::Hot };
        self.hot_cues[i] = Some(HotCue { pos: p, kind, color: kind.default_color(), loop_beats });
    }

    pub fn delete_hot_cue(&mut self, i: usize) {
        if i < HOT_CUES {
            self.hot_cues[i] = None;
        }
    }

    /// Hot cue pad pressed: empty → set; playing → (quantized) jump; stopped → preview/play.
    pub fn hot_cue_down(&mut self, i: usize) {
        if i >= HOT_CUES || !self.is_loaded() {
            return;
        }
        let Some(hc) = self.hot_cues[i] else {
            self.set_hot_cue(i, None);
            return;
        };
        let target = hc.pos * self.track_sr;
        let action = match hc.loop_beats {
            Some(b) => Pending::Loop(target, b),
            None => Pending::Jump(target),
        };
        if self.playing && self.preview.is_none() {
            if self.quantize && self.speed() > 0.0 {
                if let Some(b) = self.next_boundary() {
                    self.pending = Some((b, action));
                    return;
                }
            }
            self.run_pending(action, 0.0);
        } else {
            self.run_pending(action, 0.0);
            self.playing = true;
            self.motion = Motion::Normal;
            if self.hotcue_mode == HotCueMode::Preview {
                self.preview = Some(Preview::HotCue(i));
            }
        }
    }

    pub fn hot_cue_up(&mut self, i: usize) {
        if self.preview == Some(Preview::HotCue(i)) {
            let p = self.hot_cues[i].map(|h| h.pos * self.track_sr).unwrap_or(self.pos);
            self.pause();
            self.jump(p);
        }
    }

    fn run_pending(&mut self, a: Pending, overshoot: f64) {
        match a {
            Pending::Jump(t) => self.jump(t + overshoot),
            Pending::Loop(t, beats) => {
                let len = beats * self.beat_frames();
                self.lp = LoopRegion { defined: true, active: true, roll: false, start: t, end: t + len, beats: Some(beats) };
                self.jump(t + overshoot.min(len * 0.999));
            }
        }
    }

    // ------------------------------------------------------------------ jog / scratch / motions

    pub fn set_vinyl_mode(&mut self, on: bool) {
        self.vinyl_mode = on;
    }

    /// Touch / release the platter. In vinyl mode touching starts a scratch.
    pub fn jog_touch(&mut self, touched: bool) {
        if touched && self.vinyl_mode && self.is_loaded() {
            if !self.scratching {
                self.scratching = true;
                self.scratch_target = self.pos;
                self.scratch_vel = self.speed();
                if self.slip_mode && self.slip_pos.is_none() && self.playing {
                    self.slip_pos = Some(self.pos);
                }
            }
        } else if !touched && self.scratching {
            self.scratching = false;
            self.end_slip();
        }
    }

    /// Jog movement in platter revolutions. Scratches while touched, nudges (temporary tempo
    /// change) while playing, and moves the position slowly while stopped.
    pub fn jog(&mut self, revs: f64) {
        if !self.is_loaded() {
            return;
        }
        if self.scratching {
            self.scratch_target += revs * SECONDS_PER_REV * self.track_sr;
        } else if self.playing {
            self.nudge = (self.nudge + revs * 0.5).clamp(-0.5, 0.5);
        } else {
            let f = self.pos + revs * SECONDS_PER_REV * self.track_sr * 0.25;
            self.jump(f.clamp(0.0, self.src.frames as f64));
        }
    }

    pub fn set_reverse(&mut self, on: bool) {
        if on == self.reverse {
            return;
        }
        self.reverse = on;
        if on && self.slip_mode && self.playing && self.slip_pos.is_none() {
            self.slip_pos = Some(self.pos);
        } else if !on {
            self.end_slip();
        }
    }

    /// Vinyl brake to a stop over `seconds`.
    pub fn brake(&mut self, seconds: f64) {
        let s = self.speed().abs();
        if s <= 0.0 {
            return;
        }
        self.motion = Motion::Brake { speed: s, decel: s / seconds.max(0.05) };
    }

    /// Backspin: fast reverse spin that slows to a stop.
    pub fn spinback(&mut self) {
        if !self.is_loaded() {
            return;
        }
        self.motion = Motion::Spinback { speed: -4.0 };
        self.playing = true;
    }

    pub fn set_slip(&mut self, on: bool) {
        self.slip_mode = on;
        if !on {
            self.slip_pos = None;
        }
    }

    fn end_slip(&mut self) {
        if let Some(g) = self.slip_pos.take() {
            if !self.lp.roll {
                self.jump(g);
            } else {
                self.slip_pos = Some(g);
            }
        }
    }

    // ------------------------------------------------------------------ loops

    fn loop_start_snapped(&self, beats: f64) -> f64 {
        match (self.quantize, self.grid()) {
            (true, Some(g)) => {
                let step = beats.min(1.0);
                let b = g.beat_at(self.position()) / step;
                g.time_of_beat((b + 1e-9).floor() * step) * self.track_sr
            }
            _ => self.pos,
        }
    }

    /// Auto loop of `beats` beats (toggle off when the same size is already looping).
    pub fn auto_loop(&mut self, beats: f64) {
        if !self.is_loaded() || beats <= 0.0 {
            return;
        }
        if self.lp.active && !self.lp.roll && self.lp.beats.map(|b| (b - beats).abs() < 1e-9).unwrap_or(false) {
            self.loop_exit();
            return;
        }
        let start = self.loop_start_snapped(beats);
        let len = beats * self.beat_frames();
        self.auto_beats = beats;
        self.engage_loop(start, start + len, Some(beats));
    }

    fn engage_loop(&mut self, start: f64, end: f64, beats: Option<f64>) {
        self.lp = LoopRegion { defined: true, active: true, roll: false, start, end, beats };
        if self.slip_mode && self.playing && self.slip_pos.is_none() {
            self.slip_pos = Some(self.pos);
        }
        if self.pos >= end || self.pos < start - 1.0 {
            let len = end - start;
            let p = if self.pos >= end { start + (self.pos - start).rem_euclid(len) } else { start };
            self.jump(p);
        }
    }

    pub fn loop_in(&mut self) {
        let p = self.snap(self.pos);
        self.loop_in_mark = Some(p);
        if self.lp.active && p < self.lp.end {
            self.lp.start = p;
            self.lp.beats = self.grid().map(|_| self.lp.len() / self.beat_frames());
        }
    }

    pub fn loop_out(&mut self) {
        if self.lp.active && !self.lp.roll {
            self.loop_exit();
            return;
        }
        if let Some(s) = self.loop_in_mark {
            let e = self.snap(self.pos);
            if e > s + 32.0 {
                let beats = self.grid().map(|_| (e - s) / self.beat_frames());
                self.engage_loop(s, e, beats);
            }
        }
    }

    pub fn loop_exit(&mut self) {
        if self.lp.active {
            self.lp.active = false;
            self.lp.roll = false;
            self.end_slip();
        }
    }

    /// Re-engage the last loop and jump to its start (exit when active).
    pub fn reloop(&mut self) {
        if self.lp.active {
            self.loop_exit();
        } else if self.lp.defined {
            self.lp.active = true;
            let s = self.lp.start;
            self.jump(s);
        }
    }

    pub fn loop_halve(&mut self) {
        if self.lp.defined && self.lp.len() > 64.0 {
            let len = self.lp.len() * 0.5;
            self.lp.end = self.lp.start + len;
            self.lp.beats = self.lp.beats.map(|b| b * 0.5);
            if self.lp.active && self.pos >= self.lp.end {
                let p = self.lp.start + (self.pos - self.lp.start).rem_euclid(len);
                self.jump(p);
            }
        }
        self.auto_beats = (self.auto_beats * 0.5).max(1.0 / 32.0);
    }

    pub fn loop_double(&mut self) {
        if self.lp.defined {
            let len = self.lp.len() * 2.0;
            self.lp.end = self.lp.start + len;
            self.lp.beats = self.lp.beats.map(|b| b * 2.0);
        }
        self.auto_beats = (self.auto_beats * 2.0).min(32.0);
    }

    /// Move the loop (and the playhead with it) by `beats`.
    pub fn loop_move(&mut self, beats: f64) {
        if !self.lp.defined {
            return;
        }
        let d = beats * self.beat_frames();
        self.lp.start += d;
        self.lp.end += d;
        if self.lp.active {
            let p = self.pos + d;
            let lp = self.lp;
            self.jump(p);
            self.lp = lp;
        }
    }

    /// Momentary loop with slip: on release playback continues where it would have been.
    pub fn loop_roll(&mut self, beats: f64, on: bool) {
        if !self.is_loaded() {
            return;
        }
        if on {
            if self.slip_pos.is_none() {
                self.slip_pos = Some(self.pos);
            }
            let start = match self.grid() {
                Some(g) => {
                    let step = beats.min(1.0);
                    let b = g.beat_at(self.position()) / step;
                    g.time_of_beat((b + 1e-9).floor() * step) * self.track_sr
                }
                None => self.pos,
            };
            let len = beats * self.beat_frames();
            self.lp = LoopRegion { defined: true, active: true, roll: true, start, end: start + len, beats: Some(beats) };
            if self.pos >= start + len {
                self.jump(start + (self.pos - start).rem_euclid(len));
            }
        } else if self.lp.roll {
            self.lp.active = false;
            self.lp.roll = false;
            if let Some(g) = self.slip_pos.take() {
                if self.playing {
                    self.jump(g);
                }
            }
        }
    }

    /// Beat jump by `beats` (moves an active loop along).
    pub fn beat_jump(&mut self, beats: f64) {
        if !self.is_loaded() {
            return;
        }
        let d = beats * self.beat_frames();
        if self.lp.active {
            self.lp.start += d;
            self.lp.end += d;
        }
        let lp = self.lp;
        let p = (self.pos + d).clamp(0.0, self.src.frames as f64);
        self.jump(p);
        self.lp = lp;
    }

    pub fn loop_state(&self) -> LoopState {
        let sr = self.track_sr;
        LoopState {
            defined: self.lp.defined,
            active: self.lp.active,
            start: self.lp.start / sr,
            end: self.lp.end / sr,
            beats: self.lp.beats,
            roll: self.lp.roll,
            loop_in_mark: self.loop_in_mark.map(|f| f / sr),
            auto_beats: self.auto_beats,
        }
    }

    // ------------------------------------------------------------------ stems

    fn apply_stem_gains(&mut self) {
        for s in 0..4 {
            let g = if self.stem_mute[s] { 0.0 } else { self.stem_level[s] } * self.stem_auto[s];
            self.src.gains[s].set(g);
            self.src.sends[s].set(self.stem_send[s]);
        }
    }

    pub fn set_stem_gain(&mut self, stem: StemKind, g: f32) {
        self.stem_level[stem.index()] = g.clamp(0.0, 2.0);
        self.apply_stem_gains();
    }

    pub fn set_stem_mute(&mut self, stem: StemKind, mute: bool) {
        self.stem_mute[stem.index()] = mute;
        self.apply_stem_gains();
    }

    /// Solo `stem` (toggle: soloing the only unmuted stem unmutes everything).
    pub fn stem_solo(&mut self, stem: StemKind) {
        let i = stem.index();
        let already = (0..4).all(|s| self.stem_mute[s] == (s != i));
        for s in 0..4 {
            self.stem_mute[s] = if already { false } else { s != i };
        }
        self.apply_stem_gains();
    }

    /// Vocals only (toggle).
    pub fn acapella(&mut self) {
        self.stem_solo(StemKind::Vocals);
    }

    /// Everything but vocals (toggle).
    pub fn instrumental(&mut self) {
        let v = StemKind::Vocals.index();
        let on = !(self.stem_mute[v] && (0..4).filter(|s| *s != v).all(|s| !self.stem_mute[s]));
        for s in 0..4 {
            self.stem_mute[s] = on && s == v;
        }
        self.apply_stem_gains();
    }

    pub fn stems_reset(&mut self) {
        self.stem_level = [1.0; 4];
        self.stem_mute = [false; 4];
        self.apply_stem_gains();
    }

    /// Automation multiplier on a stem gain (used by automix / mix playback).
    pub fn set_stem_automation(&mut self, stem: StemKind, g: f32) {
        self.stem_auto[stem.index()] = g.clamp(0.0, 2.0);
        self.apply_stem_gains();
    }

    /// Jump the stem gains to their targets without smoothing: a track that starts in the middle
    /// of a transition must not sound at full level until the gains have glided down (that
    /// includes the audio the time-stretcher is primed with).
    pub fn snap_stem_gains(&mut self) {
        self.apply_stem_gains();
        for g in self.src.gains.iter_mut().chain(self.src.sends.iter_mut()) {
            g.reset(g.target);
        }
    }

    /// How far (track seconds) the audio being fed into the time-stretcher (stems mixed in) is
    /// ahead of what is heard: 0 without the stretcher.
    pub fn feed_lead_secs(&self) -> f64 {
        let stretching = self.cur_path == Path::Stretch || (self.cur_path == Path::Silent && (self.keylock || self.key_shift != 0.0));
        if stretching {
            (self.in_lat as f64 + self.out_lat as f64 * self.cur_rate.abs().max(0.05)) / self.track_sr
        } else {
            0.0
        }
    }

    pub fn has_stems(&self) -> bool {
        self.src.track.as_ref().is_some_and(|t| t.stems.is_some())
    }

    /// Effective stem gains (user × automation × mute).
    pub fn stem_gains(&self) -> [f32; 4] {
        let mut g = [0f32; 4];
        for (s, v) in g.iter_mut().enumerate() {
            *v = self.src.gains[s].target;
        }
        g
    }

    /// Stem echo send for `stem` (0..1); `beats` delay, `feedback` 0..0.95.
    pub fn set_stem_echo(&mut self, stem: StemKind, send: f32, beats: f32, feedback: f32) {
        self.stem_send[stem.index()] = send.clamp(0.0, 1.0);
        self.stem_echo.beats = beats.clamp(1.0 / 16.0, 4.0);
        self.stem_echo.feedback = feedback.clamp(0.0, 0.95);
        self.apply_stem_gains();
    }

    /// "Echo out" one stem: the stem is muted and its last beat echoes away.
    pub fn stem_echo_out(&mut self, stem: StemKind) {
        let i = stem.index();
        self.stem_send[i] = 1.0;
        self.stem_echo.beats = 0.5;
        self.stem_echo.feedback = 0.65;
        self.stem_mute[i] = true;
        self.apply_stem_gains();
        self.echo_out_release = Some((i, 1.0 * self.beat_frames() / self.track_sr));
    }

    // ------------------------------------------------------------------ jumps & crossfades

    /// Jump the heard position to frame `f` with a declick crossfade.
    fn jump(&mut self, f: f64) {
        self.capture_tail();
        if self.lp.active && !self.lp.roll && (f < self.lp.start - 1.0 || f >= self.lp.end) {
            self.lp.active = false;
        }
        self.pos = f.clamp(0.0, self.src.frames as f64);
        self.stretch_live = false;
        self.pending = None;
        if self.scratching {
            self.scratch_target = self.pos;
        }
    }

    /// Render `XF_LEN` frames of the current path's continuation into the crossfade tail.
    fn capture_tail(&mut self) {
        crate::prof::time(3, || self.capture_tail_inner())
    }

    fn capture_tail_inner(&mut self) {
        let mut tmp = std::mem::take(&mut self.xf_tmp);
        match self.cur_path {
            Path::Silent => tmp.fill(0.0),
            Path::Vari => {
                let p = self.pos;
                let r = self.cur_rate;
                self.render_vari(&mut tmp, XF_LEN, r, false);
                self.pos = p;
            }
            Path::Stretch => {
                if self.stretch_live {
                    let m = ((self.cur_rate * XF_LEN as f64).round() as usize).min(FEED_CAP);
                    self.read_feed(m, false);
                    self.stretch.process(&self.feed_buf[..m * 2], &mut tmp[..]);
                    self.stretch_live = false;
                } else {
                    let p = self.pos;
                    let r = self.cur_rate;
                    self.render_vari(&mut tmp, XF_LEN, r, false);
                    self.pos = p;
                }
            }
        }
        if self.xf_active {
            for i in 0..XF_LEN {
                let j = self.xf_idx + i;
                if j < XF_LEN {
                    let w = self.xf_w[j];
                    tmp[i * 2] = tmp[i * 2] * w + self.xf_buf[j * 2] * (1.0 - w);
                    tmp[i * 2 + 1] = tmp[i * 2 + 1] * w + self.xf_buf[j * 2 + 1] * (1.0 - w);
                }
            }
        }
        std::mem::swap(&mut self.xf_buf, &mut tmp);
        self.xf_tmp = tmp;
        self.xf_idx = 0;
        self.xf_active = true;
    }

    fn apply_xfade(&mut self, out: &mut [f32]) {
        if !self.xf_active {
            return;
        }
        for f in out.as_chunks_mut::<2>().0 {
            if self.xf_idx >= XF_LEN {
                self.xf_active = false;
                break;
            }
            let w = self.xf_w[self.xf_idx];
            f[0] = f[0] * w + self.xf_buf[self.xf_idx * 2] * (1.0 - w);
            f[1] = f[1] * w + self.xf_buf[self.xf_idx * 2 + 1] * (1.0 - w);
            self.xf_idx += 1;
        }
    }

    // ------------------------------------------------------------------ rendering

    /// Varispeed render of `n` frames at `rate` (source frames per output frame) from `self.pos`.
    fn render_vari(&mut self, out: &mut [f32], n: usize, rate: f64, echo: bool) {
        let p0 = self.pos;
        let p_last = p0 + rate * (n as f64 - 1.0).max(0.0);
        let lo = p0.min(p_last).floor() as i64 - 1;
        let hi = p0.max(p_last).floor() as i64 + 3;
        let count = ((hi - lo) as usize).min(SRC_CAP);
        let use_echo = echo && (self.stem_send.iter().any(|s| *s > 0.0) || !self.stem_echo.silent());
        let mut src_buf = std::mem::take(&mut self.src_buf);
        let mut send_buf = std::mem::take(&mut self.send_buf);
        self.src.fill(lo, count, &mut src_buf, if use_echo { Some(&mut send_buf[..]) } else { None }, rate >= 0.0, n);
        let max_idx = count as i64 - 3;
        for i in 0..n {
            let p = p0 + rate * i as f64;
            let ip = p.floor();
            let t = (p - ip) as f32;
            let idx = ((ip as i64 - lo).clamp(1, max_idx.max(1))) as usize;
            let b = &src_buf[(idx - 1) * 2..(idx + 3) * 2];
            out[i * 2] = hermite(b[0], b[2], b[4], b[6], t);
            out[i * 2 + 1] = hermite(b[1], b[3], b[5], b[7], t);
        }
        if use_echo {
            let bpm = self.grid().map(|g| g.bpm).unwrap_or(120.0);
            let sp = self.base_speed().max(0.25);
            self.stem_echo.delay = (self.stem_echo.beats as f64 * 60.0 / (bpm * sp) * self.sr_out) as f32;
            for i in 0..n {
                let p = p0 + rate * i as f64;
                let idx = ((p.round() as i64 - lo).clamp(0, count as i64 - 1)) as usize;
                let y = self.stem_echo.process([send_buf[idx * 2], send_buf[idx * 2 + 1]]);
                out[i * 2] += y[0];
                out[i * 2 + 1] += y[1];
            }
        }
        self.src_buf = src_buf;
        self.send_buf = send_buf;
        self.pos = p0 + rate * n as f64;
    }

    /// Feed position the stretcher should have for heard position `p` (loop aware).
    fn feed_map(&self, x: f64) -> f64 {
        if self.lp.active && x >= self.lp.end && self.pos >= self.lp.start - 1.0 && self.lp.len() > 1.0 {
            self.lp.start + (x - self.lp.start).rem_euclid(self.lp.len())
        } else {
            x
        }
    }

    fn feed_distance(&self, target: f64) -> f64 {
        let f = self.feed as f64;
        let d = target - f;
        if self.lp.active && d < -1.0 && f <= self.lp.end + 2.0 && target >= self.lp.start - 1.0 {
            (self.lp.end - f) + (target - self.lp.start)
        } else {
            d
        }
    }

    /// Read `m` frames into `feed_buf` from the feed cursor, wrapping at the loop end with an
    /// input-side crossfade, applying the stem echo when `echo`.
    fn read_feed(&mut self, m: usize, echo: bool) {
        let m = m.min(FEED_CAP);
        let mut feed_buf = std::mem::take(&mut self.feed_buf);
        let mut feed_send = std::mem::take(&mut self.feed_send);
        let mut xf_tmp = std::mem::take(&mut self.xf_tail);
        let use_echo = echo && (self.stem_send.iter().any(|s| *s > 0.0) || !self.stem_echo.silent());
        let mut w = 0usize;
        while w < m {
            let mut k = m - w;
            let (ls, le) = (self.lp.start.round() as i64, self.lp.end.round() as i64);
            let looping = self.lp.active && le - ls > 1 && self.feed < le && self.feed >= ls - 2;
            if looping {
                k = k.min((le - self.feed) as usize);
            }
            let send = if use_echo { Some(&mut feed_send[w * 2..]) } else { None };
            self.src.fill(self.feed, k, &mut feed_buf[w * 2..], send, true, k);
            if self.feed_xf_rem > 0 {
                let x = k.min(self.feed_xf_rem);
                self.src.fill(self.feed_xf_old, x, &mut xf_tmp, None, true, 0);
                for q in 0..x {
                    let wi = XF_LEN - self.feed_xf_rem + q;
                    let wt = self.xf_w[wi.min(XF_LEN - 1)];
                    for c in 0..2 {
                        let a = &mut feed_buf[(w + q) * 2 + c];
                        *a = *a * wt + xf_tmp[q * 2 + c] * (1.0 - wt);
                    }
                }
                self.feed_xf_old += x as i64;
                self.feed_xf_rem -= x;
            }
            self.feed += k as i64;
            w += k;
            if looping && self.feed >= le {
                self.feed_xf_old = self.feed;
                self.feed_xf_rem = XF_LEN;
                self.feed -= le - ls;
            }
        }
        if use_echo {
            let bpm = self.grid().map(|g| g.bpm).unwrap_or(120.0);
            self.stem_echo.delay = (self.stem_echo.beats as f64 * 60.0 / bpm * self.track_sr) as f32;
            for q in 0..m {
                let y = self.stem_echo.process([feed_send[q * 2], feed_send[q * 2 + 1]]);
                feed_buf[q * 2] += y[0];
                feed_buf[q * 2 + 1] += y[1];
            }
        }
        self.feed_buf = feed_buf;
        self.feed_send = feed_send;
        self.xf_tail = xf_tmp;
    }

    fn update_transpose(&mut self, speed: f64) {
        let pitch = if self.keylock { 1.0 } else { speed };
        let t = (pitch * 2f64.powf(self.key_shift as f64 / 12.0) * self.track_sr / self.sr_out) as f32;
        if (t - self.transpose).abs() > 1e-6 {
            self.transpose = t;
            crate::prof::time(4, || self.stretch.set_transpose_factor(t));
        }
    }

    /// Reset the stretcher and prime it so its next output sample is the heard position.
    fn stretch_restart(&mut self, rate: f64) {
        crate::prof::time(0, || self.stretch_restart_inner(rate))
    }

    fn stretch_restart_inner(&mut self, rate: f64) {
        self.stretch.reset();
        let speed = rate / self.src_per_out;
        self.update_transpose(speed);
        self.stretch.set_transpose_factor(self.transpose);
        let pre_end = self.feed_map(self.pos + self.in_lat as f64).round() as i64;
        let pre = self.preroll.min(FEED_CAP);
        let mut feed_buf = std::mem::take(&mut self.feed_buf);
        self.src.fill(pre_end - pre as i64, pre, &mut feed_buf, None, true, 0);
        self.stretch.seek(&feed_buf[..pre * 2], rate);
        self.feed_buf = feed_buf;
        self.feed = pre_end;
        self.feed_xf_rem = 0;
        // pre-run the output latency so the output is at full level and aligned immediately
        let ol = self.out_lat.min(self.discard.len() / 2);
        let m = ((self.out_lat as f64 * rate).round() as usize).min(FEED_CAP);
        self.read_feed(m, false);
        let mut discard = std::mem::take(&mut self.discard);
        self.stretch.process(&self.feed_buf[..m * 2], &mut discard[..ol * 2]);
        self.discard = discard;
        self.stretch_live = true;
    }

    fn render_stretch(&mut self, out: &mut [f32], n: usize, rate: f64) {
        if !self.stretch_live {
            self.stretch_restart(rate);
        }
        let speed = rate / self.src_per_out;
        self.update_transpose(speed);
        let la = self.in_lat as f64 + self.out_lat as f64 * rate;
        let mut p_end = self.pos + rate * n as f64;
        if self.lp.active && p_end >= self.lp.end && self.pos >= self.lp.start - 1.0 && self.lp.len() > 1.0 {
            p_end = self.lp.start + (p_end - self.lp.start).rem_euclid(self.lp.len());
        }
        let nominal = rate * n as f64;
        let mut d = self.feed_distance(self.feed_map(p_end + la));
        if d.is_nan() || d < 0.0 || (d - nominal).abs() > (nominal * 0.1).max(24.0) {
            // unpredictable discontinuity: crossfade out and re-prime
            self.capture_tail();
            self.stretch_restart(rate);
            d = self.feed_distance(self.feed_map(p_end + la));
        }
        let m = (d.round().max(0.0) as usize).min(FEED_CAP);
        crate::prof::time(2, || self.read_feed(m, true));
        crate::prof::time(1, || self.stretch.process(&self.feed_buf[..m * 2], &mut out[..n * 2]));
        self.pos = p_end;
    }

    fn choose_path(&self, speed: f64) -> Path {
        if !self.is_loaded() {
            return Path::Silent;
        }
        if self.scratching {
            return Path::Vari;
        }
        if speed == 0.0 {
            return Path::Silent;
        }
        let stretchable = self.motion == Motion::Normal && speed > 0.05 && speed < MAX_STRETCH_SPEED;
        if stretchable && (self.keylock || self.key_shift != 0.0) {
            Path::Stretch
        } else {
            Path::Vari
        }
    }

    fn update_motion(&mut self, dt: f64) {
        match &mut self.motion {
            Motion::Normal => {}
            Motion::Brake { speed, decel } => {
                *speed -= *decel * dt;
                if *speed <= 0.005 {
                    self.motion = Motion::Normal;
                    self.playing = false;
                    self.preview = None;
                }
            }
            Motion::Spinback { speed } => {
                *speed *= (-dt / 0.35).exp();
                if speed.abs() < 0.03 {
                    self.motion = Motion::Normal;
                    self.playing = false;
                    self.preview = None;
                }
            }
        }
        if self.scratching {
            let err = (self.scratch_target - self.pos) / self.track_sr;
            let desired = (err / 0.03).clamp(-MAX_SPEED, MAX_SPEED);
            let a = 1.0 - (-dt / 0.012).exp();
            self.scratch_vel += (desired - self.scratch_vel) * a;
        }
        self.nudge *= (-dt / 0.15).exp();
        if self.nudge.abs() < 1e-5 {
            self.nudge = 0.0;
        }
        if let Some((s, rem)) = &mut self.echo_out_release {
            *rem -= dt;
            if *rem <= 0.0 {
                let s = *s;
                self.stem_send[s] = 0.0;
                self.echo_out_release = None;
                self.apply_stem_gains();
            }
        }
    }

    /// Frames (at `rate`) until the heard position reaches frame `x`, if ahead.
    fn frames_until(&self, x: f64, rate: f64) -> Option<usize> {
        if rate > 0.0 && x >= self.pos {
            Some(((x - self.pos) / rate).ceil().max(0.0) as usize)
        } else if rate < 0.0 && x <= self.pos {
            Some(((self.pos - x) / -rate).ceil().max(0.0) as usize)
        } else {
            None
        }
    }

    /// Render the next block. `out` is interleaved stereo, at most [`MAX_BLOCK`] frames.
    pub fn process(&mut self, out: &mut [f32]) {
        let n = (out.len() / 2).min(MAX_BLOCK);
        let out = &mut out[..n * 2];
        out.fill(0.0);
        let dt = n as f64 / self.sr_out;
        self.update_motion(dt);
        let speed = self.speed();
        let rate = speed * self.src_per_out;
        let path = self.choose_path(speed);
        if path != self.cur_path {
            self.capture_tail();
            self.stretch_live = false;
            self.cur_path = path;
        }
        self.cur_rate = rate;
        if path == Path::Vari {
            self.vari_gain.set(if self.scratching { ((speed.abs() - 0.01) * 25.0).clamp(0.0, 1.0) as f32 } else { 1.0 });
        } else {
            self.vari_gain.reset(1.0);
        }
        if self.playing {
            let adv = self.base_speed() * self.src_per_out * n as f64;
            if let Some(g) = &mut self.slip_pos {
                *g += adv;
            }
        }
        if path == Path::Silent {
            self.apply_xfade(out);
            return;
        }
        let frames = self.src.frames as f64;
        let mut done = 0usize;
        let mut guard = 0;
        while done < n && guard < 64 {
            guard += 1;
            // due events
            if let Some((at, a)) = self.pending {
                if rate <= 0.0 || self.pos >= at - 1e-6 {
                    self.pending = None;
                    let over = (self.pos - at).max(0.0);
                    self.run_pending(a, over);
                }
            }
            if self.cur_path == Path::Vari && self.lp.active && self.lp.len() > 1.0 {
                if rate > 0.0 && self.pos >= self.lp.end && self.pos < self.lp.end + rate * 2.0 + 1.0 {
                    let p = self.pos - self.lp.len();
                    self.capture_tail();
                    self.pos = p;
                } else if rate < 0.0 && self.pos < self.lp.start && self.pos > self.lp.start + rate * 2.0 - 1.0 {
                    let p = self.pos + self.lp.len();
                    self.capture_tail();
                    self.pos = p;
                }
            }
            if (rate > 0.0 && self.pos >= frames) || (rate < 0.0 && self.pos <= 0.0) {
                // end (or start, in reverse) of the track
                self.pos = self.pos.clamp(0.0, frames);
                self.playing = false;
                self.preview = None;
                self.motion = Motion::Normal;
                self.scratch_vel = 0.0;
                self.capture_tail();
                self.cur_path = Path::Silent;
                self.apply_xfade(&mut out[done * 2..]);
                return;
            }
            let mut seg = n - done;
            if let Some((at, _)) = self.pending {
                if let Some(k) = self.frames_until(at, rate) {
                    seg = seg.min(k.max(1));
                }
            }
            if self.cur_path == Path::Vari && self.lp.active && self.lp.len() > 1.0 {
                let edge = if rate > 0.0 { self.lp.end } else { self.lp.start - 1e-9 };
                if let Some(k) = self.frames_until(edge, rate) {
                    seg = seg.min(k.max(1));
                }
            }
            if let Some(k) = self.frames_until(if rate > 0.0 { frames } else { 0.0 }, rate) {
                seg = seg.min(k.max(1));
            }
            let o = &mut out[done * 2..(done + seg) * 2];
            match self.cur_path {
                Path::Vari => {
                    self.render_vari(o, seg, rate, true);
                    if !self.vari_gain.settled() || self.vari_gain.value != 1.0 {
                        for f in o.as_chunks_mut::<2>().0 {
                            let g = self.vari_gain.next();
                            f[0] *= g;
                            f[1] *= g;
                        }
                    }
                }
                Path::Stretch => self.render_stretch(o, seg, rate),
                Path::Silent => {}
            }
            self.apply_xfade(o);
            done += seg;
        }
    }

    /// Fill the deck part of a [`DeckState`] snapshot.
    pub fn snapshot(&self, st: &mut DeckState) {
        let t = self.src.track.as_ref();
        st.loaded = t.is_some();
        st.track_id = t.and_then(|t| t.track_id);
        st.load_serial = self.load_serial;
        st.duration = self.duration();
        st.position = self.position();
        st.playing = self.playing;
        st.previewing = self.preview.is_some();
        st.reverse = self.reverse;
        st.scratching = self.scratching;
        st.braking = self.motion != Motion::Normal;
        st.slip_mode = self.slip_mode;
        st.slip_position = self.slip_pos.map(|f| f / self.track_sr);
        let grid = self.grid();
        st.bpm = grid.map(|g| g.bpm);
        st.effective_bpm = self.effective_bpm();
        st.tempo = self.tempo() * (1.0 + self.bend);
        st.pitch_percent = (st.tempo - 1.0) * 100.0;
        st.pitch_slider = self.pitch_slider;
        st.pitch_range = self.pitch_range;
        st.keylock = self.keylock;
        st.key_shift = self.key_shift;
        st.key = t.and_then(|t| t.key);
        st.effective_key = st.key.map(|k| {
            let vs = if self.keylock { 0.0 } else { 12.0 * st.tempo.max(1e-3).log2() };
            k.shifted((self.key_shift as f64 + vs).round() as i32)
        });
        st.stretching = self.cur_path == Path::Stretch;
        st.cue = self.cue_pos;
        st.cue_mode = self.cue_mode;
        st.hotcue_mode = self.hotcue_mode;
        st.hot_cues = self.hot_cues;
        st.loop_state = self.loop_state();
        st.quantize = self.quantize;
        st.beat = grid.map(|g| g.beat_at(self.position()));
        st.stem_gains = self.stem_gains();
        st.stem_muted = self.stem_mute;
        st.has_stems = self.has_stems();
        st.stems_ready = t.and_then(|t| t.stems.as_ref()).map(|s| s.ready_fraction()).unwrap_or(0.0);
        st.stem_echo = self.stem_send;
    }

    /// Ensure the next phase-sync jump is seamless: jump to frame-accurate seconds.
    pub fn jump_seconds(&mut self, t: f64) {
        self.seek(t);
    }

    /// The stretcher's total latency in output frames at speed 1 (for diagnostics).
    pub fn stretch_latency(&self) -> usize {
        self.in_lat + self.out_lat
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clubcraft_core::STEM_CHANNELS;
    use crate::track::buffer_from_mono;
    use clubcraft_core::StemTrack;

    fn noise(n: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                ((s >> 9) as f32 / (1u32 << 23) as f32 - 1.0) * 0.5
            })
            .collect()
    }

    fn render(deck: &mut Deck, frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * 2);
        let mut buf = [0f32; MAX_BLOCK * 2];
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(MAX_BLOCK);
            deck.process(&mut buf[..n * 2]);
            out.extend_from_slice(&buf[..n * 2]);
            done += n;
        }
        out
    }

    #[test]
    fn residual_stems_unity_is_bit_exact() {
        let n = 3 * STEM_CHUNK_FRAMES + 1234;
        let l = noise(n, 1);
        let r = noise(n, 2);
        // random stems that do NOT sum to the mix: unity gains must still give the original
        let stems: [(Vec<f32>, Vec<f32>); 4] = [(noise(n, 3), noise(n, 4)), (noise(n, 5), noise(n, 6)), (noise(n, 7), noise(n, 8)), (noise(n, 9), noise(n, 10))];
        let st = Arc::new(StemTrack::from_planar("test", &stems));
        let audio = Arc::new(clubcraft_audio::AudioBuffer::new(44100, l.clone(), r.clone()));
        let track = Arc::new(LoadedTrack::new("t", audio).with_stems(st));
        let mut deck = Deck::new(44100);
        deck.set_keylock(false);
        deck.set_quantize(false);
        deck.load(track);
        deck.seek(0.0);
        // bounce the vocal gain and come back to unity: once settled, output is bit exact again
        deck.set_stem_gain(StemKind::Vocals, 0.0);
        deck.play();
        let _ = render(&mut deck, 4096);
        deck.set_stem_gain(StemKind::Vocals, 1.0);
        let _ = render(&mut deck, 8192);
        deck.seek(0.0);
        let _ = render(&mut deck, XF_LEN); // crossfade from the seek
        let start = (deck.position() * 44100.0).round() as usize;
        let out = render(&mut deck, n - start - 10);
        for i in 0..out.len() / 2 {
            assert_eq!(out[i * 2], l[start + i], "L differs at {}", start + i);
            assert_eq!(out[i * 2 + 1], r[start + i], "R differs at {}", start + i);
        }
        // muting vocals: the other stems, plus 3/4 of what no stem holds
        let muted = |mute: &[StemKind]| {
            let mut deck = Deck::new(44100);
            deck.set_keylock(false);
            let st = Arc::new(StemTrack::from_planar("test", &stems));
            let audio = Arc::new(clubcraft_audio::AudioBuffer::new(44100, l.clone(), r.clone()));
            deck.load(Arc::new(LoadedTrack::new("t", audio).with_stems(st)));
            deck.seek(0.0);
            for k in mute {
                deck.set_stem_mute(*k, true);
            }
            deck.play();
            let _ = render(&mut deck, 8192);
            let start = (deck.position() * 44100.0).round() as usize;
            (start, render(&mut deck, 1000))
        };
        let (start, out) = muted(&[StemKind::Vocals]);
        for i in 0..1000 {
            let f = start + i;
            let sum: f32 = (0..4).map(|k| stems[k].0[f]).sum();
            let e = stems[0].0[f] + stems[1].0[f] + stems[2].0[f] + 0.75 * (l[f] - sum);
            // stems are held as 16-bit with a per-stem scale: error ≤ peak / 32767
            assert!((out[i * 2] - e).abs() < 1e-4, "{} vs {e}", out[i * 2]);
        }
        // all stems off is silence, although these stems don't add up to the track
        let (_, out) = muted(&StemKind::ALL);
        assert!(out.iter().all(|v| v.abs() < 1e-4), "max {}", out.iter().fold(0f32, |m, v| m.max(v.abs())));
    }

    #[test]
    fn progressive_stems_fall_back_to_original() {
        let n = 2 * STEM_CHUNK_FRAMES;
        let l = noise(n, 11);
        let st = Arc::new(StemTrack::new(n, "p"));
        // only chunk 1 ready: vocals = whole mix there
        let mut data = vec![0f32; STEM_CHUNK_FRAMES * STEM_CHANNELS];
        for f in 0..STEM_CHUNK_FRAMES {
            data[f * 8 + 6] = l[STEM_CHUNK_FRAMES + f];
            data[f * 8 + 7] = l[STEM_CHUNK_FRAMES + f];
        }
        st.set_chunk(1, data);
        let audio = Arc::new(buffer_from_mono(44100, &l));
        let mut deck = Deck::new(44100);
        deck.set_keylock(false);
        deck.load(Arc::new(LoadedTrack::new("t", audio).with_stems(st)));
        deck.set_stem_mute(StemKind::Vocals, true);
        deck.seek(0.0);
        deck.play();
        let out = render(&mut deck, n - 100);
        // chunk 0 (no stems): original
        let r0: f32 = out[2000..20000].iter().map(|v| v * v).sum::<f32>();
        assert!(r0 > 100.0);
        // well inside chunk 1: vocals (= everything) removed
        let a = (STEM_CHUNK_FRAMES + 8000) * 2;
        let r1: f32 = out[a..a + 20000].iter().map(|v| v * v).sum::<f32>();
        assert!(r1 < 1e-3, "residual {r1}");
        // no hard step at the chunk boundary
        let b = STEM_CHUNK_FRAMES * 2;
        for i in (b - 4000..b + 4000).step_by(2) {
            assert!((out[i] - out[i + 2]).abs() < 1.2);
        }
    }

    #[test]
    fn loop_is_sample_accurate() {
        // 120 BPM: 1 beat = 22050 frames exactly
        let n = 44100 * 20;
        let l = noise(n, 21);
        let audio = Arc::new(buffer_from_mono(44100, &l));
        let mut deck = Deck::new(44100);
        deck.set_keylock(false);
        deck.load(Arc::new(LoadedTrack::new("t", audio).with_grid(BeatGrid::new(120.0, 0.0))));
        deck.seek(2.3);
        deck.play();
        let _ = render(&mut deck, 1000);
        deck.auto_loop(1.0);
        let ls = deck.loop_state();
        assert!(ls.active);
        assert!((ls.start - 2.0).abs() < 1e-9, "quantized loop start {}", ls.start);
        assert!((ls.end - ls.start - 0.5).abs() < 1e-12);
        let out = render(&mut deck, 22050 * 6);
        // output is exactly periodic with the loop length
        for i in 22050 * 2..22050 * 4 {
            assert_eq!(out[i * 2], out[(i + 22050) * 2], "not periodic at {i}");
        }
        // and the loop body equals the source (outside the declick region)
        let pos = deck.position();
        assert!((2.0..2.5).contains(&pos));
        // fractional loop length (128 BPM, 20671.875 frames per beat): 8 loops = 165375 frames
        let audio = Arc::new(buffer_from_mono(44100, &l));
        let mut deck = Deck::new(44100);
        deck.set_keylock(false);
        deck.load(Arc::new(LoadedTrack::new("t", audio).with_grid(BeatGrid::new(128.0, 0.0))));
        deck.seek(1.0);
        deck.play();
        deck.auto_loop(1.0);
        let p0 = deck.position();
        let _ = render(&mut deck, 165375);
        let p1 = deck.position();
        assert!((p1 - p0).abs() < 1e-9, "after 8 loops drift {} s", p1 - p0);
    }

    #[test]
    fn keylock_preserves_pitch() {
        let sr = 44100.0;
        let f0 = 440.0;
        let n = 44100 * 6;
        let sine: Vec<f32> = (0..n).map(|i| (2.0 * std::f64::consts::PI * f0 * i as f64 / sr).sin() as f32 * 0.5).collect();
        for (keylock, expect) in [(true, f0), (false, f0 * 1.08)] {
            let audio = Arc::new(buffer_from_mono(44100, &sine));
            let mut deck = Deck::new(44100);
            deck.set_keylock(keylock);
            deck.load(Arc::new(LoadedTrack::new("s", audio)));
            deck.set_pitch_range(PitchRange::Pct16);
            deck.set_tempo(1.08);
            assert!((deck.tempo() - 1.08).abs() < 1e-6);
            deck.seek(0.5);
            deck.play();
            let out = render(&mut deck, 44100 * 2);
            let p = deck.position();
            assert!((p - (0.5 + 2.0 * 1.08)).abs() < 0.01, "position {p}");
            let mono: Vec<f64> = out.iter().step_by(2).map(|v| *v as f64).skip(8000).collect();
            let f = estimate_freq(&mono, sr);
            assert!((f - expect).abs() / expect < 0.003, "keylock={keylock}: {f} Hz, expected {expect}");
        }
        // key shift +2 semitones with keylock at tempo 1.0
        let audio = Arc::new(buffer_from_mono(44100, &sine));
        let mut deck = Deck::new(48000);
        deck.load(Arc::new(LoadedTrack::new("s", audio)));
        deck.set_key_shift(2.0);
        deck.play();
        let out = render(&mut deck, 48000);
        let mono: Vec<f64> = out.iter().step_by(2).map(|v| *v as f64).skip(8000).collect();
        let f = estimate_freq(&mono, 48000.0);
        let e = f0 * 2f64.powf(2.0 / 12.0);
        assert!((f - e).abs() / e < 0.003, "key shift at 48k: {f} Hz, expected {e}");
    }

    /// Zero-crossing frequency estimate with linear interpolation.
    pub(crate) fn estimate_freq(x: &[f64], sr: f64) -> f64 {
        let mut crossings = Vec::new();
        for i in 1..x.len() {
            if x[i - 1] < 0.0 && x[i] >= 0.0 {
                let t = x[i - 1] / (x[i - 1] - x[i]);
                crossings.push(i as f64 - 1.0 + t);
            }
        }
        let k = crossings.len() - 1;
        k as f64 * sr / (crossings[k] - crossings[0])
    }

    #[test]
    fn keylock_jump_is_immediate_and_aligned() {
        // click track; jump with keylock on and check the next click comes when expected
        let n = 44100 * 10;
        let mut l = vec![0f32; n];
        for k in (0..n).step_by(22050) {
            for j in 0..32 {
                if k + j < n {
                    l[k + j] = 0.8;
                }
            }
        }
        let audio = Arc::new(buffer_from_mono(44100, &l));
        let mut deck = Deck::new(44100);
        deck.load(Arc::new(LoadedTrack::new("c", audio).with_grid(BeatGrid::new(120.0, 0.0))));
        deck.set_quantize(false);
        deck.seek(1.4); // next click at 1.5 s → 0.1 s = 4410 frames later
        deck.play();
        let out = render(&mut deck, 8000);
        let first = out.iter().step_by(2).position(|v| *v > 0.3).unwrap();
        assert!((first as i64 - 4410).abs() < 60, "first click at {first}");
        let q = deck.position();
        assert!((q - (1.4 + 8000.0 / 44100.0)).abs() < 1e-6);
    }

    #[test]
    fn hot_cue_quantized_jump_and_preview() {
        let n = 44100 * 10;
        let l = noise(n, 5);
        let audio = Arc::new(buffer_from_mono(44100, &l));
        let mut deck = Deck::new(44100);
        deck.set_keylock(false);
        deck.load(Arc::new(LoadedTrack::new("t", audio).with_grid(BeatGrid::new(120.0, 0.0))));
        deck.set_hot_cue(0, Some(4.0));
        deck.seek(1.1);
        deck.play();
        deck.hot_cue_down(0);
        // waits for the next beat (1.5 s): 0.4 s later
        let _ = render(&mut deck, 17000);
        assert!(deck.position() < 1.5 && deck.position() > 1.1);
        let _ = render(&mut deck, 1000);
        // jumped at 1.5 → now 4.0 + (18000 - 17640)/44100
        let e = 4.0 + (18000.0 - 0.4 * 44100.0) / 44100.0;
        assert!((deck.position() - e).abs() < 1e-6, "pos {} expected {e}", deck.position());
        deck.hot_cue_up(0);
        // preview while stopped
        deck.pause();
        deck.hot_cue_down(0);
        assert!(deck.is_playing());
        let _ = render(&mut deck, 4410);
        deck.hot_cue_up(0);
        assert!(!deck.is_playing());
        assert!((deck.position() - 4.0).abs() < 1e-9);
    }

    #[test]
    fn cdj_cue_logic() {
        let audio = Arc::new(buffer_from_mono(44100, &noise(44100 * 4, 3)));
        let mut deck = Deck::new(44100);
        deck.load(Arc::new(LoadedTrack::new("t", audio)));
        deck.set_quantize(false);
        deck.seek(1.0);
        deck.cue_down(); // paused away from cue → set cue
        deck.cue_up();
        assert!((deck.cue_point() - 1.0).abs() < 1e-9);
        deck.cue_down(); // on cue → preview
        assert!(deck.is_playing());
        let _ = render(&mut deck, 2000);
        deck.cue_up(); // back to cue, paused
        assert!(!deck.is_playing() && (deck.position() - 1.0).abs() < 1e-9);
        deck.play();
        let _ = render(&mut deck, 2000);
        deck.cue_down(); // playing → back to cue + stop
        assert!(!deck.is_playing() && (deck.position() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn loop_roll_slips() {
        let audio = Arc::new(buffer_from_mono(44100, &noise(44100 * 10, 9)));
        let mut deck = Deck::new(44100);
        deck.set_keylock(false);
        deck.load(Arc::new(LoadedTrack::new("t", audio).with_grid(BeatGrid::new(120.0, 0.0))));
        deck.seek(1.0);
        deck.play();
        deck.loop_roll(0.25, true);
        let _ = render(&mut deck, 44100);
        assert!(deck.position() >= 1.0 && deck.position() < 1.125 + 1e-9);
        deck.loop_roll(0.25, false);
        assert!((deck.position() - 2.0).abs() < 0.01, "slip resume at {}", deck.position());
    }
}
