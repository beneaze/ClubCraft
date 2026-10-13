//! Deck effects. Beat-synced effects take their tempo and phase from the deck's grid through
//! [`FxCtx`]. Every slot preallocates all effect types, so switching effects on the audio thread
//! never allocates. Tail effects (echo, reverb) ring out after being switched off.

use crate::dsp::{Smoother, Svf};
use serde::{Deserialize, Serialize};

/// Effect types available in a slot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FxKind {
    #[default]
    None,
    /// Beat-synced echo. params: feedback, tone (high damping).
    Echo,
    /// Echo bouncing left/right. params: feedback, tone.
    PingPong,
    /// Freeverb-style reverb. params: size, damping.
    Reverb,
    /// Beat-synced flanger. params: feedback, depth.
    Flanger,
    /// Resonant low-pass swept by a beat-synced LFO. params: resonance, depth.
    FilterLfo,
    /// Trance gate. params: duty, depth.
    Gate,
    /// Beat roll / stutter: captures `beats` on the grid and repeats it.
    Roll,
    /// Bit/sample-rate reduction. params: bits, downsample.
    Bitcrusher,
    /// Vinyl brake over `beats` (acts on the deck).
    Brake,
}

impl FxKind {
    pub const ALL: [FxKind; 10] = [
        FxKind::None,
        FxKind::Echo,
        FxKind::PingPong,
        FxKind::Reverb,
        FxKind::Flanger,
        FxKind::FilterLfo,
        FxKind::Gate,
        FxKind::Roll,
        FxKind::Bitcrusher,
        FxKind::Brake,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FxKind::None => "—",
            FxKind::Echo => "Echo",
            FxKind::PingPong => "Ping Pong",
            FxKind::Reverb => "Reverb",
            FxKind::Flanger => "Flanger",
            FxKind::FilterLfo => "Filter LFO",
            FxKind::Gate => "Gate",
            FxKind::Roll => "Roll",
            FxKind::Bitcrusher => "Bitcrusher",
            FxKind::Brake => "Brake",
        }
    }

    /// Labels of the two continuous parameters.
    pub fn param_labels(self) -> [&'static str; 2] {
        match self {
            FxKind::Echo | FxKind::PingPong => ["Feedback", "Tone"],
            FxKind::Reverb => ["Size", "Damp"],
            FxKind::Flanger => ["Feedback", "Depth"],
            FxKind::FilterLfo => ["Resonance", "Depth"],
            FxKind::Gate => ["Duty", "Depth"],
            FxKind::Bitcrusher => ["Bits", "Downsample"],
            _ => ["", ""],
        }
    }

    /// Default beat length.
    pub fn default_beats(self) -> f32 {
        match self {
            FxKind::Echo | FxKind::PingPong => 0.75,
            FxKind::Flanger | FxKind::FilterLfo => 8.0,
            FxKind::Gate => 0.25,
            FxKind::Roll => 0.25,
            FxKind::Brake => 2.0,
            _ => 1.0,
        }
    }

    pub fn default_params(self) -> [f32; 2] {
        match self {
            FxKind::Echo | FxKind::PingPong => [0.5, 0.3],
            FxKind::Reverb => [0.7, 0.4],
            FxKind::Flanger => [0.5, 0.7],
            FxKind::FilterLfo => [0.5, 0.8],
            FxKind::Gate => [0.5, 1.0],
            FxKind::Bitcrusher => [0.5, 0.3],
            _ => [0.5, 0.5],
        }
    }

    pub fn has_tail(self) -> bool {
        matches!(self, FxKind::Echo | FxKind::PingPong | FxKind::Reverb)
    }
}

/// Tempo context for one block.
#[derive(Clone, Copy, Debug)]
pub struct FxCtx {
    pub sr: f32,
    /// Effective BPM (120 when unknown).
    pub bpm: f64,
    /// Beat position (deck grid) at the block start, if the deck has a grid and is playing.
    pub beat: Option<f64>,
    /// Beats advanced per output frame.
    pub beats_per_frame: f64,
}

impl FxCtx {
    pub fn free(sr: f32) -> Self {
        FxCtx { sr, bpm: 120.0, beat: None, beats_per_frame: 120.0 / 60.0 / sr as f64 }
    }

    pub fn beat_frames(&self, beats: f64) -> f64 {
        beats * 60.0 / self.bpm.max(20.0) * self.sr as f64
    }
}

struct DelayLine {
    buf: Vec<[f32; 2]>,
    w: usize,
}

impl DelayLine {
    fn new(cap: usize) -> Self {
        DelayLine { buf: vec![[0.0; 2]; cap.max(4)], w: 0 }
    }

    #[inline]
    fn read(&self, d: f32) -> [f32; 2] {
        let cap = self.buf.len();
        let d = d.clamp(1.0, (cap - 2) as f32);
        let rp = self.w as f32 + cap as f32 - d;
        let i0 = rp as usize % cap;
        let i1 = (i0 + 1) % cap;
        let fr = rp.fract();
        let a = self.buf[i0];
        let b = self.buf[i1];
        [a[0] + (b[0] - a[0]) * fr, a[1] + (b[1] - a[1]) * fr]
    }

    #[inline]
    fn write(&mut self, x: [f32; 2]) {
        self.buf[self.w] = x;
        self.w = (self.w + 1) % self.buf.len();
    }

    fn clear(&mut self) {
        self.buf.iter_mut().for_each(|v| *v = [0.0; 2]);
    }
}

/// Beat-synced echo with damped feedback; also used as the mixer's automation echo send.
pub struct Echo {
    line: DelayLine,
    time: Smoother,
    lp: [f32; 2],
    hp: [f32; 2],
    energy: f32,
}

impl Echo {
    pub fn new(sr: f32, max_secs: f32) -> Self {
        Echo { line: DelayLine::new((sr * max_secs) as usize + 4), time: Smoother::new(sr * 0.375, 0.05, sr), lp: [0.0; 2], hp: [0.0; 2], energy: 0.0 }
    }

    pub fn is_silent(&self) -> bool {
        self.energy < 1e-5
    }

    pub fn clear(&mut self) {
        self.line.clear();
        self.energy = 0.0;
    }

    /// Adds the echo to `buf`. `input` = per-block send ramp (start, end).
    pub fn process(&mut self, buf: &mut [f32], ctx: &FxCtx, beats: f32, feedback: f32, tone: f32, input: (f32, f32), wet: f32, pingpong: bool) {
        let n = buf.len() / 2;
        if input.0 == 0.0 && input.1 == 0.0 && self.is_silent() {
            return;
        }
        self.time.set(ctx.beat_frames(beats as f64) as f32);
        let fb = feedback.clamp(0.0, 0.97);
        let damp = 0.05 + tone.clamp(0.0, 1.0) * 0.6;
        let hpc = 0.02f32;
        let inv = if n > 1 { 1.0 / (n - 1) as f32 } else { 0.0 };
        let mut e = 0f32;
        for (i, f) in buf.chunks_exact_mut(2).enumerate() {
            let g = input.0 + (input.1 - input.0) * i as f32 * inv;
            let d = self.time.next();
            let y = self.line.read(d);
            // damping (one-pole LP) + DC-ish low cut in the feedback path
            let mut fbs = [0f32; 2];
            for c in 0..2 {
                self.lp[c] += (y[c] - self.lp[c]) * (1.0 - damp);
                self.hp[c] += (self.lp[c] - self.hp[c]) * hpc;
                fbs[c] = (self.lp[c] - self.hp[c]) * fb;
            }
            let x = [f[0] * g, f[1] * g];
            let w = if pingpong {
                let m = 0.5 * (x[0] + x[1]);
                [m + fbs[1], fbs[0]]
            } else {
                [x[0] + fbs[0], x[1] + fbs[1]]
            };
            self.line.write([w[0].clamp(-4.0, 4.0), w[1].clamp(-4.0, 4.0)]);
            f[0] += y[0] * wet;
            f[1] += y[1] * wet;
            e += y[0].abs() + y[1].abs() + x[0].abs();
        }
        self.energy = self.energy * 0.5 + e;
    }
}

struct Comb {
    buf: Vec<f32>,
    i: usize,
    store: f32,
}

impl Comb {
    #[inline]
    fn process(&mut self, x: f32, fb: f32, damp: f32) -> f32 {
        let y = self.buf[self.i];
        self.store = y * (1.0 - damp) + self.store * damp;
        self.buf[self.i] = x + self.store * fb;
        self.i = (self.i + 1) % self.buf.len();
        y
    }
}

struct Allpass {
    buf: Vec<f32>,
    i: usize,
}

impl Allpass {
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let b = self.buf[self.i];
        self.buf[self.i] = x + b * 0.5;
        self.i = (self.i + 1) % self.buf.len();
        b - x
    }
}

/// Freeverb (Jezar's Schroeder/Moorer design): 8 damped combs + 4 allpasses per channel,
/// stereo spread 23 samples, scaled to the sample rate.
pub struct Reverb {
    combs: [Vec<Comb>; 2],
    aps: [Vec<Allpass>; 2],
    energy: f32,
}

impl Reverb {
    pub fn new(sr: f32) -> Self {
        let s = sr / 44100.0;
        let comb_len = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
        let ap_len = [556, 441, 341, 225];
        let mk = |spread: usize| -> (Vec<Comb>, Vec<Allpass>) {
            (
                comb_len.iter().map(|l| Comb { buf: vec![0.0; ((l + spread) as f32 * s) as usize], i: 0, store: 0.0 }).collect(),
                ap_len.iter().map(|l| Allpass { buf: vec![0.0; ((l + spread) as f32 * s) as usize], i: 0 }).collect(),
            )
        };
        let (cl, al) = mk(0);
        let (cr, ar) = mk(23);
        Reverb { combs: [cl, cr], aps: [al, ar], energy: 0.0 }
    }

    pub fn is_silent(&self) -> bool {
        self.energy < 1e-5
    }

    pub fn process(&mut self, buf: &mut [f32], size: f32, damp: f32, input: (f32, f32), wet: f32) {
        if input.0 == 0.0 && input.1 == 0.0 && self.is_silent() {
            return;
        }
        let fb = 0.7 + size.clamp(0.0, 1.0) * 0.28;
        let damp = damp.clamp(0.0, 1.0) * 0.4;
        let n = buf.len() / 2;
        let inv = if n > 1 { 1.0 / (n - 1) as f32 } else { 0.0 };
        let mut e = 0f32;
        for (i, f) in buf.chunks_exact_mut(2).enumerate() {
            let g = input.0 + (input.1 - input.0) * i as f32 * inv;
            let x = (f[0] + f[1]) * 0.015 * g;
            let mut y = [0f32; 2];
            for c in 0..2 {
                let mut acc = 0f32;
                for comb in self.combs[c].iter_mut() {
                    acc += comb.process(x, fb, damp);
                }
                for ap in self.aps[c].iter_mut() {
                    acc = ap.process(acc);
                }
                y[c] = acc;
            }
            // width 1: wet1 = wet*(1+w)/2, wet2 = wet*(1-w)/2 → plain stereo
            f[0] += y[0] * wet * 3.0;
            f[1] += y[1] * wet * 3.0;
            e += y[0].abs() + y[1].abs() + x.abs();
        }
        self.energy = self.energy * 0.5 + e;
    }
}

/// Beat roll / stutter buffer.
struct Roll {
    buf: Vec<[f32; 2]>,
    len: usize,
    rec: usize,
    play: usize,
    state: u8, // 0 idle, 1 armed, 2 recording, 3 looping
}

/// One effect slot of a deck.
pub struct FxSlot {
    pub kind: FxKind,
    pub active: bool,
    /// Dry/wet 0..1.
    pub wet: f32,
    /// Beat length (echo time, LFO period, gate step, roll length, brake length).
    pub beats: f32,
    pub params: [f32; 2],
    amount: Smoother,
    echo: Echo,
    reverb: Reverb,
    flanger: DelayLine,
    flanger_phase: f64,
    lfo_svf: Svf,
    lfo_phase: f64,
    gate_env: f32,
    roll: Roll,
    crush_hold: [f32; 2],
    crush_cnt: f32,
    dry: Vec<f32>,
    counter: usize,
}

impl FxSlot {
    pub fn new(sr: f32) -> Self {
        FxSlot {
            kind: FxKind::None,
            active: false,
            wet: 0.5,
            beats: 1.0,
            params: [0.5, 0.5],
            amount: Smoother::new(0.0, 0.01, sr),
            echo: Echo::new(sr, 4.0),
            reverb: Reverb::new(sr),
            flanger: DelayLine::new((sr * 0.02) as usize),
            flanger_phase: 0.0,
            lfo_svf: Svf::default(),
            lfo_phase: 0.0,
            gate_env: 1.0,
            roll: Roll { buf: vec![[0.0; 2]; (sr * 4.0) as usize], len: 1, rec: 0, play: 0, state: 0 },
            crush_hold: [0.0; 2],
            crush_cnt: 0.0,
            dry: vec![0.0; crate::dsp::MAX_BLOCK * 2],
            counter: 0,
        }
    }

    /// Select an effect (keeps tails of the previous echo/reverb ringing only if same kind).
    pub fn set_kind(&mut self, kind: FxKind) {
        if kind != self.kind {
            self.kind = kind;
            self.beats = kind.default_beats();
            self.params = kind.default_params();
            self.roll.state = 0;
            self.echo.clear();
            self.flanger.clear();
            self.lfo_svf.reset();
        }
    }

    pub fn set_active(&mut self, on: bool) {
        if on && !self.active {
            self.roll.state = 1;
        }
        self.active = on;
    }

    /// Process in place. Brake is handled by the engine (it acts on the deck).
    pub fn process(&mut self, buf: &mut [f32], ctx: &FxCtx) {
        let n = buf.len() / 2;
        self.amount.set(if self.active && self.kind != FxKind::None { 1.0 } else { 0.0 });
        let (a0, a1) = self.amount.advance(n);
        let wet = self.wet.clamp(0.0, 1.0);
        match self.kind {
            FxKind::None | FxKind::Brake => {}
            FxKind::Echo | FxKind::PingPong => {
                let pp = self.kind == FxKind::PingPong;
                self.echo.process(buf, ctx, self.beats, self.params[0] * 0.95, self.params[1], (a0, a1), wet, pp)
            }
            FxKind::Reverb => self.reverb.process(buf, self.params[0], self.params[1], (a0, a1), wet),
            _ => {
                if a0 == 0.0 && a1 == 0.0 {
                    return;
                }
                let dry = &mut self.dry[..n * 2];
                dry.copy_from_slice(buf);
                match self.kind {
                    FxKind::Flanger => self.flanger(buf, ctx),
                    FxKind::FilterLfo => self.filter_lfo(buf, ctx),
                    FxKind::Gate => self.gate(buf, ctx),
                    FxKind::Roll => self.roll(buf, ctx),
                    FxKind::Bitcrusher => self.crush(buf),
                    _ => {}
                }
                let inv = if n > 1 { 1.0 / (n - 1) as f32 } else { 0.0 };
                for i in 0..n {
                    let a = (a0 + (a1 - a0) * i as f32 * inv) * wet;
                    for c in 0..2 {
                        let d = self.dry[i * 2 + c];
                        buf[i * 2 + c] = d + (buf[i * 2 + c] - d) * a;
                    }
                }
            }
        }
    }

    fn lfo_phase(&mut self, ctx: &FxCtx, i: usize, period_beats: f64, free_phase: &mut f64) -> f64 {
        match ctx.beat {
            Some(b) => ((b + ctx.beats_per_frame * i as f64) / period_beats.max(1e-3)).rem_euclid(1.0),
            None => {
                *free_phase = (*free_phase + ctx.beats_per_frame / period_beats.max(1e-3)).rem_euclid(1.0);
                *free_phase
            }
        }
    }

    fn flanger(&mut self, buf: &mut [f32], ctx: &FxCtx) {
        let fb = self.params[0].clamp(0.0, 0.9);
        let depth = self.params[1].clamp(0.0, 1.0);
        let sr = ctx.sr;
        let period = self.beats as f64;
        let mut fp = self.flanger_phase;
        for (i, f) in buf.chunks_exact_mut(2).enumerate() {
            let ph = self.lfo_phase(ctx, i, period, &mut fp);
            let lfo = 0.5 - 0.5 * (std::f64::consts::TAU * ph).cos();
            let d = (0.0007 + 0.004 * depth as f64 * lfo) as f32 * sr;
            let y = self.flanger.read(d);
            self.flanger.write([f[0] + y[0] * fb, f[1] + y[1] * fb]);
            f[0] = 0.5 * (f[0] + y[0]) * 1.4;
            f[1] = 0.5 * (f[1] + y[1]) * 1.4;
        }
        self.flanger_phase = fp;
    }

    fn filter_lfo(&mut self, buf: &mut [f32], ctx: &FxCtx) {
        let q = 0.7 + self.params[0].clamp(0.0, 1.0) * 6.0;
        let depth = self.params[1].clamp(0.0, 1.0) as f64;
        let period = self.beats as f64;
        let mut fp = self.lfo_phase;
        for (i, f) in buf.chunks_exact_mut(2).enumerate() {
            let ph = self.lfo_phase(ctx, i, period, &mut fp);
            if self.counter == 0 {
                let lfo = 0.5 - 0.5 * (std::f64::consts::TAU * ph).cos();
                let oct = 6.5 * (1.0 - depth * (1.0 - lfo));
                let fc = 150.0 * 2f64.powf(oct);
                self.lfo_svf.set(fc as f32, q, ctx.sr);
                self.counter = 16;
            }
            self.counter -= 1;
            f[0] = self.lfo_svf.process(0, f[0]).lp;
            f[1] = self.lfo_svf.process(1, f[1]).lp;
        }
        self.lfo_phase = fp;
    }

    fn gate(&mut self, buf: &mut [f32], ctx: &FxCtx) {
        let duty = self.params[0].clamp(0.05, 0.95) as f64;
        let floor = 1.0 - self.params[1].clamp(0.0, 1.0);
        let period = self.beats as f64;
        let coef = 1.0 - (-1.0 / (0.0015 * ctx.sr)).exp();
        let mut fp = self.lfo_phase;
        for (i, f) in buf.chunks_exact_mut(2).enumerate() {
            let ph = self.lfo_phase(ctx, i, period, &mut fp);
            let target = if ph < duty { 1.0 } else { floor };
            self.gate_env += (target - self.gate_env) * coef;
            f[0] *= self.gate_env;
            f[1] *= self.gate_env;
        }
        self.lfo_phase = fp;
    }

    fn roll(&mut self, buf: &mut [f32], ctx: &FxCtx) {
        let r = &mut self.roll;
        let len = (ctx.beat_frames(self.beats as f64) as usize).clamp(16, r.buf.len());
        for (i, f) in buf.chunks_exact_mut(2).enumerate() {
            if r.state == 1 {
                // wait for the grid boundary of the roll length
                let start = match ctx.beat {
                    Some(b) => {
                        let step = (self.beats as f64).max(1.0 / 32.0);
                        let x0 = (b + ctx.beats_per_frame * (i as f64 - 1.0)) / step;
                        let x1 = (b + ctx.beats_per_frame * i as f64) / step;
                        x1.floor() != x0.floor() || i == 0 && x1.fract() < 1e-6
                    }
                    None => true,
                };
                if start {
                    r.state = 2;
                    r.rec = 0;
                    r.len = len;
                }
            }
            match r.state {
                2 => {
                    r.buf[r.rec] = [f[0], f[1]];
                    r.rec += 1;
                    if r.rec >= r.len {
                        r.state = 3;
                        r.play = 0;
                    }
                }
                3 => {
                    let s = r.buf[r.play];
                    // tiny fade at the loop edges
                    let edge = (r.play.min(r.len - 1 - r.play) as f32 / 32.0).min(1.0);
                    f[0] = s[0] * edge;
                    f[1] = s[1] * edge;
                    r.play = (r.play + 1) % r.len;
                }
                _ => {}
            }
        }
    }

    fn crush(&mut self, buf: &mut [f32]) {
        let bits = 2.0 + (1.0 - self.params[0].clamp(0.0, 1.0)) * 14.0;
        let steps = 2f32.powf(bits - 1.0);
        let ds = 1.0 + self.params[1].clamp(0.0, 1.0) * 39.0;
        for f in buf.chunks_exact_mut(2) {
            self.crush_cnt += 1.0;
            if self.crush_cnt >= ds {
                self.crush_cnt -= ds;
                self.crush_hold = [(f[0] * steps).round() / steps, (f[1] * steps).round() / steps];
            }
            f[0] = self.crush_hold[0];
            f[1] = self.crush_hold[1];
        }
    }

    pub fn state(&self) -> crate::state::FxSlotState {
        crate::state::FxSlotState { kind: self.kind, active: self.active, wet: self.wet, beats: self.beats, params: self.params }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_delay_is_beat_synced() {
        let sr = 44100.0;
        let mut slot = FxSlot::new(sr);
        slot.set_kind(FxKind::Echo);
        slot.beats = 1.0;
        slot.wet = 1.0;
        slot.params = [0.0, 0.0];
        slot.set_active(true);
        let ctx = FxCtx { sr, bpm: 120.0, beat: Some(0.0), beats_per_frame: 2.0 / sr as f64 };
        // settle the delay-time smoother first
        let mut buf = vec![0f32; 512];
        for _ in 0..400 {
            buf.fill(0.0);
            slot.process(&mut buf, &ctx);
        }
        // impulse, then find the echo 22050 frames later
        let mut out = Vec::new();
        for b in 0..120 {
            buf.fill(0.0);
            if b == 0 {
                buf[0] = 1.0;
                buf[1] = 1.0;
            }
            slot.process(&mut buf, &ctx);
            out.extend_from_slice(&buf);
        }
        let mut best = (0, 0f32);
        for i in 100..out.len() / 2 {
            if out[i * 2].abs() > best.1 {
                best = (i, out[i * 2].abs());
            }
        }
        assert!((best.0 as i64 - 22050).abs() <= 2, "echo at {}", best.0);
    }

    #[test]
    fn reverb_rings_and_decays() {
        let sr = 44100.0;
        let mut slot = FxSlot::new(sr);
        slot.set_kind(FxKind::Reverb);
        slot.wet = 1.0;
        slot.set_active(true);
        let ctx = FxCtx::free(sr);
        let mut buf = vec![0f32; 512];
        for _ in 0..10 {
            buf.fill(0.0);
            slot.process(&mut buf, &ctx);
        }
        buf.fill(0.0);
        buf[0] = 1.0;
        slot.process(&mut buf, &ctx);
        slot.set_active(false);
        let mut e_early = 0.0;
        let mut e_late = 0.0;
        for b in 0..400 {
            buf.fill(0.0);
            slot.process(&mut buf, &ctx);
            let e: f32 = buf.iter().map(|v| v * v).sum();
            if b < 20 {
                e_early += e;
            } else if b > 350 {
                e_late += e;
            }
        }
        assert!(e_early > 0.0 && e_late < e_early * 1e-3);
    }
}
