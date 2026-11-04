//! Silent hardware probe. Run with no arguments to enumerate output IDs; pass
//! master and optional cue IDs to exercise outputs without loading or playing tracks.
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
    if args.len() > 2 {
        bail!("usage: output_probe [master-device-id [cue-device-id]]");
    }
    let separate = args.len() == 2;
    let engine = Engine::start_with_commands(AudioConfig {
        device: Some(args[0].clone()),
        headphone_device: args.get(1).cloned(),
        headphones: if separate { HeadphoneRouting::SeparateDevice } else { HeadphoneRouting::Off },
        ..AudioConfig::default()
    }, [Command::SetMasterGain(-60.0), Command::SetHeadphoneGain(-60.0)])?;
    std::thread::sleep(Duration::from_secs(3));
    let info = engine.stream_info();
    println!("{info:#?}");
    if (separate && info.headphones.is_none())
        || info.master.failed
        || info.headphones.as_ref().is_some_and(|h| h.failed)
        || info.master.observed_buffer_frames.is_none()
        || info.headphones.as_ref().is_some_and(|h| h.observed_buffer_frames.is_none())
    {
        bail!("all selected output streams must be running (see diagnostics above)");
    }
    // An invalid selection must leave the current streams and the core running.
    let mut invalid = engine.audio_config();
    if separate {
        invalid.headphone_device = Some("clubcraft-nonexistent-output-probe".into());
    } else {
        invalid.device = Some("clubcraft-nonexistent-output-probe".into());
    }
    if engine.set_audio_config(invalid).is_ok() {
        bail!("missing output device was unexpectedly accepted");
    }
    engine.set_audio_config(engine.audio_config())?;
    std::thread::sleep(Duration::from_millis(200));
    let reopened = engine.stream_info();
    if reopened.master.failed || reopened.master.observed_buffer_frames.is_none()
        || (separate && reopened.headphones.is_none())
        || reopened.headphones.as_ref().is_some_and(|h| h.failed || h.observed_buffer_frames.is_none()) {
        bail!("output did not reopen successfully");
    }
    if engine.state().master.gain_db != -60.0 { bail!("core settings were lost while reopening output"); }
    println!("All selected silent output streams opened and reopened successfully.");
    Ok(())
}
