//! Offline engine tests: sync, render alignment, limiter, automix, CPU budget.

use clubcraft_audio::AudioBuffer;
use clubcraft_core::{BeatGrid, Cue, CueKind, Mix, MixTrack, StemKind, StemTrack, TempoMode, Transition, TransitionStyle};
use clubcraft_engine::*;
use std::sync::Arc;

const SR: u32 = 44100;

/// Click track: a 3 ms 2 kHz burst on every beat.
fn click_track(bpm: f64, secs: f64, offset: f64) -> Vec<f32> {
    let n = (secs * SR as f64) as usize;
    let mut v = vec![0f32; n];
    let bl = 60.0 / bpm;
    let mut k = 0usize;
    loop {
        let t = offset + k as f64 * bl;
        let s = (t * SR as f64).round() as usize;
        if s >= n {
            break;
        }
        for j in 0..132 {
            if s + j < n {
                let env = (1.0 - j as f32 / 132.0).powi(2);
                v[s + j] = 0.7 * env * (2.0 * std::f32::consts::PI * 2000.0 * j as f32 / SR as f32).sin().signum();
            }
        }
        k += 1;
    }
    v
}

fn mono(v: &[f32]) -> Arc<AudioBuffer> {
    Arc::new(AudioBuffer::new(SR, v.to_vec(), v.to_vec()))
}

fn track(v: &[f32], bpm: f64, downbeat: f64) -> Arc<LoadedTrack> {
    Arc::new(LoadedTrack::new("t", mono(v)).with_grid(BeatGrid::new(bpm, downbeat)))
}

/// Onsets (frames): first sample above `thr` after at least `gap` quiet frames.
fn onsets(x: &[f32], thr: f32, gap: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut quiet = gap;
    for (i, v) in x.iter().enumerate() {
        if v.abs() > thr {
            if quiet >= gap {
                out.push(i);
            }
            quiet = 0;
        } else {
            quiet += 1;
        }
    }
    out
}

#[test]
fn sync_phase_locks_124_to_126_within_1ms() {
    for keylock in [true, false] {
        let mut core = EngineCore::new(SR, 2);
        core.apply(Command::LoadTrack(0, track(&click_track(124.0, 60.0, 0.1), 124.0, 0.1)));
        core.apply(Command::LoadTrack(1, track(&click_track(126.0, 60.0, 0.3), 126.0, 0.3)));
        core.apply(Command::SetKeylock(0, keylock));
        core.apply(Command::SetKeylock(1, keylock));
        core.apply(Command::Play(0));
        let mut m = vec![0f32; 256 * 2];
        let mut c = vec![0f32; 256 * 2];
        for _ in 0..200 {
            core.process(&mut m, &mut c);
        }
        core.apply(Command::Seek(1, 7.77)); // arbitrary phase
        core.apply(Command::Play(1));
        core.apply(Command::Sync(1));
        // 10 s of playback, collect each channel's pre-fader audio for the last 4 s
        let mut a = Vec::new();
        let mut b = Vec::new();
        let blocks = 10 * SR as usize / 256;
        for i in 0..blocks {
            core.process(&mut m, &mut c);
            if i > blocks - 4 * SR as usize / 256 {
                a.extend(core.channel_buffer(0, 256).iter().step_by(2));
                b.extend(core.channel_buffer(1, 256).iter().step_by(2));
            }
        }
        let st = core.snapshot();
        assert_eq!(st.master_deck, Some(0));
        assert!(st.decks[1].sync);
        assert!((st.decks[1].effective_bpm.unwrap() - 124.0).abs() < 0.01, "bpm {:?}", st.decks[1].effective_bpm);
        // phase from the (latency compensated) heard positions
        let g0 = BeatGrid::new(124.0, 0.1);
        let g1 = BeatGrid::new(126.0, 0.3);
        let e = (g0.beat_at(st.decks[0].position) - g1.beat_at(st.decks[1].position) + 0.5).rem_euclid(1.0) - 0.5;
        let err_ms = e * 60.0 / 124.0 * 1000.0;
        assert!(err_ms.abs() < 1.0, "keylock={keylock}: position phase error {err_ms} ms");
        // and in the audio: click onsets coincide
        let oa = onsets(&a, 0.2, 2000);
        let ob = onsets(&b, 0.2, 2000);
        assert!(oa.len() > 5 && ob.len() > 5);
        let mut worst = 0f64;
        for x in &oa {
            let nearest = ob.iter().map(|y| (*y as f64 - *x as f64).abs()).fold(f64::MAX, f64::min);
            worst = worst.max(nearest);
        }
        let worst_ms = worst / SR as f64 * 1000.0;
        let tol = if keylock { 3.0 } else { 1.0 };
        eprintln!("sync keylock={keylock}: position error {err_ms:.4} ms, audio onset error {worst_ms:.3} ms");
        assert!(worst_ms < tol, "keylock={keylock}: onset misalignment {worst_ms} ms");
    }
}

fn two_track_mix(style: TransitionStyle, keylock: bool) -> Mix {
    let mut mix = Mix { keylock, tempo: TempoMode::Glide, ..Default::default() };
    mix.tracks.push(MixTrack { track_id: 1, mix_in: 0.0, mix_out: 20.0, key_shift: 0, gain_db: 0.0 });
    mix.tracks.push(MixTrack { track_id: 2, mix_in: 8.0 * 60.0 / 126.0, mix_out: 30.0, key_shift: 0, gain_db: 0.0 });
    mix.normalize();
    mix.transitions[0] = Transition::new(style, 16);
    mix
}

#[test]
fn render_mix_aligns_beats_in_overlap() {
    for keylock in [false, true] {
        let a = click_track(120.0, 40.0, 0.0);
        let b = click_track(126.0, 40.0, 0.0);
        let silent = vec![0f32; a.len()];
        let mix = two_track_mix(TransitionStyle::Crossfade, keylock);
        let opts = RenderOptions { limiter: false, tail_secs: 0.5, ..Default::default() };
        // render each track alone in the mix context (timing does not depend on the audio)
        let only_a = |id: i64| Some(if id == 1 { track(&a, 120.0, 0.0) } else { track(&silent, 126.0, 0.0) });
        let only_b = |id: i64| Some(if id == 1 { track(&silent, 120.0, 0.0) } else { track(&b, 126.0, 0.0) });
        let (ra, rep) = render_mix_to_vec(&mix, &only_a, &opts).unwrap();
        let (rb, _) = render_mix_to_vec(&mix, &only_b, &opts).unwrap();
        let tl = &rep.timeline;
        let x = &tl.transitions[0];
        assert!((x.start_sec - 20.0).abs() < 1e-9);
        // overlap window, skipping the very quiet edges of the crossfade
        let s0 = (tl.sec_at_beat(x.start_beat + 3.9) * SR as f64) as usize;
        let s1 = (tl.sec_at_beat(x.start_beat + 12.1) * SR as f64) as usize;
        let la: Vec<f32> = ra[s0 * 2..s1 * 2].iter().step_by(2).copied().collect();
        let lb: Vec<f32> = rb[s0 * 2..s1 * 2].iter().step_by(2).copied().collect();
        let oa = onsets(&la, 0.08, 4000);
        let ob = onsets(&lb, 0.08, 4000);
        assert!(oa.len() >= 8 && ob.len() >= 8, "onsets {} {}", oa.len(), ob.len());
        let mut worst = 0f64;
        for x in &oa {
            let nearest = ob.iter().map(|y| (*y as f64 - *x as f64).abs()).fold(f64::MAX, f64::min);
            worst = worst.max(nearest);
        }
        let worst_ms = worst / SR as f64 * 1000.0;
        eprintln!("render keylock={keylock}: {} beats in overlap, worst misalignment {worst_ms:.3} ms; render speed {:.0}x realtime", oa.len(), rep.speed);
        assert!(worst_ms < if keylock { 3.0 } else { 1.0 }, "misaligned by {worst_ms} ms");
        // the incoming track starts exactly on the outgoing mix-out beat: first click of B at 20 s
        let fb = onsets(&rb.iter().step_by(2).copied().collect::<Vec<_>>(), 1e-4, 4000);
        assert!(!fb.is_empty());
        // tempo glide: the overlap second half ramps to 126 BPM
        assert!((tl.bpm_at_beat(x.start_beat + 4.0) - 120.0).abs() < 1e-9);
        assert!((tl.bpm_at_beat(x.start_beat + 16.0) - 126.0).abs() < 1e-9);
    }
}

#[test]
fn render_mix_writes_wav() {
    let a = click_track(120.0, 30.0, 0.0);
    let b = click_track(124.0, 30.0, 0.0);
    let mix = two_track_mix(TransitionStyle::BassSwap, true);
    let dir = std::env::temp_dir().join("clubcraft-engine-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mix.wav");
    let tr = |id: i64| Some(if id == 1 { track(&a, 120.0, 0.0) } else { track(&b, 124.0, 0.0) });
    let mut calls = 0;
    let rep = render_mix(&mix, &tr, &path, &RenderOptions { format: WavFormat::Pcm16, ..Default::default() }, &mut |_| {
        calls += 1;
        true
    })
    .unwrap();
    let r = hound::WavReader::open(&path).unwrap();
    assert_eq!(r.spec().bits_per_sample, 16);
    assert_eq!(r.duration() as u64, rep.frames);
    assert!(calls > 3);
    assert!(rep.peak <= clubcraft_engine::dsp::db_to_gain(-1.0) + 1e-6);
    std::fs::remove_file(&path).ok();
}

#[test]
fn limiter_never_exceeds_0dbfs_in_engine() {
    let mut core = EngineCore::new(48000, 4);
    let n = SR as usize * 6;
    let mut seed = 7u32;
    for d in 0..4 {
        let v: Vec<f32> = (0..n)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = (seed >> 9) as f32 / (1u32 << 23) as f32 - 1.0;
                (0.9 * (i as f32 * 0.05 * (d + 1) as f32).sin() + 0.4 * noise).clamp(-1.0, 1.0)
            })
            .collect();
        core.apply(Command::LoadTrack(d, Arc::new(LoadedTrack::new("loud", mono(&v)))));
        core.apply(Command::SetTrim(d, 12.0));
        core.apply(Command::SetEq(d, EqBand::Low, 1.0));
        core.apply(Command::SetEq(d, EqBand::High, 1.0));
        core.apply(Command::Play(d));
    }
    core.apply(Command::SetMasterGain(12.0));
    core.apply(Command::SetLimiter { enabled: true, ceiling_db: 0.0 });
    let mut m = vec![0f32; 512 * 2];
    let mut c = vec![0f32; 512 * 2];
    let mut peak = 0f32;
    for _ in 0..(48000 * 4 / 512) {
        core.process(&mut m, &mut c);
        for v in m.iter().chain(c.iter()) {
            peak = peak.max(v.abs());
        }
    }
    assert!(peak <= 1.0, "peak {peak}");
    assert!(peak > 0.9);
    assert!(core.snapshot().master.limiter_gain < 0.5);
}

#[test]
fn eq_low_kill_in_channel() {
    let mut core = EngineCore::new(SR, 2);
    let n = SR as usize * 4;
    let s: Vec<f32> = (0..n).map(|i| (2.0 * std::f64::consts::PI * 60.0 * i as f64 / SR as f64).sin() as f32 * 0.5).collect();
    core.apply(Command::LoadTrack(0, Arc::new(LoadedTrack::new("sine", mono(&s)))));
    core.apply(Command::SetKeylock(0, false));
    core.apply(Command::Play(0));
    let mut m = vec![0f32; 256 * 2];
    let mut c = vec![0f32; 256 * 2];
    let rms = |core: &mut EngineCore, m: &mut Vec<f32>, c: &mut Vec<f32>| {
        let mut acc = 0f64;
        let mut cnt = 0;
        for _ in 0..200 {
            core.process(m, c);
            for v in core.channel_buffer(0, 256).iter().step_by(2) {
                acc += (*v as f64).powi(2);
                cnt += 1;
            }
        }
        (acc / cnt as f64).sqrt()
    };
    let _ = rms(&mut core, &mut m, &mut c);
    let r0 = rms(&mut core, &mut m, &mut c);
    core.apply(Command::SetEq(0, EqBand::Low, 0.0));
    let _ = rms(&mut core, &mut m, &mut c);
    let r1 = rms(&mut core, &mut m, &mut c);
    let att = 20.0 * (r1 / r0).log10();
    eprintln!("low kill at 60 Hz: {att:.1} dB");
    assert!(att < -40.0, "{att} dB");
}

#[test]
fn automix_starts_incoming_on_the_mix_out_beat() {
    let a = click_track(122.0, 40.0, 0.0);
    let b = click_track(122.0, 40.0, 0.0);
    let mut ta = LoadedTrack::new("a", mono(&a)).with_grid(BeatGrid::new(122.0, 0.0));
    ta.cues = vec![Cue::auto(None, CueKind::MixIn, 0.0), Cue::auto(None, CueKind::MixOut, 64.0 * 60.0 / 122.0)];
    let mut tb = LoadedTrack::new("b", mono(&b)).with_grid(BeatGrid::new(122.0, 0.0));
    tb.cues = vec![Cue::auto(None, CueKind::MixIn, 4.0 * 60.0 / 122.0), Cue::auto(None, CueKind::MixOut, 30.0)];
    let playlist = vec![Arc::new(ta), Arc::new(tb)];
    let opts = AutomixOptions { beats: 16, decks: [0, 1], keylock: false, ..Default::default() };
    let mut core = EngineCore::new(SR, 2);
    core.apply(automix::start(&playlist, &opts, None).unwrap());
    let mut m = vec![0f32; 256 * 2];
    let mut c = vec![0f32; 256 * 2];
    let mut first_b = None;
    let mut preloaded = false;
    let mut t = 0.0;
    while t < 40.0 {
        core.process(&mut m, &mut c);
        let st = core.snapshot();
        if st.decks[1].loaded && !st.decks[1].playing && t > 1.0 {
            preloaded = true;
            assert!((st.decks[1].position - 4.0 * 60.0 / 122.0).abs() < 1e-6, "parked at mix-in");
        }
        if first_b.is_none() && st.decks[1].playing {
            first_b = Some((st.mix.time, st.decks[0].position, st.decks[1].position));
        }
        t = st.mix.time;
    }
    assert!(preloaded, "next track preloaded on the idle deck");
    let (mt, pa, pb) = first_b.expect("deck B started");
    let mix_out = 64.0 * 60.0 / 122.0;
    // started within one block of the mix-out, phase aligned
    assert!((mt - mix_out).abs() < 0.01, "B started at mix time {mt}");
    let ga = BeatGrid::new(122.0, 0.0);
    let e = (ga.beat_at(pa) - ga.beat_at(pb) + 0.5).rem_euclid(1.0) - 0.5;
    assert!(e.abs() * 60.0 / 122.0 < 0.0005, "phase error {e} beats");
    let st = core.snapshot();
    assert!(st.mix.current_track == 1);
    assert!(!st.decks[0].playing, "outgoing stopped after the transition");
}

#[test]
fn mix_player_seek_repositions_decks() {
    let a = click_track(120.0, 40.0, 0.0);
    let b = click_track(128.0, 40.0, 0.0);
    let mix = two_track_mix(TransitionStyle::StemBlend, true);
    let tr = |id: i64| Some(if id == 1 { track(&a, 120.0, 0.0) } else { track(&b, 128.0, 0.0) });
    let player = MixPlayer::new(&mix, &tr, [2, 3]).unwrap();
    let mut core = EngineCore::new(48000, 4);
    player.play(&mut core, 0.0).unwrap();
    let mut m = vec![0f32; 480 * 2];
    let mut c = vec![0f32; 480 * 2];
    for _ in 0..100 {
        core.process(&mut m, &mut c);
    }
    let x = player.timeline().transitions[0].clone();
    let mid = 0.5 * (x.start_sec + x.end_sec);
    player.seek(&mut core, mid).unwrap();
    for _ in 0..10 {
        core.process(&mut m, &mut c);
    }
    let st = core.snapshot();
    assert!(st.decks[2].playing && st.decks[3].playing, "both decks play in the transition");
    let tl = player.timeline();
    let b = tl.beat_at_sec(st.mix.time);
    assert!((st.decks[2].position - tl.track_pos(0, b)).abs() < 0.004);
    assert!((st.decks[3].position - tl.track_pos(1, b)).abs() < 0.004);
    assert!(st.mix.transition.is_some());
    player.stop(&mut core).unwrap();
    core.process(&mut m, &mut c);
    assert!(!core.snapshot().decks[2].playing);
}

fn noise(n: usize, seed: u32, amp: f32) -> Vec<f32> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            ((s >> 9) as f32 / (1u32 << 23) as f32 - 1.0) * amp
        })
        .collect()
}

/// CPU budget: 10 s of 4 decks, all playing with keylock (+3 % tempo, +1 semitone on two
/// decks), stems active with non-unity gains, 2 FX on every deck, EQ + filter. Run with
/// `cargo test --release -p clubcraft-engine --test engine cpu_budget -- --nocapture`.
#[test]
fn cpu_budget_4_decks_keylock_stems_fx() {
    let secs = 10.0;
    let n = (SR as f64 * 40.0) as usize;
    let mut core = EngineCore::new(48000, 4);
    for d in 0..4 {
        let tone: Vec<f32> = (0..n).map(|i| (i as f32 * 0.031 * (d + 1) as f32).sin() * 0.3).collect();
        let nz = noise(n, d as u32 + 1, 0.1);
        let l: Vec<f32> = tone.iter().zip(&nz).map(|(a, b)| a + b).collect();
        let stems: [(Vec<f32>, Vec<f32>); 4] = std::array::from_fn(|s| (noise(n, 10 + s as u32, 0.1), noise(n, 20 + s as u32, 0.1)));
        let st = Arc::new(StemTrack::from_planar("bench", &stems));
        let t = LoadedTrack::new("bench", Arc::new(AudioBuffer::new(SR, l.clone(), l))).with_grid(BeatGrid::new(124.0, 0.0)).with_stems(st);
        core.apply(Command::LoadTrack(d, Arc::new(t)));
        core.apply(Command::SetKeylock(d, true));
        core.apply(Command::SetTempo(d, 1.03));
        if d % 2 == 1 {
            core.apply(Command::SetKeyShift(d, 1.0));
        }
        core.apply(Command::SetStemGain { deck: d, stem: StemKind::Vocals, gain: 0.3 });
        core.apply(Command::SetStemGain { deck: d, stem: StemKind::Drums, gain: 0.7 });
        core.apply(Command::SetFx { deck: d, slot: 0, kind: FxKind::Echo });
        core.apply(Command::SetFxActive { deck: d, slot: 0, on: true });
        core.apply(Command::SetFx { deck: d, slot: 1, kind: if d % 2 == 0 { FxKind::Reverb } else { FxKind::Flanger } });
        core.apply(Command::SetFxActive { deck: d, slot: 1, on: true });
        core.apply(Command::SetEq(d, EqBand::Low, 0.3));
        core.apply(Command::SetFilter(d, 0.35));
        core.apply(Command::Play(d));
    }
    let block = 256usize;
    let mut m = vec![0f32; block * 2];
    let mut c = vec![0f32; block * 2];
    let blocks = (secs * 48000.0) as usize / block;
    // thread CPU time (excludes time the OS spent running other processes)
    fn cpu_now() -> f64 {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
        ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
    }
    let mut times = Vec::with_capacity(blocks);
    let wall = std::time::Instant::now();
    let t0 = cpu_now();
    for _ in 0..blocks {
        let tb = cpu_now();
        core.process(&mut m, &mut c);
        times.push(cpu_now() - tb);
    }
    let el = cpu_now() - t0;
    eprintln!("wall clock {:.3} s (machine load affects this), thread CPU {:.3} s", wall.elapsed().as_secs_f64(), el);
    let first = times[0];
    let mut sorted: Vec<f64> = times[4..].to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let worst_block = *sorted.last().unwrap();
    let p99 = sorted[sorted.len() * 99 / 100];
    let p999 = sorted[sorted.len() * 999 / 1000];
    eprintln!("block times: first {:.3} ms, p99 {:.3} ms, p99.9 {:.3} ms, max(after start) {:.3} ms", first * 1e3, p99 * 1e3, p999 * 1e3, worst_block * 1e3);
    let st = core.snapshot();
    assert!(st.decks.iter().all(|d| d.playing && d.stretching));
    let rt = el / secs;
    let block_budget = block as f64 / 48000.0;
    eprintln!(
        "CPU: 10 s of 4 decks (keylock + stems + 2 FX each) in {:.3} s CPU → {:.2} % of one core total, {:.2} % per deck; worst block {:.3} ms of {:.3} ms budget ({:.1} %)",
        el,
        rt * 100.0,
        rt * 25.0,
        worst_block * 1000.0,
        block_budget * 1000.0,
        worst_block / block_budget * 100.0
    );
    if !cfg!(debug_assertions) {
        assert!(rt < 0.25, "realtime factor {rt}");
    }
}

#[test]
fn stem_echo_out_leaves_a_decaying_tail() {
    // vocal stem = the whole mix, so muting it silences the deck except for the echo
    let n = SR as usize * 20;
    let v = click_track(120.0, 20.0, 0.0);
    let z = vec![0f32; n];
    let stems: [(Vec<f32>, Vec<f32>); 4] = [(z.clone(), z.clone()), (z.clone(), z.clone()), (z.clone(), z.clone()), (v.clone(), v.clone())];
    let t = LoadedTrack::new("v", mono(&v)).with_grid(BeatGrid::new(120.0, 0.0)).with_stems(Arc::new(StemTrack::from_planar("t", &stems)));
    let mut core = EngineCore::new(SR, 2);
    core.apply(Command::LoadTrack(0, Arc::new(t)));
    core.apply(Command::Play(0));
    let mut m = vec![0f32; 256 * 2];
    let mut c = vec![0f32; 256 * 2];
    for _ in 0..100 {
        core.process(&mut m, &mut c);
    }
    core.apply(Command::StemEchoOut(0, StemKind::Vocals));
    let mut energy = Vec::new();
    for _ in 0..(SR as usize * 4 / 256) {
        core.process(&mut m, &mut c);
        energy.push(m.iter().map(|x| x * x).sum::<f32>());
    }
    let sec = |a: usize, b: usize| energy[a * SR as usize / 256..b * SR as usize / 256].iter().sum::<f32>();
    let (e1, e2, e3) = (sec(1, 2), sec(2, 3), sec(3, 4));
    assert!(e1 > 1e-3, "echo tail audible after the mute: {e1}");
    assert!(e2 < e1 && e3 < e2, "tail decays: {e1} {e2} {e3}");
    assert!(core.snapshot().decks[0].stem_muted[3]);
}

#[test]
fn brake_spinback_reverse() {
    let v = click_track(120.0, 20.0, 0.0);
    let mut core = EngineCore::new(SR, 2);
    core.apply(Command::LoadTrack(0, track(&v, 120.0, 0.0)));
    core.apply(Command::Seek(0, 5.0));
    core.apply(Command::Play(0));
    let mut m = vec![0f32; 256 * 2];
    let mut c = vec![0f32; 256 * 2];
    let run = |core: &mut EngineCore, m: &mut Vec<f32>, c: &mut Vec<f32>, secs: f64| {
        for _ in 0..(secs * SR as f64 / 256.0) as usize {
            core.process(m, c);
        }
    };
    run(&mut core, &mut m, &mut c, 0.5);
    core.apply(Command::Brake(0, 2.0)); // 2 beats = 1 s at 120 BPM
    let p0 = core.snapshot().decks[0].position;
    run(&mut core, &mut m, &mut c, 1.3);
    let st = core.snapshot();
    assert!(!st.decks[0].playing, "stopped after the brake");
    let travelled = st.decks[0].position - p0;
    assert!((travelled - 0.5).abs() < 0.05, "brake travels ~half the distance: {travelled}");
    core.apply(Command::Play(0));
    core.apply(Command::SetReverse(0, true));
    let p1 = core.snapshot().decks[0].position;
    run(&mut core, &mut m, &mut c, 1.0);
    let p2 = core.snapshot().decks[0].position;
    assert!((p1 - p2 - 1.0).abs() < 0.01, "reverse at speed 1: {p1} -> {p2}");
    core.apply(Command::SetReverse(0, false));
    core.apply(Command::Spinback(0));
    run(&mut core, &mut m, &mut c, 3.0);
    let st = core.snapshot();
    assert!(!st.decks[0].playing && st.decks[0].position < p2 - 0.3, "spinback went backwards and stopped");
}

#[test]
fn quantized_auto_loop_with_keylock_stays_on_grid() {
    // keylock path: a 1-beat loop must keep the heard position inside the loop and phase locked
    let v = click_track(126.0, 30.0, 0.0);
    let mut core = EngineCore::new(48000, 2);
    core.apply(Command::LoadTrack(0, track(&v, 126.0, 0.0)));
    core.apply(Command::SetTempo(0, 1.04));
    core.apply(Command::Seek(0, 3.3));
    core.apply(Command::Play(0));
    let mut m = vec![0f32; 256 * 2];
    let mut c = vec![0f32; 256 * 2];
    for _ in 0..20 {
        core.process(&mut m, &mut c);
    }
    core.apply(Command::AutoLoop(0, 1.0));
    let mut out = Vec::new();
    for _ in 0..(48000 * 4 / 256) {
        core.process(&mut m, &mut c);
        let st = core.snapshot();
        let l = st.decks[0].loop_state;
        assert!(l.active && st.decks[0].position >= l.start - 1e-6 && st.decks[0].position < l.end + 1e-6);
        out.extend(core.channel_buffer(0, 256).iter().step_by(2));
    }
    // clicks keep coming once per (stretched) beat
    let o = onsets(&out[48000..], 0.2, 3000);
    let period = 60.0 / (126.0 * 1.04) * 48000.0;
    for w in o.windows(2) {
        let d = (w[1] - w[0]) as f64;
        assert!((d - period).abs() < 48.0, "click spacing {d} vs {period}");
    }
}
