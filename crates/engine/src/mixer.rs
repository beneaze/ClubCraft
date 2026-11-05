//! DJ mixer: per-deck channel strip (trim/auto-gain → automation loop, brake and pump → 3-band EQ
//! → filter → FX slots → PFL tap → channel fader → crossfader; pre-fader automation reverb send,
//! noise sweep and post-fader echo send; VU) and the master section
//! (gain → true-peak limiter → VU; headphone cue/master blend).

use crate::dsp::{db_to_gain, soft_clip, DjFilter, Eq3, Meter, Smoother, MAX_BLOCK};
use crate::fx::{Echo, FxCtx, FxSlot, LaneFx};
use crate::state::{FX_SLOTS, Levels};
use serde::{Deserialize, Serialize};

/// Crossfader curve.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrossfaderCurve {
    /// Equal power (constant loudness through the middle).
    #[default]
    Smooth,
    /// Both sides at full level in the centre; each fades over its outer half.
    Full,
    /// Sharp cut near the ends (5 %).
    Cut,
    /// Scratch: cut-in within 1 % of the ends.
    Scratch,
}

impl CrossfaderCurve {
    pub const ALL: [CrossfaderCurve; 4] = [CrossfaderCurve::Smooth, CrossfaderCurve::Full, CrossfaderCurve::Cut, CrossfaderCurve::Scratch];

    /// Gains for side A and B at crossfader position `x` (0 = A, 1 = B).
    pub fn gains(self, x: f32) -> (f32, f32) {
        let x = x.clamp(0.0, 1.0);
        match self {
            CrossfaderCurve::Smooth => crate::dsp::equal_power(x),
            CrossfaderCurve::Full => ((2.0 * (1.0 - x)).min(1.0), (2.0 * x).min(1.0)),
            CrossfaderCurve::Cut => (((1.0 - x) / 0.05).min(1.0), (x / 0.05).min(1.0)),
            CrossfaderCurve::Scratch => (((1.0 - x) / 0.01).min(1.0), (x / 0.01).min(1.0)),
        }
    }
}

/// Crossfader assignment of a channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrossfaderAssign {
    A,
    #[default]
    Thru,
    B,
}

/// Channel (line) fader curve.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FaderCurve {
    Linear,
    /// Audio taper (x²): most of the travel in the musically useful range.
    #[default]
    Smooth,
    /// Fast: reaches near-full level early (√x).
    Fast,
}

impl FaderCurve {
    pub fn gain(self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        match self {
            FaderCurve::Linear => x,
            FaderCurve::Smooth => x * x,
            FaderCurve::Fast => x.sqrt(),
        }
    }
}

/// One mixer channel.
pub struct Channel {
    pub trim_db: f32,
    pub auto_gain_db: f32,
    gain: Smoother,
    pub eq_knobs: [f32; 3],
    eq: Eq3,
    pub filter_knob: f32,
    pub filter_resonance: f32,
    filter: DjFilter,
    pub fx: [FxSlot; FX_SLOTS],
    pub fader: f32,
    /// Automation volume (mix lanes), 0..1; gain = value² (fixed, independent of the DJ fader
    /// curve preference, so studio transitions sound the same for everyone).
    pub lane_volume: f32,
    fader_gain: Smoother,
    /// Jump to the next automation values without smoothing (set when a mix track starts).
    pub snap_next: bool,
    pub assign: CrossfaderAssign,
    pub pfl: bool,
    meter: Meter,
    send: Echo,
    /// Post-fader echo send (automation `Param::Echo`), 0..1.
    pub echo_send: f32,
    send_amt: Smoother,
    send_buf: Vec<f32>,
    /// Transition effects driven by the mix lanes (loop, brake, pump, reverb send, noise).
    pub lane_fx: LaneFx,
}

impl Channel {
    pub fn new(sr: f32) -> Self {
        Channel {
            trim_db: 0.0,
            auto_gain_db: 0.0,
            gain: Smoother::new(1.0, 0.02, sr),
            eq_knobs: [0.5; 3],
            eq: Eq3::new(sr),
            filter_knob: 0.5,
            filter_resonance: 0.3,
            filter: DjFilter::new(sr),
            fx: std::array::from_fn(|_| FxSlot::new(sr)),
            fader: 1.0,
            lane_volume: 1.0,
            fader_gain: Smoother::new(1.0, 0.005, sr),
            snap_next: false,
            assign: CrossfaderAssign::Thru,
            pfl: false,
            meter: Meter::new(sr),
            send: Echo::new(sr, 3.0),
            echo_send: 0.0,
            send_amt: Smoother::new(0.0, 0.01, sr),
            send_buf: vec![0.0; MAX_BLOCK * 2],
            lane_fx: LaneFx::new(sr),
        }
    }

    pub fn set_eq(&mut self, band: usize, v: f32) {
        if band < 3 {
            self.eq_knobs[band] = v.clamp(0.0, 1.0);
        }
    }

    pub fn levels(&self) -> Levels {
        Levels { peak: self.meter.peak, rms: self.meter.rms() }
    }

    /// Reset the mix-automation parameters to neutral.
    pub fn reset_automation(&mut self) {
        self.lane_volume = 1.0;
        self.eq_knobs = [0.5; 3];
        self.filter_knob = 0.5;
        self.echo_send = 0.0;
        self.lane_fx.reset();
    }

    pub(crate) fn clear_preview_history(&mut self) {
        self.reset_automation();
        self.eq.clear_history();
        self.filter.clear_history();
        self.send.clear();
        self.send_amt.reset(0.0);
        self.lane_fx.clear_history();
    }

    /// `buf`: deck audio in; on return holds the pre-fader (post-FX) signal for PFL.
    /// `post` receives the post-fader signal (crossfader gain `xf` applied) plus the echo send.
    pub fn process(&mut self, buf: &mut [f32], post: &mut [f32], ctx: &FxCtx, curve: FaderCurve, xf: f32, auto_gain_on: bool) {
        let n = buf.len() / 2;
        self.gain.set(db_to_gain(self.trim_db + if auto_gain_on { self.auto_gain_db } else { 0.0 }));
        if self.snap_next {
            self.gain.reset(self.gain.target);
        }
        if !(self.gain.settled() && self.gain.value == 1.0) {
            for f in buf.as_chunks_mut::<2>().0 {
                let g = self.gain.next();
                f[0] *= g;
                f[1] *= g;
            }
        }
        if self.snap_next {
            self.lane_fx.snap();
        }
        // loop and brake act on the track itself, before EQ and filter
        self.lane_fx.process_pre(buf, ctx);
        self.eq.set_knobs(self.eq_knobs[0], self.eq_knobs[1], self.eq_knobs[2]);
        if self.snap_next {
            self.eq.snap();
        }
        self.eq.process(buf);
        self.filter.set_resonance(self.filter_resonance);
        self.filter.set(self.filter_knob);
        if self.snap_next {
            self.filter.snap();
        }
        self.filter.process(buf);
        for s in self.fx.iter_mut() {
            s.process(buf, ctx);
        }
        self.fader_gain.set(curve.gain(self.fader) * FaderCurve::Smooth.gain(self.lane_volume) * xf);
        if self.snap_next {
            self.fader_gain.reset(self.fader_gain.target);
            self.snap_next = false;
        }
        for (i, f) in buf.as_chunks::<2>().0.iter().enumerate() {
            let g = self.fader_gain.next();
            post[i * 2] = f[0] * g;
            post[i * 2 + 1] = f[1] * g;
        }
        // reverb send taken before the fader (a track can dissolve into it), and the noise sweep
        self.lane_fx.process_post(buf, &mut post[..n * 2], ctx);
        self.meter.process(&post[..n * 2]);
        // automation echo send (post-fader, keeps ringing after the fader closes)
        self.send_amt.set(self.echo_send.clamp(0.0, 1.0));
        let (s0, s1) = self.send_amt.advance(n);
        if s0 > 0.0 || s1 > 0.0 || !self.send.is_silent() {
            let fb = 0.3 + 0.5 * s0.max(s1);
            let sb = &mut self.send_buf[..n * 2];
            sb.copy_from_slice(&post[..n * 2]);
            // Echo::process adds the echo on top of its input: sb = post + echo
            self.send.process(sb, ctx, 0.5, fb, 0.3, (s0, s1), 1.0, false);
            post[..n * 2].copy_from_slice(sb);
        }
    }
}

/// Mixer: channels + crossfader + master + headphones.
pub struct Mixer {
    sr: f32,
    pub channels: Vec<Channel>,
    pub crossfader: f32,
    pub crossfader_curve: CrossfaderCurve,
    xf_gain: [Smoother; 2],
    pub fader_curve: FaderCurve,
    pub master_gain_db: f32,
    master_gain: Smoother,
    pub limiter: crate::dsp::Limiter,
    master_meter: Meter,
    pub headphone_mix: f32,
    pub shared_output: bool,
    pub headphone_gain_db: f32,
    hp_meter: Meter,
    pub auto_gain: bool,
    post: Vec<f32>,
    /// Extra signal added to the master before its gain and limiter (sample lanes), `n` frames.
    pub extra: Vec<f32>,
    pub extra_on: bool,
    /// Independent preview, inserted before master gain or after Cue/Master blending.
    pub preview: Vec<f32>,
    pub preview_output: crate::command::PreviewOutput,

    /// One channel strip per sample lane, driven by its clips' curves.
    pub sample_channels: Vec<Channel>,
    /// Each sample lane's audio of the block, and its channel output.
    pub sample_bufs: Vec<Vec<f32>>,
    pub sample_post: Vec<f32>,
}

impl Mixer {
    pub fn new(sr: f32, channels: usize) -> Self {
        Mixer {
            sr,
            channels: (0..channels).map(|_| Channel::new(sr)).collect(),
            crossfader: 0.5,
            crossfader_curve: CrossfaderCurve::Smooth,
            xf_gain: [Smoother::new(1.0, 0.002, sr); 2],
            fader_curve: FaderCurve::Smooth,
            master_gain_db: 0.0,
            master_gain: Smoother::new(1.0, 0.02, sr),
            limiter: crate::dsp::Limiter::new(sr),
            master_meter: Meter::new(sr),
            headphone_mix: 0.0,
            shared_output: false,
            headphone_gain_db: 0.0,
            hp_meter: Meter::new(sr),
            auto_gain: true,
            post: vec![0.0; MAX_BLOCK * 2],
            extra: vec![0.0; MAX_BLOCK * 2],
            extra_on: false,
            preview: vec![0.0; MAX_BLOCK * 2],
            preview_output: Default::default(),
            sample_channels: (0..clubcraft_core::SAMPLE_LANES).map(|_| Channel::new(sr)).collect(),
            sample_bufs: (0..clubcraft_core::SAMPLE_LANES).map(|_| vec![0.0; MAX_BLOCK * 2]).collect(),
            sample_post: vec![0.0; MAX_BLOCK * 2],
        }
    }

    pub fn sample_rate(&self) -> f32 {
        self.sr
    }

    pub fn master_levels(&self) -> Levels {
        Levels { peak: self.master_meter.peak, rms: self.master_meter.rms() }
    }

    pub fn headphone_levels(&self) -> Levels {
        Levels { peak: self.hp_meter.peak, rms: self.hp_meter.rms() }
    }

    /// Channel strips and sample lanes, before either output bus applies gain or limiting.
    pub(crate) fn process_channels(&mut self, decks: &mut [Vec<f32>], ctxs: &[FxCtx], active: usize, n: usize, master: &mut [f32], cue: &mut [f32]) {
        let master = &mut master[..n * 2];
        let cue = &mut cue[..n * 2];
        master.fill(0.0);
        cue.fill(0.0);
        let (xa, xb) = self.crossfader_curve.gains(self.crossfader);
        self.xf_gain[0].set(xa);
        self.xf_gain[1].set(xb);
        let (a0, a1) = self.xf_gain[0].advance(n);
        let (b0, b1) = self.xf_gain[1].advance(n);
        let fc = self.fader_curve;
        let ag = self.auto_gain;
        for (i, ch) in self.channels.iter_mut().enumerate().take(active) {
            let buf = &mut decks[i][..n * 2];
            let xf = match ch.assign {
                CrossfaderAssign::Thru => 1.0,
                CrossfaderAssign::A => (a0 + a1) * 0.5,
                CrossfaderAssign::B => (b0 + b1) * 0.5,
            };
            let post = &mut self.post[..n * 2];
            ch.process(buf, post, &ctxs[i], fc, xf, ag);
            for (m, p) in master.iter_mut().zip(post.iter()) {
                *m += *p;
            }
            if ch.pfl {
                for (c, b) in cue.iter_mut().zip(buf.iter()) {
                    *c += *b;
                }
            }
        }
        if self.extra_on {
            for (m, e) in master.iter_mut().zip(&self.extra[..n * 2]) {
                *m += *e;
            }
        }
    }

    /// Mix channels and preview into the final master and headphone output buses.
    pub fn process(&mut self, decks: &mut [Vec<f32>], ctxs: &[FxCtx], active: usize, n: usize, master: &mut [f32], cue: &mut [f32]) {
        self.process_channels(decks, ctxs, active, n, master, cue);
        let master = &mut master[..n * 2];
        let cue = &mut cue[..n * 2];
        if !self.shared_output && self.preview_output == crate::command::PreviewOutput::Master {
            for (m, p) in master.iter_mut().zip(&self.preview[..n * 2]) { *m += *p; }
        }
        self.master_gain.set(db_to_gain(self.master_gain_db));
        for f in master.as_chunks_mut::<2>().0 {
            let g = self.master_gain.next();
            f[0] *= g;
            f[1] *= g;
        }
        self.limiter.process(master);
        self.master_meter.process(master);
        // Only the Cue/Master control determines master monitoring, even with no PFL selected.
        let mix = if self.shared_output { 0.0 } else { self.headphone_mix.clamp(0.0, 1.0) };
        let hg = db_to_gain(self.headphone_gain_db);
        for ((c, m), p) in cue.iter_mut().zip(master.iter()).zip(&self.preview[..n * 2]) {
            let preview = if self.shared_output || self.preview_output == crate::command::PreviewOutput::Headphones { *p } else { 0.0 };
            *c = soft_clip((*c * (1.0 - mix) + *m * mix + preview) * hg);
        }
        self.hp_meter.process(cue);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(mixer: &mut Mixer) -> (Vec<f32>, Vec<f32>) {
        let mut decks = vec![vec![0.1; MAX_BLOCK * 2]];
        let mut master = vec![0.0; MAX_BLOCK * 2];
        let mut cue = master.clone();
        mixer.process(&mut decks, &[FxCtx::free(44100.0)], 1, MAX_BLOCK, &mut master, &mut cue);
        (master, cue)
    }

    #[test]
    fn no_pfl_is_silent_at_cue_and_master_monitoring_requires_the_mix_control() {
        let mut mixer = Mixer::new(44100.0, 1);
        let (master, cue) = render(&mut mixer);
        assert!(master.iter().any(|v| v.abs() > 0.01));
        assert!(cue.iter().all(|v| *v == 0.0));
        for blend in [0.5, 1.0, 0.0] {
            mixer.headphone_mix = blend;
            let (master, cue) = render(&mut mixer);
            assert!(master.iter().any(|v| v.abs() > 0.01));
            assert!(master.iter().zip(cue).all(|(m, c)| (c - m * blend).abs() < 1e-6));
        }
    }

    #[test]
    fn pfl_selection_controls_cue_before_the_channel_fader() {
        let mut mixer = Mixer::new(44100.0, 1);
        mixer.channels[0].fader = 0.0;
        for _ in 0..20 { render(&mut mixer); }
        mixer.channels[0].pfl = true;
        let (master, cue) = render(&mut mixer);
        assert!(master.iter().all(|v| v.abs() < 1e-6));
        assert!(cue.iter().any(|v| v.abs() > 0.01));
        mixer.channels[0].pfl = false;
        let (_, cue) = render(&mut mixer);
        assert!(cue.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn shared_output_never_duplicates_master_and_keeps_pfl_independent() {
        let mut mixer = Mixer::new(44100.0, 1);
        mixer.shared_output = true;
        for blend in [0.0, 0.5, 1.0] {
            mixer.headphone_mix = blend;
            let (master, cue) = render(&mut mixer);
            assert!(master.iter().any(|v| v.abs() > 0.01));
            assert!(cue.iter().all(|v| *v == 0.0));
        }
        mixer.channels[0].fader = 0.0;
        for _ in 0..20 { render(&mut mixer); }
        mixer.channels[0].pfl = true;
        let (master, cue) = render(&mut mixer);
        assert!(master.iter().all(|v| v.abs() < 1e-6));
        assert!(cue.iter().any(|v| v.abs() > 0.01));
    }

    #[test]
    fn crossfader_curves() {
        let (a, b) = CrossfaderCurve::Smooth.gains(0.5);
        assert!((a * a + b * b - 1.0).abs() < 1e-5);
        assert_eq!(CrossfaderCurve::Full.gains(0.5), (1.0, 1.0));
        assert!((CrossfaderCurve::Scratch.gains(0.995).0 - 0.5).abs() < 1e-4);
        assert_eq!(CrossfaderCurve::Cut.gains(0.0), (1.0, 0.0));
    }
}
