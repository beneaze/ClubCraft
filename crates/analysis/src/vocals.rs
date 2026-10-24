//! Where a track really sings, at 16th-note resolution, and which of its vocal phrases are worth
//! lifting into a mashup ([`crate::mashup`]).
//!
//! The map ([`compute`]), per 16th note of the beat grid:
//!
//! * **Level** of the vocal stem relative to its loud level (p95).
//! * **Singing**: loud enough and not leakage. Separation is not clean: lead synths, pads and
//!   hi-hats leak into the vocal stem 10-25 dB down. Leakage is a quieter copy of another stem's
//!   spectrum, while a voice has harmonics and formants of its own, so every STFT frame compares
//!   the vocal stem's spectrum (64 semitone bands, 150 Hz to 6 kHz) with the melody and the drum
//!   stem.
//! * **Timbre**: a coarse, level-free spectral shape of the singing, to find repeats.
//!
//! A phrase ([`VocalMap::phrase`]) is judged the way a DJ picks an acapella:
//!
//! * **Lines**: reverb keeps the vocal stem from ever going quiet, so the breaks between sung
//!   lines are dips against the surrounding beats. A phrase should start where a line starts (a
//!   pickup of up to two beats before the bar, or an entry up to two beats after it, is fine)
//!   and end in a break, not in the middle of a line.
//! * **Lyrical, not a loop**: vocal chops, and instruments the separation put into the vocal
//!   stem, repeat every bar or two. Sung lines don't, so a phrase that matches itself one or two
//!   bars later is not the one to pick.
//! * **Hook**: the chorus comes back elsewhere in the track (in dance music often as the very
//!   same recording); a phrase that has a near-copy elsewhere is the hook.

use crate::dsp;
use crate::StemsRef;
use clubcraft_core::{BeatGrid, StemKind};

/// Bumped when the map changes.
pub const VERSION: u32 = 1;
/// Grid steps per beat (16th notes).
pub const STEPS_PER_BEAT: i32 = 4;
pub const TIMBRE_BANDS: usize = 16;

const BANDS: usize = 64;
const F_LO: f64 = 150.0;
const N_FFT: usize = 2048;
const HOP: usize = 512;
const SR: f64 = clubcraft_core::SAMPLE_RATE as f64;
const MAGIC: &[u8; 4] = b"CCVM";
const FLOOR_DB: f32 = -60.0;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct VocalMap {
    pub version: u32,
    /// Grid the steps refer to.
    pub bpm: f64,
    pub downbeat: f64,
    pub beats_per_bar: u32,
    /// Grid step (16th note; step 0 = the grid anchor) of the first entry.
    pub first_step: i32,
    /// Per step: vocal stem level, dB relative to its loud level (p95), floored at -60.
    pub level: Vec<f32>,
    /// Per step: how surely the vocal stem sings (0..1).
    pub sung: Vec<f32>,
    /// Per step: spectral shape of the singing (mean-free log band energies, unit length; zero
    /// where nothing is sung).
    pub timbre: Vec<[f32; TIMBRE_BANDS]>,
}

/// How good a vocal phrase is, each part 0..1.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PhraseScore {
    /// Share of the phrase that is sung.
    pub cover: f32,
    /// A line starts with the phrase (1), the singing starts late (less), or the phrase cuts
    /// into a line (0).
    pub start: f32,
    /// The phrase ends in a break between lines (1) or cuts a line off (0).
    pub end: f32,
    /// Not a loop: 0 when the phrase repeats itself every bar or two.
    pub lyrical: f32,
    /// How closely the phrase comes back elsewhere in the track.
    pub hook: f32,
    pub quality: f32,
}

fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn dot(a: &[f32; TIMBRE_BANDS], b: &[f32; TIMBRE_BANDS]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

impl VocalMap {
    pub fn grid(&self) -> BeatGrid {
        BeatGrid { bpm: self.bpm, downbeat: self.downbeat, beats_per_bar: self.beats_per_bar }
    }

    pub fn steps_per_bar(&self) -> i32 {
        self.beats_per_bar as i32 * STEPS_PER_BEAT
    }

    /// Step where `bar` starts.
    pub fn step_of_bar(&self, bar: i32) -> i32 {
        bar * self.steps_per_bar()
    }

    fn idx(&self, step: i32) -> Option<usize> {
        usize::try_from(step - self.first_step).ok().filter(|j| *j < self.sung.len())
    }

    pub fn sung_at(&self, step: i32) -> f32 {
        self.idx(step).map(|j| self.sung[j]).unwrap_or(0.0)
    }

    pub fn level_at(&self, step: i32) -> f32 {
        self.idx(step).map(|j| self.level[j]).unwrap_or(FLOOR_DB)
    }

    /// Share of bar `bar` that is sung (0..1).
    pub fn bar_sung(&self, bar: i32) -> f32 {
        let s = self.step_of_bar(bar);
        (s..s + self.steps_per_bar()).map(|k| self.sung_at(k)).sum::<f32>() / self.steps_per_bar().max(1) as f32
    }

    /// A break between lines at `step`: this 16th and the next are quiet ([`VocalMap::is_quiet`]).
    /// A single quiet 16th is a consonant or a staccato note, not a breath.
    pub fn is_break(&self, step: i32) -> bool {
        self.is_quiet(step) && (self.is_quiet(step + 1) || self.is_quiet(step - 1))
    }

    /// Nothing sung at `step`, or a dip of 9 dB under the loudest singing of the surrounding beat
    /// and a half (reverb fills the gaps between lines, but much quieter).
    pub fn is_quiet(&self, step: i32) -> bool {
        if self.sung_at(step) < 0.25 {
            return true;
        }
        let around = (step - 6..=step + 6).map(|k| self.level_at(k)).fold(FLOOR_DB, f32::max);
        self.level_at(step) < around - 9.0
    }

    /// Similarity of the singing in `len` steps from `a` and from `b` (0..1): same rhythm and the
    /// same sound.
    fn similarity(&self, a: i32, b: i32, len: i32) -> f32 {
        let (mut s, mut n) = (0f32, 0f32);
        for k in 0..len {
            let (Some(i), Some(j)) = (self.idx(a + k), self.idx(b + k)) else {
                n += self.sung_at(a + k);
                continue;
            };
            let (sa, sb) = (self.sung[i], self.sung[j]);
            s += sa.min(sb) * dot(&self.timbre[i], &self.timbre[j]).max(0.0);
            n += sa.max(sb);
        }
        if n > 0.0 {
            s / n
        } else {
            0.0
        }
    }

    /// A sung line starts at `step`: a break before it, and singing on it and on most of the
    /// beat from it (a sidechain pump ducks the 16th after each kick, so not every one).
    pub fn line_starts(&self, step: i32) -> bool {
        self.is_break(step - 1) && !self.is_quiet(step) && (step..step + STEPS_PER_BEAT).filter(|&k| !self.is_quiet(k)).count() >= 3
    }

    /// A sung passage starts within two beats of bar `bar`: a line starts there after a pause of
    /// at least a beat.
    pub fn passage_starts(&self, bar: i32) -> bool {
        let s = self.step_of_bar(bar);
        (s - 2 * STEPS_PER_BEAT..=s + 2 * STEPS_PER_BEAT).any(|k| self.line_starts(k) && (k - STEPS_PER_BEAT..k).all(|j| self.is_quiet(j)))
    }

    /// How cleanly the vocals enter at step `s` (0..1). A line may start up to two beats before
    /// the bar (a pickup) or up to two beats after it; later entries lose a little per beat. A
    /// line that is still running from before is a cut (0).
    fn entry(&self, s: i32) -> f32 {
        let b = STEPS_PER_BEAT;
        let near = (s - 2 * b..=s + 2 * b).filter(|&k| self.line_starts(k)).min_by_key(|&k| (k - s).abs());
        if let Some(k) = near {
            // singing between the bar and a later line start is the tail of the previous line
            let tail = (s..k).filter(|&j| !self.is_quiet(j)).count() as f32;
            let offset = if k < s { if s - k <= b { 1.0 } else { 0.9 } } else { 1.0 - 0.25 * (k - s) as f32 / b as f32 };
            return (offset * (1.0 - tail / (2 * b) as f32)).max(0.0);
        }
        if (s..s + 2).all(|k| self.is_quiet(k)) {
            // a pause at the bar: fine if the singing comes in within a bar or two
            let first = (s..s + 2 * self.steps_per_bar()).find(|&k| !self.is_quiet(k)).unwrap_or(s + 2 * self.steps_per_bar());
            return 0.8 * (1.0 - (first - s) as f32 / (2 * self.steps_per_bar()) as f32);
        }
        0.0
    }

    /// How cleanly the vocals stop at step `e` (0..1): the last line ends up to two beats before
    /// it or a beat after it (1), runs on up to two beats (0.5) or is cut off (0).
    fn exit(&self, e: i32) -> f32 {
        let b = STEPS_PER_BEAT;
        if (e - 2 * b..=e + b).any(|k| self.is_break(k)) {
            1.0
        } else if (e + b..=e + 2 * b).any(|k| self.is_break(k)) {
            0.5
        } else {
            0.0
        }
    }

    /// Score the phrase of `bars` bars from bar `start`.
    pub fn phrase(&self, start: i32, bars: usize) -> PhraseScore {
        let spb = self.steps_per_bar();
        let (s, e) = (self.step_of_bar(start), self.step_of_bar(start + bars as i32));
        let len = e - s;
        let cover = (s..e).map(|k| self.sung_at(k)).sum::<f32>() / len.max(1) as f32;
        let start_score = self.entry(s);
        let end_score = self.exit(e);
        // loops repeat after one or two bars; lyrics don't
        let lyrical = if len > 2 * spb {
            let lp = self.similarity(s, s + spb, len - spb).max(self.similarity(s, s + 2 * spb, len - 2 * spb));
            1.0 - smoothstep((lp - 0.45) / 0.35)
        } else {
            1.0
        };
        // the hook comes back elsewhere, whole-bar offsets, not overlapping
        let mut best = 0f32;
        let total = self.sung.len() as i32;
        let mut c = self.first_step.div_euclid(spb) * spb;
        while c + len <= self.first_step + total {
            if (c - s).abs() >= len {
                best = best.max(self.similarity(s, c, len));
            }
            c += spb;
        }
        let hook = smoothstep((best - 0.3) / 0.45);
        // cutting into a line or off at its end is what a listener notices first: those scale the
        // score instead of adding to it
        let quality = smoothstep(cover / 0.5) * (0.25 + 0.75 * lyrical) * (0.2 + 0.8 * start_score) * (0.5 + 0.5 * end_score) * (0.45 + 0.55 * hook);
        PhraseScore { cover, start: start_score, end: end_score, lyrical, hook, quality }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(40 + self.sung.len() * (2 + TIMBRE_BANDS) * 4);
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&self.version.to_le_bytes());
        b.extend_from_slice(&self.bpm.to_le_bytes());
        b.extend_from_slice(&self.downbeat.to_le_bytes());
        b.extend_from_slice(&self.beats_per_bar.to_le_bytes());
        b.extend_from_slice(&self.first_step.to_le_bytes());
        b.extend_from_slice(&(self.sung.len() as u32).to_le_bytes());
        for ((l, s), t) in self.level.iter().zip(&self.sung).zip(&self.timbre) {
            for v in [l, s].into_iter().chain(t) {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        b
    }

    pub fn from_bytes(b: &[u8]) -> Option<VocalMap> {
        let mut p = 0usize;
        let mut take = |n: usize| -> Option<&[u8]> {
            let s = b.get(p..p + n)?;
            p += n;
            Some(s)
        };
        if take(4)? != MAGIC {
            return None;
        }
        let version = u32::from_le_bytes(take(4)?.try_into().ok()?);
        let bpm = f64::from_le_bytes(take(8)?.try_into().ok()?);
        let downbeat = f64::from_le_bytes(take(8)?.try_into().ok()?);
        let beats_per_bar = u32::from_le_bytes(take(4)?.try_into().ok()?);
        let first_step = i32::from_le_bytes(take(4)?.try_into().ok()?);
        let n = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        let cap = n.min(1 << 20);
        let (mut level, mut sung, mut timbre) = (Vec::with_capacity(cap), Vec::with_capacity(cap), Vec::with_capacity(cap));
        for _ in 0..n {
            let raw = take((2 + TIMBRE_BANDS) * 4)?;
            let v: Vec<f32> = raw.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
            level.push(v[0]);
            sung.push(v[1]);
            let mut t = [0f32; TIMBRE_BANDS];
            t.copy_from_slice(&v[2..]);
            timbre.push(t);
        }
        Some(VocalMap { version, bpm, downbeat, beats_per_bar, first_step, level, sung, timbre })
    }
}

/// Magnitudes in semitone bands from `F_LO` up, per STFT frame.
fn band_frames(x: &[f32]) -> Vec<[f32; BANDS]> {
    let bin_hz = SR / N_FFT as f64;
    let map: Vec<(usize, usize)> = (1..N_FFT / 2)
        .filter_map(|k| {
            let f = k as f64 * bin_hz;
            let band = (12.0 * (f / F_LO).log2()).floor();
            (f >= F_LO && band < BANDS as f64).then_some((k, band as usize))
        })
        .collect();
    dsp::stft_map(x, N_FFT, HOP, |_, spec| {
        let mut p = [0f32; BANDS];
        for &(k, band) in &map {
            p[band] += spec[k].norm_sqr();
        }
        p.map(f32::sqrt)
    })
}

fn energy(b: &[f32; BANDS]) -> f32 {
    b.iter().map(|v| v * v).sum()
}

/// Spectral similarity of two frames and how many dB `s` is louder than `v`.
fn compare(v: &[f32; BANDS], s: &[f32; BANDS]) -> Option<(f32, f32)> {
    let (ev, es) = (energy(v), energy(s));
    if ev <= 1e-12 || es <= 1e-12 {
        return None;
    }
    Some((v.iter().zip(s).map(|(a, b)| a * b).sum::<f32>() / (ev * es).sqrt(), 10.0 * (es / ev).log10()))
}

/// How much frame `v` of the vocal stem looks like leakage from frame `s` of another stem (0..1).
fn leakage(v: &[f32; BANDS], s: &[f32; BANDS]) -> f32 {
    // the same spectrum at least 6 dB down starts to count, 18 dB down with a close match is leakage
    compare(v, s).map(|(cos, below_db)| smoothstep((cos - 0.6) / 0.3) * smoothstep((below_db - 6.0) / 12.0)).unwrap_or(0.0)
}

/// Per STFT frame (hop 512 at 44.1 kHz): vocal level (dB), then spectral similarity and level
/// gap (dB the other stem is louder) to the melody stem and to the drum stem. For calibration.
pub fn frame_stats(stems: &StemsRef) -> Vec<[f32; 5]> {
    let (v, o, d) = (band_frames(&stems.mono(StemKind::Vocals)), band_frames(&stems.mono(StemKind::Other)), band_frames(&stems.mono(StemKind::Drums)));
    v.iter()
        .zip(o.iter().zip(&d))
        .map(|(vb, (ob, db))| {
            let ((co, go), (cd, gd)) = (compare(vb, ob).unwrap_or((0.0, 0.0)), compare(vb, db).unwrap_or((0.0, 0.0)));
            [10.0 * energy(vb).max(1e-12).log10(), co, go, cd, gd]
        })
        .collect()
}

/// Compute the vocal map of a track from its separated stems.
pub fn compute(stems: &StemsRef, grid: BeatGrid, duration: f64) -> VocalMap {
    let (v, o, d) = (band_frames(&stems.mono(StemKind::Vocals)), band_frames(&stems.mono(StemKind::Other)), band_frames(&stems.mono(StemKind::Drums)));
    let level: Vec<f32> = v.iter().map(|b| 10.0 * energy(b).max(1e-12).log10()).collect();
    let loud = {
        let mut l: Vec<f32> = level.iter().copied().filter(|x| *x > -110.0).collect();
        l.sort_by(|a, b| a.total_cmp(b));
        l.get((l.len() as f32 * 0.95) as usize).copied().unwrap_or(-120.0)
    };
    let duration = duration.min(stems.frames() as f64 / SR);
    let first_step = (grid.beat_at(0.0) * STEPS_PER_BEAT as f64).ceil() as i32;
    let last_step = (grid.beat_at(duration) * STEPS_PER_BEAT as f64).floor() as i32;
    let n = (last_step - first_step).max(0) as usize;
    // per step: (sung, power, frames), timbre sums and their weight
    let mut acc = vec![(0f32, 0f32, 0usize); n];
    let mut tim = vec![([0f32; TIMBRE_BANDS], 0f32); n];
    for (i, vb) in v.iter().enumerate() {
        let t = (i * HOP) as f64 / SR;
        let step = (grid.beat_at(t) * STEPS_PER_BEAT as f64).floor() as i32 - first_step;
        let Some(j) = usize::try_from(step).ok().filter(|j| *j < n) else { continue };
        let gate = smoothstep((level[i] - (loud - 35.0)) / 15.0);
        let leak = o.get(i).map(|ob| leakage(vb, ob)).unwrap_or(0.0).max(d.get(i).map(|db| leakage(vb, db)).unwrap_or(0.0));
        let sung = gate * (1.0 - leak);
        acc[j].0 += sung;
        acc[j].1 += energy(vb);
        acc[j].2 += 1;
        if sung > 0.3 {
            for (k, t) in tim[j].0.iter_mut().enumerate() {
                let e: f32 = vb[k * 4..k * 4 + 4].iter().map(|x| x * x).sum();
                *t += sung * (e + 1e-10).log10();
            }
            tim[j].1 += sung;
        }
    }
    let frames = |c: usize| c.max(1) as f32;
    let level = acc.iter().map(|(_, p, c)| (10.0 * (p / frames(*c)).max(1e-12).log10() - loud).max(FLOOR_DB)).collect();
    let sung = acc.iter().map(|(s, _, c)| s / frames(*c)).collect();
    let timbre = tim
        .iter()
        .map(|(t, w)| {
            if *w <= 0.0 {
                return [0f32; TIMBRE_BANDS];
            }
            let mean = t.iter().sum::<f32>() / (TIMBRE_BANDS as f32 * w);
            let mut o = t.map(|x| x / w - mean);
            let norm = o.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 1e-6 {
                for x in &mut o {
                    *x /= norm;
                }
            }
            o
        })
        .collect();
    VocalMap { version: VERSION, bpm: grid.bpm, downbeat: grid.downbeat, beats_per_bar: grid.beats_per_bar, first_step, level, sung, timbre }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A map of `bars` 4/4 bars: `sung(step)` says where it sings, `timbre(step)` which of the
    /// unit vectors it sounds like.
    fn map(bars: i32, sung: impl Fn(i32) -> bool, timbre: impl Fn(i32) -> usize) -> VocalMap {
        let n = bars * 16;
        let mut m = VocalMap { version: VERSION, bpm: 124.0, downbeat: 0.0, beats_per_bar: 4, ..Default::default() };
        for k in 0..n {
            let on = sung(k);
            m.level.push(if on { -3.0 } else { -40.0 });
            m.sung.push(if on { 1.0 } else { 0.0 });
            let mut t = [0f32; TIMBRE_BANDS];
            if on {
                t[timbre(k) % TIMBRE_BANDS] = 1.0;
            }
            m.timbre.push(t);
        }
        m
    }

    /// Pseudo-random timbre: no two bars alike.
    fn varied(k: i32) -> usize {
        let mut x = (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        x ^= x >> 29;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        (x ^ (x >> 32)) as usize
    }

    #[test]
    fn phrases_start_and_end_with_a_line() {
        // silence for 4 bars, then 2-bar lines separated by an eighth-note breath
        let m = map(32, |k| k >= 64 && k % 32 < 30, varied);
        let clean = m.phrase(4, 8);
        assert_eq!((clean.start, clean.end), (1.0, 1.0), "{clean:?}");
        // a bar into a line: cuts in, and the end cuts a line off
        let cut = m.phrase(5, 8);
        assert_eq!((cut.start, cut.end), (0.0, 0.0), "{cut:?}");
        assert!(clean.quality > 3.0 * cut.quality, "{clean:?} {cut:?}");
        // a pause at the bar and the line a beat later still counts as a clean entry
        let late = map(32, |k| k >= 68 && k % 32 < 30, varied);
        assert!(late.phrase(4, 8).start > 0.7);
    }

    #[test]
    fn a_sidechain_pump_does_not_hide_a_line_start() {
        // the 16th after every kick is ducked, as in a pumping house vocal
        let m = map(32, |k| k >= 64 && k % 32 < 30 && k % 4 != 1, varied);
        assert_eq!(m.phrase(4, 8).start, 1.0, "{:?}", m.phrase(4, 8));
        assert!(m.passage_starts(4) && !m.passage_starts(5));
    }

    #[test]
    fn loops_are_not_lyrical() {
        let sung = |k: i32| k % 32 < 30;
        let chops = map(32, sung, |k| (k % 16) as usize);
        let lyrics = map(32, sung, varied);
        assert!(chops.phrase(8, 8).lyrical < 0.2, "{:?}", chops.phrase(8, 8));
        assert!(lyrics.phrase(8, 8).lyrical > 0.8, "{:?}", lyrics.phrase(8, 8));
    }

    #[test]
    fn the_hook_comes_back() {
        // bars 40-47 repeat bars 8-15; everything else is sung differently
        let m = map(64, |k| k % 32 < 30, |k| if (640..768).contains(&k) { varied(k - 512) } else { varied(k) });
        assert!(m.phrase(8, 8).hook > 0.8, "{:?}", m.phrase(8, 8));
        assert!(m.phrase(24, 8).hook < 0.2, "{:?}", m.phrase(24, 8));
    }

    #[test]
    fn bytes_roundtrip() {
        let m = map(4, |k| k % 3 == 0, varied);
        assert_eq!(VocalMap::from_bytes(&m.to_bytes()), Some(m));
    }
}
