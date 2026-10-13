//! Real-device smoke test: plays a quiet (−32 dBFS) 440 Hz tone for ~1.2 s through deck A,
//! prints the opened stream and engine state. `cargo run --release -p clubcraft-engine --example smoke`
use clubcraft_engine::*;
use std::sync::Arc;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    for d in devices()? {
        println!("device: {} ({} ch, {:?} Hz){}", d.name, d.max_channels, d.sample_rates, if d.is_default { " [default]" } else { "" });
    }
    let engine = Engine::start(AudioConfig::default())?;
    println!("opened: {:?}", engine.stream_info());
    let sr = 44100;
    let tone: Vec<f32> = (0..sr * 3).map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sr as f32).sin() * 0.025).collect();
    let audio = Arc::new(clubcraft_audio::AudioBuffer::new(sr as u32, tone.clone(), tone));
    engine.send(Command::LoadTrack(0, Arc::new(LoadedTrack::new("tone", audio))))?;
    engine.send(Command::SetMasterGain(-6.0))?;
    engine.send(Command::SetTempo(0, 1.04))?; // keylock on by default: still 440 Hz
    engine.send(Command::Play(0))?;
    let rec = std::env::temp_dir().join("clubcraft-smoke-rec.wav");
    engine.start_recording(&rec, WavFormat::Pcm16)?;
    for i in 0..6 {
        if i == 3 {
            // hot reconfiguration: bigger buffer; deck state must survive
            let mut cfg = engine.audio_config();
            cfg.buffer_frames = Some(512);
            engine.set_audio_config(cfg)?;
            println!("reconfigured: {:?}", engine.stream_info());
        }
        std::thread::sleep(Duration::from_millis(200));
        let st = engine.state();
        println!(
            "pos {:.3} s playing {} stretching {} master peak {:.1} dBFS cpu {:.2} % errors {}",
            st.decks[0].position,
            st.decks[0].playing,
            st.decks[0].stretching,
            st.master.level.peak_db(),
            st.cpu_load * 100.0,
            st.stream_errors
        );
    }
    let r = engine.stop_recording()?;
    println!("recorded: {r:?}");
    if let Some((p, _)) = r {
        std::fs::remove_file(p).ok();
    }
    engine.send(Command::Pause(0))?;
    std::thread::sleep(Duration::from_millis(50));
    Ok(())
}
