//! Silent hardware probe. Run with no arguments to enumerate output IDs; pass
//! master and cue IDs to exercise both devices without loading or playing tracks.
use anyhow::{bail, Result};
use clubcraft_engine::{devices, AudioConfig, Command, Engine, HeadphoneRouting};
use std::time::Duration;

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() {
        for d in devices()? {
            println!("{} | {} | {} ch | {:?}", d.id, d.name, d.max_channels, d.sample_rates);
        }
        return Ok(());
    }
    if args.len() != 2 {
        bail!("usage: output_probe [master-device-id cue-device-id]");
    }
    let engine = Engine::start(AudioConfig {
        device: Some(args[0].clone()),
        headphone_device: Some(args[1].clone()),
        headphones: HeadphoneRouting::SeparateDevice,
        ..AudioConfig::default()
    })?;
    engine.send(Command::SetMasterGain(-60.0))?;
    engine.send(Command::SetHeadphoneGain(-60.0))?;
    std::thread::sleep(Duration::from_secs(3));
    let info = engine.stream_info();
    println!("{info:#?}");
    if info.headphones.is_none()
        || info.master.failed
        || info.headphones.as_ref().is_some_and(|h| h.failed)
        || info.master.observed_buffer_frames.is_none()
        || info.headphones.as_ref().is_none_or(|h| h.observed_buffer_frames.is_none())
    {
        bail!("both output streams must be running (see diagnostics above)");
    }
    // An invalid selection must leave the current streams and the core running.
    let mut invalid = engine.audio_config();
    invalid.headphone_device = Some("clubcraft-nonexistent-output-probe".into());
    if engine.set_audio_config(invalid).is_ok() {
        bail!("missing headphone device was unexpectedly accepted");
    }
    engine.set_audio_config(engine.audio_config())?;
    std::thread::sleep(Duration::from_millis(200));
    let reopened = engine.stream_info();
    if reopened.master.failed || reopened.master.observed_buffer_frames.is_none()
        || reopened.headphones.as_ref().is_none_or(|h| h.failed || h.observed_buffer_frames.is_none()) {
        bail!("output did not reopen successfully");
    }
    if engine.state().master.gain_db != -60.0 { bail!("core settings were lost while reopening output"); }
    println!("Both silent output streams opened and reopened successfully.");
    Ok(())
}
