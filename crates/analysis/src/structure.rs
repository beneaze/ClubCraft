//! Structure analysis: bar-level features, phrase-aligned segmentation and section labelling.
//!
//! * **Bar features** (one row per bar of the grid): mix loudness, four band energies
//!   (low/low-mid/mid/high), onset density and four *stem activities* in 0..1. With stems the
//!   activities are the per-stem RMS relative to the stem's own loud level; without stems they
//!   are estimated: drums from percussive onset strength, bass from low-band energy *between*
//!   the kicks (the off-beat bass of house), melody from the mid band; vocals are unknown.
//! * **Segmentation**: dynamic programming over bars minimising within-section squared error
//!   plus a per-section cost; boundaries on 8-bar phrase lines (relative to the downbeat) are
//!   free, 4-bar lines cost a little and other bars a lot, so sections follow the 8/16/32-bar
//!   phrasing of dance music.
//! * **Labelling** from section activities and relative energy: Intro (leading sections before
//!   the first full-energy part), Drop (drums + bass + high energy, after a breakdown/build or
//!   the first full section), Breakdown (drums or bass mostly absent), Buildup (the last bars
//!   before a drop when energy/onset density rises), Verse (vocals or mid energy), Outro
//!   (trailing sections after the last drop with reduced energy).

use crate::dsp;
use crate::frontend::N_MELS;
use clubcraft_core::{BeatGrid, Section, SectionKind, StemKind};

/// Per-bar measurements, `n` bars starting at grid bar `first_bar`.
#[derive(Debug, Clone, Default)]
pub struct BarFeatures {
    pub first_bar: i32,
    /// Grid bar index where phrases are counted from: the first bar with music (normally 0;
    /// -1 when the music's first downbeat lies a few ms before t = 0).
    pub phrase_origin: i32,
    pub start: Vec<f64>,
    /// Stem activity 0..1 in StemKind order.
    pub stems: Vec<[f32; 4]>,
    /// True when the activities come from real stems (vocals are meaningful).
    pub has_stems: bool,
    /// Mix loudness per bar, dB relative to the track's loud level (p95), clamped to -60.
    pub level_db: Vec<f32>,
    /// Band energies (dB rel. p95): low <150 Hz, low-mid 150-600, mid 600-4k, high >4k.
    pub bands_db: Vec<[f32; 4]>,
    /// Onset density (relative to the track's p90).
    pub density: Vec<f32>,
    /// Coarse timbre: mean log-mel magnitude of 8 band groups relative to their loud level.
    pub timbre: Vec<[f32; 8]>,
    /// Relative energy 0..1.
    pub energy: Vec<f32>,
}

impl BarFeatures {
    pub fn len(&self) -> usize {
        self.start.len()
    }
    pub fn is_empty(&self) -> bool {
        self.start.is_empty()
    }
    pub fn bar_index(&self, i: usize) -> i32 {
        self.first_bar + i as i32
    }
}

fn db(x: f64) -> f32 {
    (10.0 * x.max(1e-12).log10()) as f32
}

/// Mean square of a stereo stem over `a..b`.
fn ms_stereo(l: &[f32], r: &[f32], a: usize, b: usize) -> f64 {
    let b = b.min(l.len()).min(r.len());
    if b <= a {
        return 0.0;
    }
    let mut s = 0.0f64;
    for i in a..b {
        let m = 0.5 * (l[i] + r[i]);
        s += (m * m) as f64;
    }
    s / (b - a) as f64
}

pub(crate) fn bar_features(prep: &crate::Prepared, grid: &BeatGrid) -> BarFeatures {
    let sr = prep.sr;
    let dur = prep.duration();
    let spec = &prep.mix;
    let fps = spec.fps;
    let first_bar = grid.bar_at(0.0).floor() as i32;
    let last_bar = grid.bar_at(dur).ceil() as i32;
    let n = (last_bar - first_bar).max(0) as usize;
    if n == 0 {
        return BarFeatures::default();
    }
    let starts: Vec<f64> = (0..n).map(|i| grid.time_of_bar((first_bar + i as i32) as f64)).collect();
    let span = |i: usize| -> (f64, f64) { (starts[i].max(0.0), (starts[i] + grid.bar_len()).min(dur)) };

    // Mix level.
    let level: Vec<f32> = (0..n)
        .map(|i| {
            let (a, b) = span(i);
            let (a, b) = ((a * sr) as usize, (b * sr) as usize);
            let b = b.min(prep.mono.len());
            if b <= a {
                return -120.0;
            }
            db(prep.mono[a..b].iter().map(|v| (v * v) as f64).sum::<f64>() / (b - a) as f64)
        })
        .collect();
    // Band energies from the mel front end.
    let band_ranges = [spec.bands(0.0, 150.0), spec.bands(150.0, 600.0), spec.bands(600.0, 4000.0), spec.bands(4000.0, 20000.0)];
    let frame_range = |a: f64, b: f64| -> (usize, usize) {
        let fa = ((a * fps).round() as usize).min(spec.n_frames);
        let fb = ((b * fps).round() as usize).min(spec.n_frames);
        (fa, fb.max(fa))
    };
    let band_e = |a: f64, b: f64| -> [f32; 4] {
        let (fa, fb) = frame_range(a, b);
        let mut o = [0.0f32; 4];
        if fb <= fa {
            return [-120.0; 4];
        }
        for (k, r) in band_ranges.iter().enumerate() {
            let mut s = 0.0f64;
            for i in fa..fb {
                let fr = spec.frame(i);
                for bnd in r.clone() {
                    s += (fr[bnd] * fr[bnd]) as f64;
                }
            }
            o[k] = db(s / ((fb - fa) * r.len().max(1)) as f64);
        }
        o
    };
    let bands: Vec<[f32; 4]> = (0..n).map(|i| {
        let (a, b) = span(i);
        band_e(a, b)
    }).collect();
    // Coarse timbre (8 groups of 5 mel bands).
    let mut timbre: Vec<[f32; 8]> = (0..n)
        .map(|i| {
            let (a, b) = span(i);
            let (fa, fb) = frame_range(a, b);
            let mut o = [0.0f32; 8];
            if fb > fa {
                for f in fa..fb {
                    let fr = &prep.mix_log[f * N_MELS..(f + 1) * N_MELS];
                    for g in 0..8 {
                        o[g] += fr[g * 5..(g + 1) * 5].iter().sum::<f32>() / 5.0;
                    }
                }
                for v in &mut o {
                    *v /= (fb - fa) as f32;
                }
            }
            o
        })
        .collect();
    for g in 0..8 {
        let col: Vec<f32> = timbre.iter().map(|t| t[g]).collect();
        let r = dsp::percentile(&col, 95.0);
        for t in timbre.iter_mut() {
            t[g] = ((t[g] - r) / 1.4).max(-4.0);
        }
    }
    // Onset density (full band flux on the mix).
    let onset = spec.onset(&prep.mix_log, 0..N_MELS);
    let dens: Vec<f32> = (0..n)
        .map(|i| {
            let (a, b) = span(i);
            let (fa, fb) = frame_range(a, b);
            if fb <= fa { 0.0 } else { dsp::mean(&onset[fa..fb]) }
        })
        .collect();

    // Stem activities.
    let mut stems_db: Vec<[f32; 4]> = vec![[-120.0; 4]; n];
    let has_stems = prep.stems.is_some();
    if let Some(st) = prep.stems {
        for (i, row) in stems_db.iter_mut().enumerate() {
            let (a, b) = span(i);
            let (a, b) = ((a * sr) as usize, (b * sr) as usize);
            for k in StemKind::ALL {
                let (l, r) = st.get(k);
                row[k.index()] = db(ms_stereo(l, r, a, b));
            }
        }
    } else {
        // Estimated activities. Drums: percussive onset strength in the high band and kick band.
        let hi_on = spec.onset(&prep.mix_log, spec.bands(2000.0, 20000.0));
        let lo_on = spec.onset(&prep.mix_log, spec.bands(0.0, 150.0));
        let beat = grid.beat_len();
        let low = band_ranges[0].clone();
        for (i, row) in stems_db.iter_mut().enumerate() {
            let (a, b) = span(i);
            let (fa, fb) = frame_range(a, b);
            if fb <= fa {
                continue;
            }
            let d = dsp::mean(&hi_on[fa..fb]) + dsp::mean(&lo_on[fa..fb]);
            row[0] = 20.0 * (d.max(1e-6)).log10();
            // Bass: low band energy in the second half of each beat (between kicks).
            let mut s = 0.0f64;
            let mut c = 0usize;
            for q in 0..4 {
                let t0 = starts[i] + q as f64 * beat;
                let (ga, gb) = frame_range((t0 + 0.5 * beat).max(0.0), (t0 + 0.95 * beat).min(dur));
                for f in ga..gb {
                    let fr = spec.frame(f);
                    for bnd in low.clone() {
                        s += (fr[bnd] * fr[bnd]) as f64;
                    }
                    c += low.len();
                }
            }
            row[1] = if c > 0 { db(s / c as f64) } else { -120.0 };
            row[2] = bands[i][2];
            row[3] = -120.0;
        }
    }
    // Reference levels (loud parts of the track) and relative values.
    let valid: Vec<usize> = (0..n).filter(|&i| level[i] > -100.0).collect();
    let refp = |vals: &dyn Fn(usize) -> f32| -> f32 {
        let v: Vec<f32> = valid.iter().map(|&i| vals(i)).collect();
        dsp::percentile(&v, 95.0)
    };
    let lref = refp(&|i| level[i]);
    let bref: Vec<f32> = (0..4).map(|k| refp(&|i| bands[i][k])).collect();
    let sref: Vec<f32> = (0..4).map(|k| refp(&|i| stems_db[i][k])).collect();
    let dref = dsp::percentile(&dens, 90.0).max(1e-6);
    let mut out = BarFeatures { first_bar, start: starts.clone(), has_stems, timbre, ..Default::default() };
    out.phrase_origin = (0..n)
        .find(|&i| starts[i] >= -0.03 && level[i] - lref > -40.0)
        .map(|i| first_bar + i as i32)
        .unwrap_or(0);
    for i in 0..n {
        let lv = (level[i] - lref).max(-60.0);
        out.level_db.push(lv);
        let mut bd = [0.0f32; 4];
        for k in 0..4 {
            bd[k] = (bands[i][k] - bref[k]).max(-60.0);
        }
        out.bands_db.push(bd);
        out.density.push((dens[i] / dref).min(1.5));
        let mut act = [0.0f32; 4];
        // (lo, hi): dB below the stem's own loud level (p95) mapped to activity 0 and 1.
        const STEM_RANGE: [(f32, f32); 4] = [(-18.0, -6.0), (-18.0, -6.0), (-16.0, -5.0), (-20.0, -8.0)];
        const MIX_RANGE: [(f32, f32); 4] = [(-12.0, -3.0), (-14.0, -5.0), (-10.0, -2.0), (-20.0, -8.0)];
        for k in 0..4 {
            if !has_stems && k == 3 {
                continue;
            }
            let rel = stems_db[i][k] - sref[k];
            act[k] = if has_stems {
                let (lo, hi) = STEM_RANGE[k];
                // Also require the stem to be audible relative to the mix (Demucs bleed).
                let mix_rel = stems_db[i][k] - level[i];
                ((rel - lo) / (hi - lo)).clamp(0.0, 1.0) * ((mix_rel + 28.0) / 10.0).clamp(0.0, 1.0)
            } else {
                let (lo, hi) = MIX_RANGE[k];
                ((rel - lo) / (hi - lo)).clamp(0.0, 1.0)
            };
        }
        out.stems.push(act);
        let e = 0.5 * ((lv + 15.0) / 15.0).clamp(0.0, 1.0) + 0.125 * (act[0] + act[1]) + 0.25 * (dens[i] / dref).min(1.0);
        out.energy.push(e.clamp(0.0, 1.0));
    }
    out
}

/// Feature vector of a bar for segmentation.
fn seg_vector(b: &BarFeatures, i: usize) -> Vec<f32> {
    let mut v = Vec::with_capacity(10);
    let s = b.stems[i];
    v.extend_from_slice(&[s[0] * 1.5, s[1] * 1.5, s[2]]);
    if b.has_stems {
        v.push(s[3]);
    }
    for k in 0..4 {
        v.push((b.bands_db[i][k] / 12.0).max(-3.0));
    }
    v.push(b.density[i].min(1.2));
    let w: f32 = std::env::var("CC_TIMBRE_W").ok().and_then(|v| v.parse().ok()).unwrap_or(0.5);
    for g in 0..8 {
        v.push(w * b.timbre[i][g]);
    }
    v
}

/// Section boundaries (bar indices into `b`, including 0 and n) by phrase-aware DP.
pub(crate) fn boundaries(b: &BarFeatures, lambda: f32) -> Vec<usize> {
    let n = b.len();
    if n == 0 {
        return vec![0];
    }
    let feats: Vec<Vec<f32>> = (0..n).map(|i| seg_vector(b, i)).collect();
    let d = feats[0].len();
    // Prefix sums for O(1) SSE.
    let mut ps = vec![vec![0.0f64; d]; n + 1];
    let mut pq = vec![0.0f64; n + 1];
    for i in 0..n {
        for k in 0..d {
            ps[i + 1][k] = ps[i][k] + feats[i][k] as f64;
        }
        pq[i + 1] = pq[i] + feats[i].iter().map(|x| (*x as f64).powi(2)).sum::<f64>();
    }
    let sse = |a: usize, e: usize| -> f64 {
        let m = (e - a) as f64;
        let mut s = pq[e] - pq[a];
        for k in 0..d {
            let t = ps[e][k] - ps[a][k];
            s -= t * t / m;
        }
        s.max(0.0)
    };
    let lambda = lambda as f64;
    // Cost of placing a boundary at bar index i (absolute grid bar = first_bar + i).
    let bcost = |i: usize| -> f64 {
        if i == 0 || i == n {
            return 0.0;
        }
        let g = b.bar_index(i) - b.phrase_origin;
        if g.rem_euclid(8) == 0 {
            0.0
        } else if g.rem_euclid(4) == 0 {
            0.3 * lambda
        } else if g.rem_euclid(2) == 0 {
            3.0 * lambda
        } else {
            4.0 * lambda
        }
    };
    let mut best = vec![f64::INFINITY; n + 1];
    let mut prev = vec![0usize; n + 1];
    best[0] = 0.0;
    for e in 1..=n {
        for a in 0..e {
            let len = e - a;
            // Sections are at least 4 bars, except a partial first/last bar region.
            if len < 4 && !(a == 0 || e == n) {
                continue;
            }
            let c = best[a] + sse(a, e) + lambda + bcost(e);
            if c < best[e] {
                best[e] = c;
                prev[e] = a;
            }
        }
    }
    let mut out = vec![n];
    let mut e = n;
    while e > 0 {
        e = prev[e];
        out.push(e);
    }
    out.reverse();
    out
}

#[derive(Debug, Clone, Copy)]
struct Seg {
    a: usize,
    e: usize,
    stems: [f32; 4],
    energy: f32,
    #[allow(dead_code)]
    level: f32,
    #[allow(dead_code)]
    density: f32,
}

impl Seg {
    fn new(b: &BarFeatures, a: usize, e: usize) -> Seg {
        let m = (e - a).max(1) as f32;
        let mut st = [0.0f32; 4];
        let (mut en, mut lv, mut de) = (0.0, 0.0, 0.0);
        for i in a..e {
            for k in 0..4 {
                st[k] += b.stems[i][k] / m;
            }
            en += b.energy[i] / m;
            lv += b.level_db[i] / m;
            de += b.density[i] / m;
        }
        Seg { a, e, stems: st, energy: en, level: lv, density: de }
    }
}

/// Bar features of a track for a given grid (public entry for [`segment`] / cue generation).
pub fn bar_features_for(audio: &clubcraft_audio::AudioBuffer, stems: Option<crate::StemsRef>, grid: &BeatGrid) -> BarFeatures {
    let prep = crate::Prepared::new(audio, stems, false);
    bar_features(&prep, grid)
}

/// Public entry: sections of a track given its grid.
pub fn sections(audio: &clubcraft_audio::AudioBuffer, stems: Option<crate::StemsRef>, grid: &BeatGrid) -> Vec<Section> {
    let prep = crate::Prepared::new(audio, stems, false);
    let bars = bar_features(&prep, grid);
    segment(&bars, grid, audio.duration())
}

/// Segment and label bars into sections.
pub fn segment(b: &BarFeatures, grid: &BeatGrid, duration: f64) -> Vec<Section> {
    let n = b.len();
    if n < 4 {
        return vec![];
    }
    let _ = grid;
    let lam: f32 = std::env::var("CC_SEG_LAMBDA").ok().and_then(|v| v.parse().ok()).unwrap_or(2.0);
    let mut bounds = boundaries(b, lam);
    // A partial lead-in bar (before the first downbeat) belongs to the first section.
    if bounds.len() > 2 && bounds[1] == 1 && b.start[0] < 0.0 {
        bounds.remove(1);
    }
    // Likewise a trailing fragment of <= 2 bars belongs to the last section.
    let m = bounds.len();
    if m > 2 && bounds[m - 1] - bounds[m - 2] <= 2 {
        bounds.remove(m - 2);
    }
    let mut segs: Vec<Seg> = bounds.windows(2).map(|w| Seg::new(b, w[0], w[1])).collect();
    let emax = segs.iter().filter(|s| s.e - s.a >= 2).map(|s| s.energy).fold(0.0f32, f32::max).max(1e-6);
    let kinds = label(&mut segs, emax, b);
    let mut out: Vec<Section> = Vec::new();
    for (s, k) in segs.iter().zip(kinds) {
        let start = b.start[s.a].max(0.0);
        let end = if s.e >= n { duration } else { b.start[s.e] };
        if end <= start {
            continue;
        }
        let sec = Section {
            kind: k,
            start,
            end,
            start_bar: b.bar_index(s.a),
            bars: (s.e - s.a) as u32,
            energy: (s.energy / emax).clamp(0.0, 1.0),
            stems: s.stems,
        };
        // Merge adjacent intro / build / outro parts (drops, breaks and verses keep their
        // phrase boundaries).
        if let Some(last) = out.last_mut() {
            if last.kind == sec.kind && matches!(sec.kind, SectionKind::Intro | SectionKind::Outro | SectionKind::Buildup) {
                let (l1, l2) = (last.bars as f32, sec.bars as f32);
                let t = (l1 + l2).max(1.0);
                last.energy = (last.energy * l1 + sec.energy * l2) / t;
                for k in 0..4 {
                    last.stems[k] = (last.stems[k] * l1 + sec.stems[k] * l2) / t;
                }
                last.end = sec.end;
                last.bars += sec.bars;
                continue;
            }
        }
        out.push(sec);
    }
    out
}

fn label(segs: &mut Vec<Seg>, emax: f32, b: &BarFeatures) -> Vec<SectionKind> {
    use SectionKind::*;
    let rel = |s: &Seg| s.energy / emax;
    let drums = |s: &Seg| s.stems[0];
    let bass = |s: &Seg| s.stems[1];
    let full = |s: &Seg| drums(s) >= 0.6 && bass(s) >= 0.6 && rel(s) >= 0.85;
    let low = |s: &Seg| drums(s) < 0.35 || bass(s) < 0.3;
    let n = segs.len();
    let mut kinds = vec![Verse; n];
    for i in 0..n {
        if full(&segs[i]) {
            kinds[i] = Drop;
        }
    }
    if !kinds.contains(&Drop) {
        // No clear full-energy part: the loudest groove sections are the drops.
        for i in 0..n {
            if drums(&segs[i]) >= 0.5 && rel(&segs[i]) >= 0.9 {
                kinds[i] = Drop;
            }
        }
    }
    for i in 0..n {
        if kinds[i] != Drop && low(&segs[i]) {
            kinds[i] = Breakdown;
        }
    }
    // The opening section is the intro, even when the track starts at full energy.
    if n > 2 && segs[0].e - segs[0].a <= 32 {
        kinds[0] = Intro;
    }
    let first_drop = kinds.iter().position(|k| *k == Drop);
    let last_drop = kinds.iter().rposition(|k| *k == Drop);
    // Intro: everything before the first drop, up to the point where the drums drop out again
    // after having been present (intro groove -> breakdown -> drop).
    let fd = first_drop.unwrap_or(n.min(1));
    let mut had_drums = false;
    for i in 0..fd {
        let s = &segs[i];
        if had_drums && drums(s) < 0.35 && i > 0 {
            break;
        }
        had_drums |= drums(s) >= 0.5;
        kinds[i] = Intro;
    }
    // Outro: trailing sections after the last drop with less energy or without bass.
    if let Some(ld) = last_drop {
        for i in (ld + 1)..n {
            if rel(&segs[i]) < rel(&segs[ld]) - 0.03 || bass(&segs[i]) < 0.5 || drums(&segs[i]) < 0.5 {
                kinds[i] = Outro;
            }
        }
        // A sole trailing "verse" between outros becomes outro too.
        let mut i = n;
        while i > ld + 1 && kinds[i - 1] == Outro {
            i -= 1;
        }
        for k in kinds.iter_mut().take(n).skip(i) {
            *k = Outro;
        }
    }
    // Buildups (bar level) before each drop that follows a non-drop part.
    let nb = b.len();
    let mut bar_kind: Vec<SectionKind> = vec![Verse; nb];
    let mut bar_seg: Vec<usize> = vec![0; nb];
    for (si, sg) in segs.iter().enumerate() {
        for j in sg.a..sg.e {
            bar_kind[j] = kinds[si];
            bar_seg[j] = si;
        }
    }
    let drum = |j: usize| b.stems[j][0];
    let mut j = 1;
    while j < nb {
        if bar_kind[j] == Drop && bar_kind[j - 1] != Drop {
            let drop_bar = j;
            let mut region_start = drop_bar;
            while region_start > 0 && bar_kind[region_start - 1] != Drop {
                region_start -= 1;
            }
            // Drums-back tail directly before the drop (tolerating the classic 1-2 bar
            // drop-out right before the drop).
            let mut end = drop_bar;
            while end > region_start && drop_bar - end < 2 && drum(end - 1) < 0.3 {
                end -= 1;
            }
            let mut tail = drop_bar;
            for t in (region_start..end).rev() {
                let e = (t + 4).min(end);
                let m = (t..e).map(drum).sum::<f32>() / (e - t) as f32;
                if m >= 0.4 {
                    tail = t;
                } else {
                    break;
                }
            }
            // Snap to the 4-bar grid.
            let g = b.bar_index(tail) - b.phrase_origin;
            let snapped = tail as i64 - g as i64 + ((g as f64 / 4.0).round() as i64) * 4;
            let tail = (snapped.max(region_start as i64) as usize).min(drop_bar);
            let len = drop_bar - tail;
            let drumless = (region_start..tail).filter(|&t| drum(t) < 0.3).count();
            let bass_tail = if len > 0 { (tail..drop_bar).map(|t| b.stems[t][1]).sum::<f32>() / len as f32 } else { 1.0 };
            let in_intro = bar_kind[tail..drop_bar].iter().all(|k| *k == Intro);
            let intro_ok = !in_intro || (drop_bar - region_start >= 16 && len <= 8);
            let mut build: Option<usize> = None;
            if (4..=16).contains(&len) && drumless >= 4 && bass_tail < 0.5 && intro_ok {
                build = Some(tail);
            } else if len < 4 && drop_bar - region_start >= 12 && !in_intro {
                // Drumless breakdown straight into the drop: the last 4/8 bars are the build
                // if they get denser / brighter (snare roll, riser).
                let rl = drop_bar - region_start;
                let tl = if rl >= 24 { 8 } else { 4 };
                let cut = drop_bar - tl;
                let act = |a: usize, e: usize| {
                    (a..e).map(|q| b.density[q] + 0.03 * b.bands_db[q][3] + 0.5 * b.stems[q][0]).sum::<f32>() / (e - a) as f32
                };
                if act(cut, drop_bar) > act(region_start, cut) + 0.12 {
                    build = Some(cut);
                }
            }
            if let Some(st) = build {
                for k in bar_kind.iter_mut().take(drop_bar).skip(st) {
                    *k = Buildup;
                }
            }
        }
        j += 1;
    }
    // Rebuild the segment list from the bar labels.
    let mut new_segs: Vec<Seg> = Vec::new();
    let mut new_kinds: Vec<SectionKind> = Vec::new();
    let mut a0 = 0;
    for j in 1..=nb {
        if j == nb || bar_kind[j] != bar_kind[a0] || bar_seg[j] != bar_seg[a0] {
            new_segs.push(Seg::new(b, a0, j));
            new_kinds.push(bar_kind[a0]);
            a0 = j;
        }
    }
    *segs = new_segs;
    let kinds = new_kinds;
    kinds
}
