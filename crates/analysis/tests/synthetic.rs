//! End-to-end tests on synthetic house-style signals.

use clubcraft_analysis::{
    analyze, beatgrid, key, waveform, AnalyzeOptions, AudioBuffer, OwnedStems,
};
use clubcraft_core::{CueKind, Key, StemTrack};

const SR: u32 = 44_100;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }
}

/// Four-on-the-floor groove: kick on every beat, clap on 2 and 4, open hat on the off-beats,
/// and (optionally) a sustained A-minor pad. `bars` bars at `bpm`, first downbeat at `t0`.
fn groove(bpm: f64, t0: f64, bars: usize, pad: bool) -> Vec<f32> {
    let beat = 60.0 / bpm;
    let n = ((t0 + bars as f64 * 4.0 * beat + 1.0) * SR as f64) as usize;
    let mut x = vec![0.0f32; n];
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for k in 0..bars * 4 {
        let tb = t0 + k as f64 * beat;
        let s0 = (tb * SR as f64).round() as usize;
        // Kick: pitch sweep 160 -> 50 Hz with exponential decay.
        let mut ph = 0.0f64;
        for i in 0..(0.25 * SR as f64) as usize {
            let t = i as f64 / SR as f64;
            let f = 50.0 + 110.0 * (-t * 30.0).exp();
            ph += 2.0 * std::f64::consts::PI * f / SR as f64;
            if s0 + i < n {
                x[s0 + i] += (0.8 * (-t * 12.0).exp() * ph.sin()) as f32;
            }
        }
        // Clap on beats 2 and 4.
        if k % 2 == 1 {
            for i in 0..(0.08 * SR as f64) as usize {
                let t = i as f32 / SR as f32;
                if s0 + i < n {
                    x[s0 + i] += 0.25 * (-t * 40.0).exp() * rng.next();
                }
            }
        }
        // Open hat on the off-beat (high-passed noise burst).
        let so = ((tb + 0.5 * beat) * SR as f64).round() as usize;
        let mut prev = 0.0f32;
        for i in 0..(0.1 * SR as f64) as usize {
            let t = i as f32 / SR as f32;
            let w = rng.next();
            let hp = w - prev;
            prev = w;
            if so + i < n {
                x[so + i] += 0.2 * (-t * 25.0).exp() * hp;
            }
        }
    }
    if pad {
        // A minor triad (A3 C4 E4) + A2 bass, from the first downbeat on.
        let notes = [110.0f32, 220.0, 261.63, 329.63];
        let s0 = (t0 * SR as f64) as usize;
        for (i, v) in x.iter_mut().enumerate().skip(s0) {
            let t = i as f32 / SR as f32;
            *v += notes
                .iter()
                .map(|f| 0.06 * (2.0 * std::f32::consts::PI * f * t).sin())
                .sum::<f32>();
        }
    }
    x
}

#[test]
fn beat_grid_constant_tempo() {
    for &(bpm, t0) in &[(124.0, 0.25), (128.0, 0.05), (122.0, 0.4)] {
        let x = groove(bpm, t0, 48, false);
        let audio = AudioBuffer::new(SR, x.clone(), x);
        let g = beatgrid::detect(&audio, None, &beatgrid::BeatOptions::default()).expect("grid");
        assert!((g.bpm - bpm).abs() < 0.01, "bpm {} vs {}", g.bpm, bpm);
        // Beat phase within 5 ms of the kick onsets.
        let ph = ((g.downbeat - t0) / g.beat_len()).rem_euclid(1.0);
        let err = ph.min(1.0 - ph) * g.beat_len();
        assert!(err < 0.005, "phase error {err} s (bpm {bpm})");
        // Bar phase: the first kick is the downbeat.
        let beats = ((g.downbeat - t0) / g.beat_len()).round() as i64;
        assert_eq!(
            beats.rem_euclid(4),
            0,
            "downbeat {} vs first kick {}",
            g.downbeat,
            t0
        );
    }
}

#[test]
fn key_a_minor() {
    let x = groove(124.0, 0.2, 32, true);
    let audio = AudioBuffer::new(SR, x.clone(), x);
    let k = key::detect(&audio, None).expect("key");
    // A minor or its relative C major.
    assert!(
        k == Key::new(9, true) || k == Key::new(0, false),
        "{}",
        k.name()
    );
}

#[test]
fn full_analysis_and_cues() {
    let x = groove(126.0, 0.1, 96, true);
    let audio = AudioBuffer::new(SR, x.clone(), x);
    let a = analyze(&audio, None, &AnalyzeOptions::default());
    assert_eq!(a.version, clubcraft_core::Analysis::CURRENT_VERSION);
    let g = a.grid.expect("grid");
    assert!((g.bpm - 126.0).abs() < 0.01);
    assert!(a.loudness_lufs.is_some() && a.gain_db.is_some());
    assert!((1..=10).contains(&a.energy.unwrap()));
    assert!(a.cues.iter().filter(|c| c.slot.is_some()).count() <= 8);
    let first = a.cue(CueKind::FirstBeat).expect("first beat");
    assert!((first.pos - 0.1).abs() < 0.01, "{}", first.pos);
    for kind in [CueKind::MixIn, CueKind::MixOut] {
        let c = a.cue(kind).expect("mix cue");
        let b = g.beat_at(c.pos);
        assert!(
            (b / 32.0 - (b / 32.0).round()).abs() < 1e-6,
            "{kind:?} at beat {b}"
        );
    }
    // Slots are unique and every cue is beat-snapped.
    let mut slots: Vec<u8> = a.cues.iter().filter_map(|c| c.slot).collect();
    slots.sort();
    slots.dedup();
    assert_eq!(
        slots.len(),
        a.cues.iter().filter(|c| c.slot.is_some()).count()
    );
    for c in &a.cues {
        let b = g.beat_at(c.pos);
        assert!((b - b.round()).abs() < 1e-6 || c.pos == 0.0);
    }
    // Sections tile the track.
    assert!(!a.sections.is_empty());
    for w in a.sections.windows(2) {
        assert!((w[0].end - w[1].start).abs() < 1e-9);
    }
}

#[test]
fn stems_path_and_waveform() {
    let x = groove(124.0, 0.2, 32, true);
    let n = x.len();
    let audio = AudioBuffer::new(SR, x.clone(), x.clone());
    // Fake stems: everything in "drums" except the pad in "other".
    let pad: Vec<f32> = {
        let y = groove(124.0, 0.2, 32, true);
        let z = groove(124.0, 0.2, 32, false);
        y.iter().zip(&z).map(|(a, b)| a - b).collect()
    };
    let drums: Vec<f32> = x.iter().zip(&pad).map(|(a, p)| a - p).collect();
    let zero = vec![0.0f32; n];
    let planar = [
        (drums.clone(), drums),
        (zero.clone(), zero.clone()),
        (pad.clone(), pad),
        (zero.clone(), zero),
    ];
    let track = StemTrack::from_planar("test", &planar);
    let owned = OwnedStems::from_stem_track(&track).expect("complete");
    // stems are stored as 16 bit with a per-chunk scale: equal within the quantisation step
    let peak = planar[2].0.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let err = owned.stems[2].0.iter().zip(&planar[2].0).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(err <= peak / 32767.0 * 1.01 + 1e-7, "stem round trip error {err}");
    let st = owned.as_ref();
    let a = analyze(&audio, Some(&st), &AnalyzeOptions::default());
    assert!((a.grid.unwrap().bpm - 124.0).abs() < 0.01);
    let w = waveform(&audio, Some(st));
    assert_eq!(w.len(), n.div_ceil(256));
    assert!(w.stems.as_ref().unwrap()[0].iter().any(|v| *v > 100));
    assert!(w.stems.as_ref().unwrap()[1].iter().all(|v| *v == 0));
}

#[test]
fn silence_and_noise_do_not_panic() {
    let n = SR as usize * 20;
    let silent = AudioBuffer::new(SR, vec![0.0; n], vec![0.0; n]);
    let a = analyze(&silent, None, &AnalyzeOptions::default());
    assert!(a.grid.is_none() && a.key.is_none() && a.loudness_lufs.is_none());
    assert!(a.cues.is_empty() && a.sections.is_empty());
    let mut rng = Rng(42);
    let noise: Vec<f32> = (0..n).map(|_| 0.1 * rng.next()).collect();
    let audio = AudioBuffer::new(SR, noise.clone(), noise);
    let _ = analyze(&audio, None, &AnalyzeOptions::default());
    let short = AudioBuffer::new(SR, vec![0.1; 1000], vec![0.1; 1000]);
    let a = analyze(&short, None, &AnalyzeOptions::default());
    assert!(a.grid.is_none());
    let w = waveform(&short, None);
    assert_eq!(w.len(), 4);
}
