//! DJ mixer: per-deck channel strip (trim/auto-gain → 3-band EQ → filter → FX slots → PFL tap →
//! channel fader → crossfader; post-fader automation echo send; VU) and the master section
//! (gain → true-peak limiter → VU; headphone cue/master blend).

use crate::dsp::{db_to_gain, soft_clip, DjFilter, Eq3, Meter, Smoother, MAX_BLOCK};
use crate::fx::{Echo, FxCtx, FxSlot};
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
    pub assign: CrossfaderAssign,
    pub pfl: bool,
    meter: Meter,
    send: Echo,
    /// Post-fader echo send (automation `Param::Echo`), 0..1.
    pub echo_send: f32,
    send_amt: Smoother,
    send_buf: Vec<f32>,
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
            assign: CrossfaderAssign::Thru,
            pfl: false,
            meter: Meter::new(sr),
            send: Echo::new(sr, 3.0),
            echo_send: 0.0,
            send_amt: Smoother::new(0.0, 0.01, sr),
            send_buf: vec![0.0; MAX_BLOCK * 2],
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
    }

    /// `buf`: deck audio in; on return holds the pre-fader (post-FX) signal for PFL.
    /// `post` receives the post-fader signal (crossfader gain `xf` applied) plus the echo send.
    pub fn process(&mut self, buf: &mut [f32], post: &mut [f32], ctx: &FxCtx, curve: FaderCurve, xf: f32, auto_gain_on: bool) {
        let n = buf.len() / 2;
        self.gain.set(db_to_gain(self.trim_db + if auto_gain_on { self.auto_gain_db } else { 0.0 }));
        if !(self.gain.settled() && self.gain.value == 1.0) {
            for f in buf.chunks_exact_mut(2) {
                let g = self.gain.next();
                f[0] *= g;
                f[1] *= g;
            }
        }
        self.eq.set_knobs(self.eq_knobs[0], self.eq_knobs[1], self.eq_knobs[2]);
        self.eq.process(buf);
        self.filter.set_resonance(self.filter_resonance);
        self.filter.set(self.filter_knob);
        self.filter.process(buf);
        for s in self.fx.iter_mut() {
            s.process(buf, ctx);
        }
        self.fader_gain.set(curve.gain(self.fader) * FaderCurve::Smooth.gain(self.lane_volume) * xf);
        for (i, f) in buf.chunks_exact(2).enumerate() {
            let g = self.fader_gain.next();
            post[i * 2] = f[0] * g;
            post[i * 2 + 1] = f[1] * g;
        }
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
    pub headphone_gain_db: f32,
    hp_meter: Meter,
    pub auto_gain: bool,
    post: Vec<f32>,
    /// Extra signal added to the master before its gain and limiter (sample lanes), `n` frames.
    pub extra: Vec<f32>,
    pub extra_on: bool,
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
            headphone_gain_db: 0.0,
            hp_meter: Meter::new(sr),
            auto_gain: true,
            post: vec![0.0; MAX_BLOCK * 2],
            extra: vec![0.0; MAX_BLOCK * 2],
            extra_on: false,
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

    /// Mix `active` deck buffers (each `n` frames interleaved, modified in place) into `master`
    /// and `cue` (headphones, already blended with the master per `headphone_mix`).
    pub fn process(&mut self, decks: &mut [Vec<f32>], ctxs: &[FxCtx], active: usize, n: usize, master: &mut [f32], cue: &mut [f32]) {
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
        let mut any_pfl = false;
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
                any_pfl = true;
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
        self.master_gain.set(db_to_gain(self.master_gain_db));
        for f in master.chunks_exact_mut(2) {
            let g = self.master_gain.next();
            f[0] *= g;
            f[1] *= g;
        }
        self.limiter.process(master);
        self.master_meter.process(master);
        // headphones: cue bus blended with master (master only when nothing is cued)
        let mix = if any_pfl { self.headphone_mix.clamp(0.0, 1.0) } else { 1.0 };
        let hg = db_to_gain(self.headphone_gain_db);
        for (c, m) in cue.iter_mut().zip(master.iter()) {
            *c = soft_clip((*c * (1.0 - mix) + *m * mix) * hg);
        }
        self.hp_meter.process(cue);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossfader_curves() {
        let (a, b) = CrossfaderCurve::Smooth.gains(0.5);
        assert!((a * a + b * b - 1.0).abs() < 1e-5);
        assert_eq!(CrossfaderCurve::Full.gains(0.5), (1.0, 1.0));
        assert!((CrossfaderCurve::Scratch.gains(0.995).0 - 0.5).abs() < 1e-4);
        assert_eq!(CrossfaderCurve::Cut.gains(0.0), (1.0, 0.0));
    }
}
