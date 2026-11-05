//! Offline signal tests: these never open a device or touch the user's library.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use clubcraft_audio::AudioBuffer;
use clubcraft_core::{BeatGrid, Mix, MixTrack, TempoMode, Transition, TransitionStyle};
use clubcraft_engine::command::PreviewOutput;
use clubcraft_engine::conductor::MixSchedule;
use clubcraft_engine::player::PreviewSink;
use clubcraft_engine::*;
use std::sync::Arc;

const SR: u32 = 44100;
const FRAMES: usize = 256;

fn audio() -> Arc<LoadedTrack> {
    let samples: Vec<f32> = (0..SR * 6)
        .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / SR as f32).sin() * 0.1)
        .collect();
    Arc::new(
        LoadedTrack::new(
            "preview-test",
            Arc::new(AudioBuffer::new(SR, samples.clone(), samples)),
        )
        .with_grid(BeatGrid::new(120.0, 0.0)),
    )
}

fn mix() -> Mix {
    let mut mix = Mix {
        tempo: TempoMode::Glide,
        keylock: false,
        ..Default::default()
    };
    mix.tracks.push(MixTrack {
        track_id: 1,
        mix_in: 0.0,
        mix_out: 4.0,
        key_shift: 0,
        gain_db: 0.0,
        stems: Vec::new(),
    });
    mix.normalize();
    mix
}

fn start(core: &mut EngineCore, output: PreviewOutput, track: &Arc<LoadedTrack>) -> MixPlayer {
    let mut p = MixPlayer::new(&mix(), &|_| Some(track.clone()), [0, 1]).unwrap();
    p.play(&mut PreviewSink { sink: core, output }, 0.0)
        .unwrap();
    p
}

fn block(core: &mut EngineCore) -> (Vec<f32>, Vec<f32>) {
    let mut m = vec![0.0; FRAMES * 2];
    let mut c = m.clone();
    core.process(&mut m, &mut c);
    (m, c)
}

#[test]
fn headphones_preview_never_enters_master_or_recording() {
    let mut core = EngineCore::new(SR, 4);
    let t = audio();
    let _p = start(&mut core, PreviewOutput::Headphones, &t);
    let (producer, mut recorded) = rtrb::RingBuffer::new(FRAMES * 4);
    core.apply(Command::SetRecorder(Some(producer)));
    for master_gain in [-60.0, 0.0, 12.0] {
        core.apply(Command::SetMasterGain(master_gain));
        for blend in [0.0, 1.0] {
            core.apply(Command::SetHeadphoneMix(blend));
            core.apply(Command::SetPfl(0, blend == 1.0));
            core.apply(Command::SetCrossfader(blend));
            let (m, c) = block(&mut core);
            assert!(m.iter().all(|v| *v == 0.0));
            assert!(
                c.iter().any(|v| v.abs() > 0.01),
                "preview bypasses Cue/Master"
            );
            while let Ok(sample) = recorded.pop() {
                assert_eq!(sample, 0.0);
            }
        }
    }
    assert!(core.snapshot().recording.seconds > 0.0);
    assert!(!core.snapshot().mix.active);
    assert!(core.snapshot().preview.playing);
}

#[test]
fn headphone_volume_controls_preview() {
    let t = audio();
    let mut loud = EngineCore::new(SR, 2);
    let mut quiet = EngineCore::new(SR, 2);
    loud.apply(Command::SetMasterGain(-80.0));
    quiet.apply(Command::SetMasterGain(-80.0));
    quiet.apply(Command::SetHeadphoneGain(-20.0));
    let _a = start(&mut loud, PreviewOutput::Headphones, &t);
    let _b = start(&mut quiet, PreviewOutput::Headphones, &t);
    let (_, a) = block(&mut loud);
    let (_, b) = block(&mut quiet);
    let energy = |x: &[f32]| x.iter().map(|v| v * v).sum::<f32>();
    let ratio = energy(&b) / energy(&a);
    assert!(
        (ratio - 0.01).abs() < 0.002,
        "20 dB headphone attenuation: {ratio}"
    );
}

#[test]
fn preview_does_not_change_live_or_studio_mix_or_decks() {
    let t = audio();
    for decks in [[0, 1], [2, 3]] {
        for output in [PreviewOutput::Headphones, PreviewOutput::Master] {
            let mut core = EngineCore::new(SR, 4);
            let mut reference = EngineCore::new(SR, 4);
            let mut main = MixPlayer::new(&mix(), &|_| Some(t.clone()), decks).unwrap();
            let mut other = MixPlayer::new(&mix(), &|_| Some(t.clone()), decks).unwrap();
            main.play(&mut core, 0.0).unwrap();
            other.play(&mut reference, 0.0).unwrap();
            let mut preview = start(&mut core, output, &t);
            for i in 0..30 {
                let mut sink = PreviewSink {
                    sink: &mut core,
                    output,
                };
                match i {
                    5 => preview.pause(&mut sink).unwrap(),
                    7 => preview.resume(&mut sink).unwrap(),
                    8 => preview.seek(&mut sink, 1.0).unwrap(),
                    10 => preview.stop(&mut sink).unwrap(),
                    20 => preview = start(&mut core, output, &t),
                    _ => {}
                }
                let (actual, _) = block(&mut core);
                let (expected, _) = block(&mut reference);
                if output == PreviewOutput::Headphones {
                    assert_eq!(actual, expected, "master samples remain identical");
                } else if i == 0 {
                    assert!(
                        actual
                            .iter()
                            .zip(&expected)
                            .any(|(a, b)| (a - b).abs() > 0.01),
                        "master mode sums preview over the running mix"
                    );
                }
                let a = core.snapshot();
                let b = reference.snapshot();
                let mut main_state = a.mix;
                main_state.schedule_id = b.mix.schedule_id;
                assert_eq!(main_state, b.mix);
                assert_eq!(a.decks, b.decks);
                assert_eq!(a.master_deck, b.master_deck);
            }
        }
    }
}

#[test]
fn master_preview_is_recorded_and_obeys_master_volume() {
    let t = audio();
    let mut core = EngineCore::new(SR, 2);
    let _p = start(&mut core, PreviewOutput::Master, &t);
    let (producer, mut recorded) = rtrb::RingBuffer::new(FRAMES * 4);
    core.apply(Command::SetRecorder(Some(producer)));
    let (m, _) = block(&mut core);
    assert!(m.iter().any(|v| v.abs() > 0.01));
    for sample in &m {
        assert_eq!(recorded.pop().unwrap(), *sample);
    }
    core.apply(Command::SetMasterGain(-80.0));
    for _ in 0..100 {
        block(&mut core);
        while recorded.pop().is_ok() {}
    }
    let (m, _) = block(&mut core);
    assert!(m.iter().all(|v| v.abs() < 0.0002));
}

#[test]
fn stale_preview_load_cannot_fill_a_new_session_or_main_mix() {
    let t = audio();
    let meta = |_: i64| Some(t.analysis());
    let mut core = EngineCore::new(SR, 2);
    let old = MixSchedule::lazy(&mix(), &meta, [0, 1]);
    let old_id = old.id;
    core.apply(Command::PreviewPlay {
        schedule: Box::new(old),
        start: 0.0,
        output: PreviewOutput::Headphones,
    });
    let new = MixSchedule::lazy(&mix(), &meta, [0, 1]);
    let new_id = new.id;
    core.apply(Command::PreviewPlay {
        schedule: Box::new(new),
        start: 0.0,
        output: PreviewOutput::Headphones,
    });
    core.apply(Command::PreviewTrackReady {
        schedule: old_id,
        index: 0,
        track: t.clone(),
    });
    let (m, c) = block(&mut core);
    assert!(m.iter().chain(&c).all(|v| *v == 0.0));
    assert_eq!(core.snapshot().preview.schedule_id, new_id);
    assert!(core.snapshot().preview.waiting);
    core.apply(Command::PreviewTrackReady {
        schedule: new_id,
        index: 0,
        track: t,
    });
    let (_, c) = block(&mut core);
    assert!(c.iter().any(|v| v.abs() > 0.01));
    assert!(!core.snapshot().mix.active);
}

#[test]
fn preview_finishes_and_stops_without_stale_audio() {
    let mut core = EngineCore::new(SR, 2);
    let t = audio();
    let mut p = MixPlayer::new(&mix(), &|_| Some(t.clone()), [0, 1]).unwrap();
    p.play(
        &mut PreviewSink {
            sink: &mut core,
            output: PreviewOutput::Headphones,
        },
        p.timeline().total_secs - 0.05,
    )
    .unwrap();
    for _ in 0..20 {
        block(&mut core);
    }
    assert!(core.snapshot().preview.finished);
    let (m, c) = block(&mut core);
    assert!(m.iter().chain(&c).all(|v| *v == 0.0));
    p.stop(&mut PreviewSink {
        sink: &mut core,
        output: PreviewOutput::Headphones,
    })
    .unwrap();
    block(&mut core);
    assert!(!core.snapshot().preview.active);
}

#[test]
fn both_mashup_orders_play_only_in_headphones() {
    let t = audio();
    for style in [TransitionStyle::Mashup, TransitionStyle::MashupIn] {
        let mut m = mix();
        m.tracks.push(MixTrack {
            track_id: 2,
            mix_in: 0.0,
            mix_out: 4.0,
            key_shift: 0,
            gain_db: 0.0,
            stems: Vec::new(),
        });
        m.transitions.push(Transition::new(style, 4));
        m.normalize();
        let mut core = EngineCore::new(SR, 2);
        let mut p = MixPlayer::new(&m, &|_| Some(t.clone()), [0, 1]).unwrap();
        let at = p.timeline().transitions[0].start_sec;
        p.play(
            &mut PreviewSink {
                sink: &mut core,
                output: PreviewOutput::Headphones,
            },
            at,
        )
        .unwrap();
        let mut energy = 0.0;
        for _ in 0..20 {
            let (master, cue) = block(&mut core);
            assert!(master.iter().all(|v| *v == 0.0));
            energy += cue.iter().map(|v| v * v).sum::<f32>();
        }
        assert!(energy > 0.01);
    }
}

#[test]
fn replacing_a_headphone_preview_with_a_silent_master_preview_leaks_no_tail() {
    let t = audio();
    let mut core = EngineCore::new(SR, 2);
    let _p = start(&mut core, PreviewOutput::Headphones, &t);
    for _ in 0..10 {
        block(&mut core);
    }
    core.apply(Command::PreviewStop);
    let silent = Arc::new(
        LoadedTrack::new(
            "silent",
            Arc::new(AudioBuffer::new(
                SR,
                vec![0.0; SR as usize * 6],
                vec![0.0; SR as usize * 6],
            )),
        )
        .with_grid(BeatGrid::new(120.0, 0.0)),
    );
    let _q = start(&mut core, PreviewOutput::Master, &silent);
    let (master, _) = block(&mut core);
    assert!(
        master.iter().all(|v| *v == 0.0),
        "old headphone audio must not escape through filter history"
    );
}

#[test]
fn preview_survives_output_rate_change_without_changing_destination() {
    let t = audio();
    let mut core = EngineCore::new(SR, 2);
    let p = start(&mut core, PreviewOutput::Headphones, &t);
    block(&mut core);
    let time = core.snapshot().preview.time;
    core.set_sample_rate(48000);
    let (master, cue) = block(&mut core);
    assert_eq!(core.snapshot().preview.schedule_id, p.schedule_id());
    assert!(core.snapshot().preview.time > time);
    assert!(master.iter().all(|v| *v == 0.0));
    assert!(cue.iter().any(|v| v.abs() > 0.01));
}

#[test]
fn main_transport_cannot_stop_the_preview() {
    let t = audio();
    let mut core = EngineCore::new(SR, 4);
    let mut main = MixPlayer::new(&mix(), &|_| Some(t.clone()), [2, 3]).unwrap();
    main.play(&mut core, 0.0).unwrap();
    let p = start(&mut core, PreviewOutput::Headphones, &t);
    block(&mut core);
    main.stop(&mut core).unwrap();
    let (_, cue) = block(&mut core);
    assert!(!core.snapshot().mix.active);
    assert_eq!(core.snapshot().preview.schedule_id, p.schedule_id());
    assert!(core.snapshot().preview.playing);
    assert!(cue.iter().any(|v| v.abs() > 0.01));
}
