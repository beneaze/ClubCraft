//! [`EngineCore`]: decks + mixer + sync + mix conductor + recorder tap. This is the object the
//! audio callback owns; it is equally usable offline (tests, CPU benchmarks, the renderer).
//! Nothing in `apply` / `process` allocates, locks or frees memory: replaced tracks, schedules
//! and recorder rings are pushed to a garbage ring that a non-real-time thread drains.

use crate::command::{Command, DeckId, EqBand};
use crate::conductor::{Conductor, MixSchedule};
use crate::deck::Deck;
use crate::dsp::MAX_BLOCK;
use crate::fx::{FxCtx, FxKind};
use crate::mixer::Mixer;
use crate::state::{EngineState, MAX_DECKS};
use crate::track::LoadedTrack;
use clubcraft_core::StemKind;
use std::sync::Arc;

/// Things the audio thread must not drop itself.
pub enum Garbage {
    Track(Arc<LoadedTrack>),
    Schedule(Box<MixSchedule>),
    Recorder(rtrb::Producer<f32>),
}

/// Phase error above which pressing sync (or starting a synced deck) jumps instead of bending.
const SYNC_SNAP_SECS: f64 = 0.03;
/// Phase-lock time constant (s).
const SYNC_TAU: f64 = 0.2;
const SYNC_MAX_CORR: f64 = 0.05;

pub struct EngineCore {
    sr: u32,
    decks: Vec<Deck>,
    deck_count: usize,
    mixer: Mixer,
    sync: [bool; MAX_DECKS],
    snap_pending: [bool; MAX_DECKS],
    phase_offset: [Option<f64>; MAX_DECKS],
    master_manual: Option<DeckId>,
    master: Option<DeckId>,
    quantize_beats: f64,
    conductor: Conductor,
    garbage: Option<rtrb::Producer<Garbage>>,
    recorder: Option<rtrb::Producer<f32>>,
    rec_frames: u64,
    rec_dropped: u64,
    deck_bufs: Vec<Vec<f32>>,
    ctxs: [FxCtx; MAX_DECKS],
    master_buf: Vec<f32>,
    cue_buf: Vec<f32>,
    frames: u64,
    state_in: Option<triple_buffer::Input<EngineState>>,
    /// Smoothed callback load (set by the real-time wrapper).
    pub cpu_load: f32,
    pub stream_errors: u32,
}

impl EngineCore {
    /// An engine rendering at `sample_rate` with `deck_count` (2 or 4) decks.
    pub fn new(sample_rate: u32, deck_count: usize) -> Self {
        let sr = sample_rate as f32;
        EngineCore {
            sr: sample_rate,
            decks: (0..MAX_DECKS).map(|_| Deck::new(sample_rate)).collect(),
            deck_count: deck_count.clamp(1, MAX_DECKS),
            mixer: Mixer::new(sr, MAX_DECKS),
            sync: [false; MAX_DECKS],
            snap_pending: [false; MAX_DECKS],
            phase_offset: [None; MAX_DECKS],
            master_manual: None,
            master: None,
            quantize_beats: 1.0,
            conductor: Conductor::default(),
            garbage: None,
            recorder: None,
            rec_frames: 0,
            rec_dropped: 0,
            deck_bufs: (0..MAX_DECKS).map(|_| vec![0.0; MAX_BLOCK * 2]).collect(),
            ctxs: [FxCtx::free(sr); MAX_DECKS],
            master_buf: vec![0.0; MAX_BLOCK * 2],
            cue_buf: vec![0.0; MAX_BLOCK * 2],
            frames: 0,
            state_in: None,
            cpu_load: 0.0,
            stream_errors: 0,
        }
    }

    /// Route dropped objects to a collector thread.
    pub fn set_garbage(&mut self, p: rtrb::Producer<Garbage>) {
        self.garbage = Some(p);
    }

    /// Publish snapshots into this triple buffer after every `process` call.
    pub fn set_state_output(&mut self, input: triple_buffer::Input<EngineState>) {
        self.state_in = Some(input);
    }

    pub fn sample_rate(&self) -> u32 {
        self.sr
    }

    /// Change the output rate. Rebuilds the mixer and FX (allocates) — not for the audio thread.
    /// Deck state (tracks, positions, cues, loops) is preserved.
    pub fn set_sample_rate(&mut self, sr: u32) {
        if sr == self.sr {
            return;
        }
        let old = std::mem::replace(&mut self.mixer, Mixer::new(sr as f32, MAX_DECKS));
        for (c, o) in self.mixer.channels.iter_mut().zip(old.channels.iter()) {
            c.trim_db = o.trim_db;
            c.auto_gain_db = o.auto_gain_db;
            c.eq_knobs = o.eq_knobs;
            c.filter_knob = o.filter_knob;
            c.filter_resonance = o.filter_resonance;
            c.fader = o.fader;
            c.assign = o.assign;
            c.pfl = o.pfl;
        }
        self.mixer.crossfader = old.crossfader;
        self.mixer.crossfader_curve = old.crossfader_curve;
        self.mixer.fader_curve = old.fader_curve;
        self.mixer.master_gain_db = old.master_gain_db;
        self.mixer.headphone_mix = old.headphone_mix;
        self.mixer.headphone_gain_db = old.headphone_gain_db;
        self.mixer.auto_gain = old.auto_gain;
        self.mixer.limiter.enabled = old.limiter.enabled;
        self.mixer.limiter.set_ceiling_db(crate::dsp::gain_to_db(old.limiter.ceiling()));
        for d in self.decks.iter_mut() {
            d.set_output_rate(sr);
        }
        self.ctxs = [FxCtx::free(sr as f32); MAX_DECKS];
        self.sr = sr;
    }

    pub fn deck_count(&self) -> usize {
        self.deck_count
    }

    pub fn deck(&self, i: DeckId) -> &Deck {
        &self.decks[i]
    }

    pub fn deck_mut(&mut self, i: DeckId) -> &mut Deck {
        &mut self.decks[i]
    }

    pub fn mixer(&self) -> &Mixer {
        &self.mixer
    }

    pub fn mixer_mut(&mut self) -> &mut Mixer {
        &mut self.mixer
    }

    /// Pre-fader (post EQ/filter/FX) signal of deck `d` for the last processed block
    /// (interleaved; `frames` long). For metering, scopes and tests.
    pub fn channel_buffer(&self, d: DeckId, frames: usize) -> &[f32] {
        &self.deck_bufs[d][..frames.min(MAX_BLOCK) * 2]
    }

    pub fn master_deck(&self) -> Option<DeckId> {
        self.master
    }

    /// Mix playback finished (conductor ran past the end).
    pub fn mix_finished(&self) -> bool {
        self.conductor.finished
    }

    pub fn mix_time(&self) -> f64 {
        self.conductor.time
    }

    /// Mix playback state (same as `snapshot().mix`, without building the whole snapshot).
    pub fn mix_state(&self) -> crate::state::MixPlaybackState {
        self.conductor.state()
    }

    fn dispose(garbage: &mut Option<rtrb::Producer<Garbage>>, g: Garbage) {
        if let Some(p) = garbage {
            // On a full ring (should never happen) the object is dropped here as a last resort.
            let _ = p.push(g);
        }
    }

    fn valid(&self, d: DeckId) -> bool {
        d < MAX_DECKS
    }

    /// Apply one command.
    pub fn apply(&mut self, cmd: Command) {
        use Command::*;
        if let Some(d) = cmd.deck() {
            if !self.valid(d) {
                // never free a track on the audio thread, even for a bogus command
                if let LoadTrack(_, t) = cmd {
                    Self::dispose(&mut self.garbage, Garbage::Track(t));
                }
                return;
            }
        }
        match cmd {
            LoadTrack(d, t) => {
                if let Some(ch) = self.mixer.channels.get_mut(d) {
                    ch.auto_gain_db = t.gain_db.unwrap_or(0.0);
                }
                if let Some(old) = self.decks[d].load(t) {
                    Self::dispose(&mut self.garbage, Garbage::Track(old));
                }
            }
            Unload(d) => {
                if let Some(old) = self.decks[d].unload() {
                    Self::dispose(&mut self.garbage, Garbage::Track(old));
                }
            }
            Play(d) => {
                let was = self.decks[d].is_playing();
                self.decks[d].play();
                if !was && self.sync[d] {
                    self.snap_phase(d);
                }
            }
            Pause(d) => self.decks[d].pause(),
            PlayPause(d) => {
                let was = self.decks[d].is_playing();
                self.decks[d].play_pause();
                if !was && self.decks[d].is_playing() && self.sync[d] {
                    self.snap_phase(d);
                }
            }
            Stop(d) => self.decks[d].stop(),
            PlayStutter(d) => self.decks[d].play_stutter(),
            CueDown(d) => self.decks[d].cue_down(),
            CueUp(d) => self.decks[d].cue_up(),
            SetCue(d) => self.decks[d].set_cue(),
            GotoCue(d) => self.decks[d].goto_cue(),
            Seek(d, t) => self.decks[d].seek(t),
            SetCueMode(d, m) => self.decks[d].set_cue_mode(m),
            SetHotCueMode(d, m) => self.decks[d].set_hotcue_mode(m),
            HotCueDown(d, i) => self.decks[d].hot_cue_down(i),
            HotCueUp(d, i) => self.decks[d].hot_cue_up(i),
            SetHotCue { deck, slot, pos } => self.decks[deck].set_hot_cue(slot, pos),
            DeleteHotCue(d, i) => self.decks[d].delete_hot_cue(i),
            SetPitch(d, v) => {
                self.decks[d].set_pitch_slider(v);
                self.unsync_if_manual(d);
            }
            SetTempo(d, r) => {
                self.decks[d].set_tempo(r);
                self.unsync_if_manual(d);
            }
            SetPitchRange(d, r) => self.decks[d].set_pitch_range(r),
            PitchReset(d) => {
                self.decks[d].pitch_reset();
                self.unsync_if_manual(d);
            }
            PitchBend(d, v) => self.decks[d].set_pitch_bend(v),
            SetKeylock(d, on) => self.decks[d].set_keylock(on),
            SetKeyShift(d, s) => self.decks[d].set_key_shift(s),
            JogTouch(d, t) => self.decks[d].jog_touch(t),
            Jog(d, r) => self.decks[d].jog(r),
            SetVinylMode(d, on) => self.decks[d].set_vinyl_mode(on),
            SetReverse(d, on) => self.decks[d].set_reverse(on),
            Brake(d, beats) => {
                let bpm = self.decks[d].effective_bpm().unwrap_or(120.0);
                self.decks[d].brake(beats * 60.0 / bpm.max(1.0));
            }
            Spinback(d) => self.decks[d].spinback(),
            SetSlip(d, on) => self.decks[d].set_slip(on),
            LoopIn(d) => self.decks[d].loop_in(),
            LoopOut(d) => self.decks[d].loop_out(),
            AutoLoop(d, b) => self.decks[d].auto_loop(b),
            LoopExit(d) => self.decks[d].loop_exit(),
            Reloop(d) => self.decks[d].reloop(),
            LoopHalve(d) => self.decks[d].loop_halve(),
            LoopDouble(d) => self.decks[d].loop_double(),
            LoopMove(d, b) => self.decks[d].loop_move(b),
            LoopRoll { deck, beats, on } => self.decks[deck].loop_roll(beats, on),
            BeatJump(d, b) => self.decks[d].beat_jump(b),
            SetQuantize(d, on) => self.decks[d].set_quantize(on),
            SetQuantizeBeats(b) => {
                self.quantize_beats = b;
                for d in self.decks.iter_mut() {
                    d.set_quantize_beats(b);
                }
            }
            Sync(d) => {
                self.sync[d] = true;
                self.snap_pending[d] = true;
                if self.master == Some(d) && self.master_manual.is_none() {
                    self.master = None;
                }
                self.update_master();
                self.sync_step();
                if self.decks[d].is_playing() {
                    self.snap_phase(d);
                }
            }
            SetSync(d, on) => {
                if on {
                    self.apply(Sync(d));
                } else {
                    self.sync[d] = false;
                    let t = self.decks[d].tempo();
                    self.decks[d].set_tempo_override(None);
                    self.decks[d].set_tempo(t);
                }
            }
            SetMaster(m) => {
                self.master_manual = m.filter(|d| *d < MAX_DECKS);
                if let Some(m) = self.master_manual {
                    self.sync[m] = false;
                    self.decks[m].set_tempo_override(None);
                }
                self.update_master();
            }
            SetStemGain { deck, stem, gain } => self.decks[deck].set_stem_gain(stem, gain),
            SetStemMute { deck, stem, mute } => self.decks[deck].set_stem_mute(stem, mute),
            ToggleStemMute(d, stem) => {
                let m = self.decks[d].stem_gains()[stem.index()] == 0.0;
                self.decks[d].set_stem_mute(stem, !m);
            }
            StemSolo(d, s) => self.decks[d].stem_solo(s),
            Acapella(d) => self.decks[d].acapella(),
            Instrumental(d) => self.decks[d].instrumental(),
            StemsReset(d) => self.decks[d].stems_reset(),
            StemEcho { deck, stem, send, beats, feedback } => self.decks[deck].set_stem_echo(stem, send, beats, feedback),
            StemEchoOut(d, s) => self.decks[d].stem_echo_out(s),
            SetTrim(d, db) => self.mixer.channels[d].trim_db = db.clamp(-24.0, 12.0),
            SetAutoGain(on) => self.mixer.auto_gain = on,
            SetEq(d, band, v) => self.mixer.channels[d].set_eq(band as usize, v),
            SetFilter(d, v) => self.mixer.channels[d].filter_knob = v.clamp(0.0, 1.0),
            SetFilterResonance(d, v) => self.mixer.channels[d].filter_resonance = v.clamp(0.0, 1.0),
            SetFader(d, v) => self.mixer.channels[d].fader = v.clamp(0.0, 1.0),
            SetFaderCurve(c) => self.mixer.fader_curve = c,
            SetCrossfader(x) => self.mixer.crossfader = x.clamp(0.0, 1.0),
            SetCrossfaderCurve(c) => self.mixer.crossfader_curve = c,
            SetCrossfaderAssign(d, a) => self.mixer.channels[d].assign = a,
            SetPfl(d, on) => self.mixer.channels[d].pfl = on,
            SetEchoSend(d, v) => self.mixer.channels[d].echo_send = v.clamp(0.0, 1.0),
            SetMasterGain(db) => self.mixer.master_gain_db = db.clamp(-60.0, 12.0),
            SetLimiter { enabled, ceiling_db } => {
                self.mixer.limiter.enabled = enabled;
                self.mixer.limiter.set_ceiling_db(ceiling_db);
            }
            SetHeadphoneMix(v) => self.mixer.headphone_mix = v.clamp(0.0, 1.0),
            SetHeadphoneGain(db) => self.mixer.headphone_gain_db = db.clamp(-60.0, 12.0),
            SetFx { deck, slot, kind } => {
                if let Some(s) = self.mixer.channels[deck].fx.get_mut(slot) {
                    s.set_kind(kind);
                }
            }
            SetFxActive { deck, slot, on } => {
                let mut brake = None;
                if let Some(s) = self.mixer.channels[deck].fx.get_mut(slot) {
                    if s.kind == FxKind::Brake {
                        if on {
                            brake = Some(s.beats as f64);
                        }
                    } else {
                        s.set_active(on);
                    }
                }
                if let Some(b) = brake {
                    self.apply(Brake(deck, b));
                }
            }
            SetFxWet { deck, slot, wet } => {
                if let Some(s) = self.mixer.channels[deck].fx.get_mut(slot) {
                    s.wet = wet.clamp(0.0, 1.0);
                }
            }
            SetFxBeats { deck, slot, beats } => {
                if let Some(s) = self.mixer.channels[deck].fx.get_mut(slot) {
                    s.beats = beats.clamp(1.0 / 32.0, 32.0);
                }
            }
            SetFxParam { deck, slot, index, value } => {
                if let Some(s) = self.mixer.channels[deck].fx.get_mut(slot) {
                    if index < 2 {
                        s.params[index] = value.clamp(0.0, 1.0);
                    }
                }
            }
            PlayMix { schedule, start } => {
                let mut ok = schedule.decks.iter().all(|d| *d < MAX_DECKS) && schedule.decks[0] != schedule.decks[1];
                ok &= !schedule.is_empty();
                if ok {
                    for &d in &schedule.decks {
                        self.sync[d] = false;
                        if d >= self.deck_count {
                            self.deck_count = MAX_DECKS;
                        }
                    }
                    let garbage = &mut self.garbage;
                    let old = self.conductor.start(schedule, start, &mut self.decks, &mut self.mixer, &mut |t| Self::dispose(garbage, Garbage::Track(t)));
                    if let Some(old) = old {
                        Self::dispose(&mut self.garbage, Garbage::Schedule(old));
                    }
                } else {
                    Self::dispose(&mut self.garbage, Garbage::Schedule(schedule));
                }
            }
            MixSeek(t) => {
                if self.conductor.sched.is_some() {
                    self.conductor.time = t.max(0.0);
                    self.conductor.finished = false;
                }
            }
            MixPause => self.conductor.pause(&mut self.decks),
            SetMixLanes(l) => self.conductor.set_lanes(l),
            MixTrackReady { schedule, index, track } => {
                if let Some(t) = self.conductor.set_track(schedule, index, track) {
                    Self::dispose(&mut self.garbage, Garbage::Track(t));
                }
            }
            MixResume => {
                if self.conductor.sched.is_some() {
                    self.conductor.playing = true;
                }
            }
            MixStop => {
                let garbage = &mut self.garbage;
                let old = self.conductor.stop(&mut self.decks, &mut self.mixer, &mut |t| Self::dispose(garbage, Garbage::Track(t)));
                if let Some(old) = old {
                    Self::dispose(&mut self.garbage, Garbage::Schedule(old));
                }
            }
            SetDeckCount(n) => self.deck_count = if n > 2 { 4 } else { 2 },
            SetRecorder(p) => {
                if let Some(old) = std::mem::replace(&mut self.recorder, p) {
                    Self::dispose(&mut self.garbage, Garbage::Recorder(old));
                }
                self.rec_frames = 0;
                self.rec_dropped = 0;
            }
        }
    }

    /// Pressing pitch on a synced deck: VirtualDJ keeps sync but the slider moves; we drop sync
    /// so manual tempo control wins.
    fn unsync_if_manual(&mut self, d: DeckId) {
        if self.sync[d] {
            self.sync[d] = false;
            self.decks[d].set_tempo_override(None);
        }
    }

    fn conducted(&self, d: DeckId) -> bool {
        self.conductor.sched.as_ref().is_some_and(|s| s.decks.contains(&d))
    }

    fn update_master(&mut self) {
        if let Some(m) = self.master_manual {
            self.master = if self.decks[m].is_loaded() { Some(m) } else { None };
            return;
        }
        let good = |s: &Self, d: DeckId| s.decks[d].is_loaded() && s.decks[d].grid().is_some();
        if let Some(m) = self.master {
            if m < self.deck_count && good(self, m) && self.decks[m].is_playing() && !self.sync[m] {
                return;
            }
        }
        let n = self.deck_count;
        let pick = (0..n)
            .find(|&d| good(self, d) && self.decks[d].is_playing() && !self.sync[d])
            .or_else(|| (0..n).find(|&d| good(self, d) && self.decks[d].is_playing()))
            .or_else(|| self.master.filter(|&m| m < n && good(self, m)));
        self.master = pick;
    }

    /// Phase error of deck `d` against the master, in deck beats (−0.5..0.5), plus the octave
    /// multiplier used for tempo matching.
    fn phase_error(&self, d: DeckId, m: DeckId) -> Option<(f64, f64, f64)> {
        let md = &self.decks[m];
        let dd = &self.decks[d];
        let mg = md.grid()?;
        let dg = dd.grid()?;
        let mbpm = mg.bpm * md.tempo();
        let mut k = 1.0;
        for cand in [0.5, 2.0] {
            if ((mbpm * cand) / dg.bpm).ln().abs() < ((mbpm * k) / dg.bpm).ln().abs() {
                k = cand;
            }
        }
        let tempo = mbpm * k / dg.bpm;
        let mb = md.beat_position()? * k;
        let db = dd.beat_position()?;
        let e = (mb - db + 0.5).rem_euclid(1.0) - 0.5;
        Some((e, tempo, dg.beat_len()))
    }

    fn snap_phase(&mut self, d: DeckId) {
        self.update_master();
        let Some(m) = self.master.filter(|m| *m != d) else { return };
        if !self.decks[m].is_playing() {
            return;
        }
        if let Some((e, tempo, bl)) = self.phase_error(d, m) {
            self.decks[d].set_tempo_override(Some(tempo));
            let es = e * bl;
            if es.abs() > 0.001 {
                let p = self.decks[d].position() + es;
                self.decks[d].seek(p.max(0.0));
            }
            self.snap_pending[d] = false;
        }
    }

    fn sync_step(&mut self) {
        let Some(m) = self.master else {
            self.phase_offset = [None; MAX_DECKS];
            return;
        };
        for d in 0..self.deck_count {
            self.phase_offset[d] = None;
            if d == m || !self.decks[d].is_loaded() || self.conducted(d) {
                continue;
            }
            let Some((e, tempo, bl)) = self.phase_error(d, m) else { continue };
            self.phase_offset[d] = Some(e);
            if !self.sync[d] {
                continue;
            }
            let both = self.decks[d].is_playing() && self.decks[m].is_playing();
            let es = e * bl; // track seconds
            if both && self.snap_pending[d] && es.abs() > SYNC_SNAP_SECS {
                self.decks[d].set_tempo_override(Some(tempo));
                let p = self.decks[d].position() + es;
                self.decks[d].seek(p.max(0.0));
                self.snap_pending[d] = false;
                continue;
            }
            if both {
                self.snap_pending[d] = false;
            }
            let corr = if both { (es / SYNC_TAU).clamp(-SYNC_MAX_CORR, SYNC_MAX_CORR) * tempo } else { 0.0 };
            self.decks[d].set_tempo_override(Some(tempo + corr));
        }
    }

    fn process_block(&mut self, n: usize) {
        let sr = self.sr as f64;
        {
            let garbage = &mut self.garbage;
            let mut drop_track = |t: Arc<LoadedTrack>| Self::dispose(garbage, Garbage::Track(t));
            self.mixer.extra_on = self.conductor.render_samples(n, sr, &mut self.mixer.extra);
            self.conductor.step(&mut self.decks, &mut self.mixer, n, sr, &mut drop_track);
        }
        self.update_master();
        self.sync_step();
        let active = self.deck_count;
        for d in 0..active {
            let deck = &mut self.decks[d];
            let bpm = deck.effective_bpm().unwrap_or(120.0).max(1.0);
            let beat = if deck.is_playing() { deck.beat_position() } else { None };
            self.ctxs[d] = FxCtx { sr: self.sr as f32, bpm, beat, beats_per_frame: bpm / 60.0 / sr };
            deck.process(&mut self.deck_bufs[d][..n * 2]);
        }
        self.mixer.process(&mut self.deck_bufs, &self.ctxs, active, n, &mut self.master_buf, &mut self.cue_buf);
        if let Some(rec) = &mut self.recorder {
            if rec.slots() >= n * 2 {
                for s in &self.master_buf[..n * 2] {
                    let _ = rec.push(*s);
                }
                self.rec_frames += n as u64;
            } else {
                self.rec_dropped += (n * 2) as u64;
            }
        }
        self.frames += n as u64;
    }

    /// Render `master.len() / 2` frames. `master` and `cue` are interleaved stereo of equal
    /// length (`cue` = headphone bus).
    pub fn process(&mut self, master: &mut [f32], cue: &mut [f32]) {
        let frames = master.len() / 2;
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(MAX_BLOCK);
            self.process_block(n);
            master[done * 2..(done + n) * 2].copy_from_slice(&self.master_buf[..n * 2]);
            if cue.len() >= (done + n) * 2 {
                cue[done * 2..(done + n) * 2].copy_from_slice(&self.cue_buf[..n * 2]);
            }
            done += n;
        }
        self.publish();
    }

    /// Fill a snapshot of the whole engine.
    pub fn snapshot_into(&self, st: &mut EngineState) {
        st.sample_rate = self.sr;
        st.deck_count = self.deck_count;
        for d in 0..MAX_DECKS {
            let ds = &mut st.decks[d];
            self.decks[d].snapshot(ds);
            let ch = &self.mixer.channels[d];
            ds.sync = self.sync[d];
            ds.is_master = self.master == Some(d);
            ds.phase_offset = self.phase_offset[d];
            ds.trim_db = ch.trim_db;
            ds.auto_gain_db = if self.mixer.auto_gain { ch.auto_gain_db } else { 0.0 };
            ds.eq = ch.eq_knobs;
            ds.filter = ch.filter_knob;
            ds.filter_resonance = ch.filter_resonance;
            ds.fader = ch.fader;
            ds.crossfader_assign = ch.assign;
            ds.pfl = ch.pfl;
            ds.level = ch.levels();
            ds.echo_send = ch.echo_send;
            for (i, s) in ch.fx.iter().enumerate() {
                ds.fx[i] = s.state();
            }
        }
        let m = &self.mixer;
        st.master.gain_db = m.master_gain_db;
        st.master.level = m.master_levels();
        st.master.limiter_gain = m.limiter.last_min_gain;
        st.master.limiter_enabled = m.limiter.enabled;
        st.master.crossfader = m.crossfader;
        st.master.crossfader_curve = m.crossfader_curve;
        st.master.headphone_mix = m.headphone_mix;
        st.master.headphone_gain_db = m.headphone_gain_db;
        st.master.headphone_level = m.headphone_levels();
        st.master_deck = self.master;
        st.master_auto = self.master_manual.is_none();
        st.quantize_beats = self.quantize_beats;
        st.auto_gain = m.auto_gain;
        st.mix = self.conductor.state();
        st.recording.active = self.recorder.is_some();
        st.recording.seconds = self.rec_frames as f64 / self.sr as f64;
        st.recording.dropped = self.rec_dropped;
        st.cpu_load = self.cpu_load;
        st.frames = self.frames;
        st.stream_errors = self.stream_errors;
    }

    pub fn snapshot(&self) -> EngineState {
        let mut s = EngineState::default();
        self.snapshot_into(&mut s);
        s
    }

    fn publish(&mut self) {
        if let Some(mut input) = self.state_in.take() {
            self.snapshot_into(input.input_buffer_mut());
            input.publish();
            self.state_in = Some(input);
        }
    }

    /// Convenience for offline use: `deck` stems gains helper.
    pub fn set_stem(&mut self, d: DeckId, stem: StemKind, gain: f32) {
        self.decks[d].set_stem_gain(stem, gain);
    }

    /// Convenience for offline use: set an EQ band.
    pub fn set_eq(&mut self, d: DeckId, band: EqBand, v: f32) {
        self.mixer.channels[d].set_eq(band as usize, v);
    }
}
