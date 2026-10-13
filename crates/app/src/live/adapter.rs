//! Bridge between the UI and `clubcraft_engine`: loading tracks (decode + stems + analysis),
//! progressive live stem separation, translating UI actions into engine commands, mapping
//! engine state back to the view-model, persisting user hot cues, studio preview / live mix
//! playback and offline export.

use super::model::*;
use crate::app::Shared;
use anyhow::{Context, Result};
use clubcraft_core::library::stems_path;
use clubcraft_core::{Cue, CueSource, Library, Mix, StemKind, StemTrack, TrackId, HOT_CUES};
use clubcraft_engine::{
    AudioConfig, Command, CommandSink, Engine, EngineHandle, EngineState, EqBand, FxKind, LoadedTrack, MixPlayer, PitchRange, RenderOptions, WavFormat,
};
use crossbeam_channel::{unbounded, Receiver, Sender};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// Decks used by the studio preview (the live decks stay untouched).
pub const STUDIO_DECKS: [usize; 2] = [2, 3];
pub const LIVE_DECKS: [usize; 2] = [0, 1];

/// Decoded tracks shared by decks, previews and renders.
#[derive(Clone, Default)]
pub struct TrackCache {
    inner: Arc<Mutex<HashMap<TrackId, Arc<LoadedTrack>>>>,
}

impl TrackCache {
    pub fn get(&self, id: TrackId) -> Option<Arc<LoadedTrack>> {
        self.inner.lock().get(&id).cloned()
    }

    pub fn insert(&self, id: TrackId, t: Arc<LoadedTrack>) {
        let mut m = self.inner.lock();
        if m.len() > 40 {
            // drop tracks nobody else holds
            m.retain(|_, v| Arc::strong_count(v) > 1);
        }
        m.insert(id, t);
    }

    pub fn invalidate(&self, id: TrackId) {
        self.inner.lock().remove(&id);
    }

    /// Load (decode, stems, analysis) a track; cached. Analyses the track first if needed.
    pub fn load(&self, id: TrackId, live_sep: Option<&LiveSeparator>) -> Result<Arc<LoadedTrack>> {
        if let Some(t) = self.get(id) {
            if t.stems.is_some() || live_sep.is_none() {
                return Ok(t);
            }
        }
        let lib = Library::open_default()?;
        let t = lib.track(id)?.context("unknown track")?;
        if lib.analysis(id)?.is_none_or(|a| a.version < clubcraft_core::Analysis::CURRENT_VERSION) {
            crate::pipeline::analyze(&lib, id, &Default::default(), &mut |_, _| {})?;
        }
        let analysis = lib.analysis(id)?.unwrap_or_default();
        let audio = Arc::new(clubcraft_audio::decode_file(&t.path, clubcraft_core::SAMPLE_RATE)?);
        let sp = stems_path(&t.hash);
        let stems = match clubcraft_stems::load_stem_track(&sp) {
            Ok(st) if st.frames.abs_diff(audio.frames()) < 4096 => Some(Arc::new(st)),
            _ => match live_sep {
                Some(sep) => {
                    let st = Arc::new(StemTrack::new(audio.frames(), "htdemucs"));
                    let start = analysis.cue(clubcraft_core::CueKind::FirstBeat).map(|c| c.pos).unwrap_or(0.0);
                    sep.submit(id, t.hash.clone(), audio.clone(), st.clone(), (start * clubcraft_core::SAMPLE_RATE as f64) as usize);
                    Some(st)
                }
                None => None,
            },
        };
        let lt = LoadedTrack::from_parts(&t, &analysis, stems, audio).with_user_cues(&lib.cues(id)?);
        let lt = Arc::new(lt);
        self.insert(id, lt.clone());
        Ok(lt)
    }
}

/// Background separation of tracks loaded on a deck before their stems were cached.
/// Uses the fast model so stems are available within seconds; the result is saved to the
/// cache (the library job queue may later upgrade it to the high-quality model).
pub struct LiveSeparator {
    tx: Sender<SepJob>,
    pub done: Receiver<TrackId>,
}

struct SepJob {
    id: TrackId,
    hash: String,
    audio: Arc<clubcraft_audio::AudioBuffer>,
    target: Arc<StemTrack>,
    start: usize,
    cancel: Arc<AtomicBool>,
}

impl LiveSeparator {
    pub fn new() -> LiveSeparator {
        let (tx, rx) = unbounded::<SepJob>();
        let (dtx, drx) = unbounded();
        std::thread::Builder::new()
            .name("cc-live-stems".into())
            .spawn(move || {
                let mut sep: Option<clubcraft_stems::Separator> = None;
                while let Ok(job) = rx.recv() {
                    if job.cancel.load(Ordering::Relaxed) || Arc::strong_count(&job.target) <= 1 {
                        continue;
                    }
                    if sep.is_none() {
                        match clubcraft_stems::Separator::new(clubcraft_stems::Quality::Fast) {
                            Ok(s) => sep = Some(s),
                            Err(e) => {
                                log::error!("live stems: {e:#}");
                                continue;
                            }
                        }
                    }
                    let s = sep.as_ref().unwrap();
                    // stop early when the deck unloads the track (only our Arc is left)
                    let target = job.target.clone();
                    let cancel = job.cancel.clone();
                    let r = s.separate_into(&job.audio, &target, job.start, &mut |_| {
                        if Arc::strong_count(&job.target) <= 2 {
                            cancel.store(true, Ordering::Relaxed);
                        }
                    }, &cancel);
                    match r {
                        Ok(()) if target.is_complete() => {
                            if let Err(e) = clubcraft_stems::save_stem_track(&stems_path(&job.hash), &target) {
                                log::warn!("saving live stems: {e:#}");
                            }
                            let _ = dtx.send(job.id);
                        }
                        Ok(()) => {}
                        Err(e) => log::warn!("live stems: {e:#}"),
                    }
                }
            })
            .expect("spawn live separator");
        LiveSeparator { tx, done: drx }
    }

    fn submit(&self, id: TrackId, hash: String, audio: Arc<clubcraft_audio::AudioBuffer>, target: Arc<StemTrack>, start: usize) {
        let _ = self.tx.send(SepJob { id, hash, audio, target, start, cancel: Arc::new(AtomicBool::new(false)) });
    }
}

enum LoadResult {
    Deck(usize, u64, Result<Arc<LoadedTrack>>),
    Mix { serial: u64, decks: [usize; 2], mix: Box<Mix>, from: f64, result: Result<()> },
}

pub struct Export {
    pub progress: Arc<AtomicU32>,
    pub done: Arc<Mutex<Option<Result<PathBuf, String>>>>,
}

pub struct LiveEngine {
    pub handle: Option<EngineHandle>,
    pub error: Option<String>,
    pub cache: TrackCache,
    pub sep: Arc<LiveSeparator>,
    tx: Sender<LoadResult>,
    rx: Receiver<LoadResult>,
    pending: [u64; 4],
    serial: u64,
    last_hot: [Option<(TrackId, u64, [Option<(f64, [u8; 3])>; HOT_CUES])>; 4],
    scratching: [bool; 4],
    /// Mix player per deck pair (studio preview on C/D, live mix on A/B).
    pub studio_player: Option<MixPlayer>,
    pub live_player: Option<MixPlayer>,
    pub loading_mix: Option<u64>,
    pub export: Option<Export>,
    pub automix_queue: Vec<TrackId>,
    /// Tracks whose library data changed (analysed while loading); the app refreshes them.
    pub touched: Vec<TrackId>,
}

impl LiveEngine {
    pub fn start() -> LiveEngine {
        let (tx, rx) = unbounded();
        let (handle, error) = match Engine::start(AudioConfig::default()) {
            Ok(h) => (Some(h), None),
            Err(e) => (None, Some(format!("Audio output unavailable: {e:#}"))),
        };
        if let Some(h) = &handle {
            let _ = h.send(Command::SetAutoGain(true));
        }
        LiveEngine {
            handle,
            error,
            cache: TrackCache::default(),
            sep: Arc::new(LiveSeparator::new()),
            tx,
            rx,
            pending: [0; 4],
            serial: 0,
            last_hot: Default::default(),
            scratching: [false; 4],
            studio_player: None,
            live_player: None,
            loading_mix: None,
            export: None,
            automix_queue: Vec::new(),
            touched: Vec::new(),
        }
    }

    pub fn state(&self) -> EngineState {
        self.handle.as_ref().map(|h| h.state()).unwrap_or_default()
    }

    fn send(&self, c: Command) {
        if let Some(h) = &self.handle {
            if let Err(e) = h.send(c) {
                log::warn!("engine: {e}");
            }
        }
    }

    pub fn load(&mut self, deck: usize, id: TrackId) {
        self.serial += 1;
        self.pending[deck] = self.serial;
        let (cache, tx, serial, sep) = (self.cache.clone(), self.tx.clone(), self.serial, self.sep.clone());
        std::thread::spawn(move || {
            let r = cache.load(id, Some(&sep));
            let _ = tx.send(LoadResult::Deck(deck, serial, r));
        });
    }

    pub fn is_loading(&self, deck: usize) -> bool {
        self.pending[deck] != 0
    }

    /// Prepare all tracks of `mix` in the background and start playing it on `decks`.
    pub fn play_mix(&mut self, mix: &Mix, decks: [usize; 2], from: f64) {
        self.serial += 1;
        self.loading_mix = Some(self.serial);
        let (cache, tx, serial, mix) = (self.cache.clone(), self.tx.clone(), self.serial, mix.clone());
        std::thread::spawn(move || {
            let mut result = Ok(());
            for t in &mix.tracks {
                if let Err(e) = cache.load(t.track_id, None) {
                    result = Err(e);
                    break;
                }
            }
            let _ = tx.send(LoadResult::Mix { serial, decks, mix: Box::new(mix), from, result });
        });
    }

    pub fn stop_mix(&mut self, decks: [usize; 2]) {
        let player = if decks == STUDIO_DECKS { self.studio_player.take() } else { self.live_player.take() };
        if let (Some(p), Some(h)) = (player, self.handle.as_mut()) {
            let _ = p.stop(h);
        }
        self.loading_mix = None;
    }

    pub fn studio_seek(&mut self, t: f64) {
        if let (Some(p), Some(h)) = (self.studio_player.as_ref(), self.handle.as_mut()) {
            let _ = p.seek(h, t);
        }
    }

    pub fn studio_pause(&mut self, pause: bool) {
        if let (Some(p), Some(h)) = (self.studio_player.as_ref(), self.handle.as_mut()) {
            let _ = if pause { p.pause(h) } else { p.resume(h) };
        }
    }

    /// Push mix edits into a running studio preview.
    pub fn studio_update(&mut self, mix: &Mix) {
        let cache = self.cache.clone();
        let state = self.state();
        if let (Some(p), Some(h)) = (self.studio_player.as_mut(), self.handle.as_mut()) {
            if let Err(e) = p.update(mix, &|id| cache.get(id), h, &state) {
                log::warn!("mix update: {e:#}");
            }
        }
    }

    pub fn export_mix(&mut self, mix: &Mix, out: PathBuf) {
        let progress = Arc::new(AtomicU32::new(0));
        let done = Arc::new(Mutex::new(None));
        self.export = Some(Export { progress: progress.clone(), done: done.clone() });
        let (cache, mix) = (self.cache.clone(), mix.clone());
        std::thread::spawn(move || {
            let r = (|| -> Result<PathBuf> {
                for t in &mix.tracks {
                    cache.load(t.track_id, None)?;
                }
                let opts = RenderOptions { format: WavFormat::Pcm24, ..Default::default() };
                let report = clubcraft_engine::render_mix(&mix, &|id| cache.get(id), &out, &opts, &mut |p| {
                    progress.store((p.clamp(0.0, 1.0) * 1000.0) as u32, Ordering::Relaxed);
                    true
                })?;
                log::info!("rendered {:?}", report);
                write_cue_sheet(&mix, &cache, &out)?;
                Ok(out)
            })();
            *done.lock() = Some(r.map_err(|e| format!("{e:#}")));
        });
    }

    /// Per-frame housekeeping: finished loads, hot cue persistence, stems finished.
    pub fn tick(&mut self, lib: &Library, sh: &Shared, jobs: &crate::jobs::Jobs) -> Vec<String> {
        let mut messages = Vec::new();
        while let Ok(r) = self.rx.try_recv() {
            match r {
                LoadResult::Deck(deck, serial, r) => {
                    if self.pending[deck] != serial {
                        continue;
                    }
                    self.pending[deck] = 0;
                    match r {
                        Ok(t) => {
                            if let Some(id) = t.track_id {
                                let _ = lib.increment_play_count(id);
                                self.touched.push(id);
                            }
                            self.send(Command::LoadTrack(deck, t));
                        }
                        Err(e) => messages.push(format!("Load failed: {e:#}")),
                    }
                }
                LoadResult::Mix { serial, decks, mix, from, result } => {
                    if self.loading_mix != Some(serial) {
                        continue;
                    }
                    self.loading_mix = None;
                    if let Err(e) = result {
                        messages.push(format!("Mix: {e:#}"));
                        continue;
                    }
                    let cache = self.cache.clone();
                    match MixPlayer::new(&mix, &|id| cache.get(id), decks) {
                        Ok(p) => {
                            if let Some(h) = self.handle.as_mut() {
                                if let Err(e) = p.play(h, from) {
                                    messages.push(format!("Mix: {e:#}"));
                                }
                            }
                            if decks == STUDIO_DECKS {
                                self.studio_player = Some(p);
                            } else {
                                self.live_player = Some(p);
                            }
                        }
                        Err(e) => messages.push(format!("Mix: {e:#}")),
                    }
                }
            }
        }
        while let Ok(id) = self.sep.done.try_recv() {
            self.cache.invalidate(id);
            self.touched.push(id);
            jobs.analyze_only(id); // re-analyse with stems for better cues
        }
        // Persist hot cues the user set or moved.
        let st = self.state();
        for d in 0..2 {
            let ds = &st.decks[d];
            let Some(id) = ds.track_id else { continue };
            let now: [Option<(f64, [u8; 3])>; HOT_CUES] = std::array::from_fn(|i| ds.hot_cues[i].map(|h| (h.pos, h.color)));
            match &self.last_hot[d] {
                Some((tid, serial, prev)) if *tid == id && *serial == ds.load_serial => {
                    if *prev != now {
                        self.save_hot_cues(lib, sh, id, &now);
                        self.last_hot[d] = Some((id, ds.load_serial, now));
                    }
                }
                _ => self.last_hot[d] = Some((id, ds.load_serial, now)),
            }
        }
        let finished = self.export.as_ref().and_then(|ex| ex.done.lock().take());
        if let Some(r) = finished {
            messages.push(match r {
                Ok(p) => format!("Exported {}", p.display()),
                Err(e) => format!("Export failed: {e}"),
            });
            self.export = None;
        }
        messages
    }

    fn save_hot_cues(&self, lib: &Library, sh: &Shared, id: TrackId, now: &[Option<(f64, [u8; 3])>; HOT_CUES]) {
        let auto = sh.analyses.get(&id).map(|a| a.cues.clone()).unwrap_or_default();
        let mut cues: Vec<Cue> = lib.cues(id).unwrap_or_default().into_iter().filter(|c| c.slot.is_none() || c.source != CueSource::User).collect();
        for (slot, h) in now.iter().enumerate() {
            let Some((pos, color)) = h else { continue };
            let auto_same = auto.iter().any(|c| c.slot == Some(slot as u8) && (c.pos - pos).abs() < 0.002);
            if auto_same {
                continue;
            }
            cues.retain(|c| c.slot != Some(slot as u8));
            cues.push(Cue { slot: Some(slot as u8), kind: clubcraft_core::CueKind::Hot, pos: *pos, name: format!("Cue {}", slot + 1), color: *color, loop_beats: None, source: CueSource::User });
        }
        if let Err(e) = lib.set_cues(id, &cues) {
            log::warn!("saving cues: {e}");
        }
        self.cache.invalidate(id);
    }

    /// Translate one UI action.
    pub fn apply(&mut self, a: LiveAction, decks: &[DeckUi], sh: &Shared) {
        use LiveAction as A;
        let stem = |i: usize| StemKind::ALL[i.min(3)];
        match a {
            A::Load(d, id) => self.load(d, id),
            A::Eject(d) => self.send(Command::Unload(d)),
            A::PlayPause(d) => self.send(Command::PlayPause(d)),
            A::CueDown(d) => self.send(Command::CueDown(d)),
            A::CueUp(d) => self.send(Command::CueUp(d)),
            A::Seek(d, t) => self.send(Command::Seek(d, t)),
            A::Scrub(d, secs) => {
                if !self.scratching[d] {
                    self.scratching[d] = true;
                    self.send(Command::JogTouch(d, true));
                }
                self.send(Command::Jog(d, secs / clubcraft_engine::deck::SECONDS_PER_REV));
            }
            A::ScrubEnd(d) => {
                self.scratching[d] = false;
                self.send(Command::JogTouch(d, false));
            }
            A::Sync(d) => {
                let on = decks.get(d).is_some_and(|x| x.synced);
                self.send(if on { Command::SetSync(d, false) } else { Command::Sync(d) });
            }
            A::SetMaster(d) => {
                let is = decks.get(d).is_some_and(|x| x.master);
                self.send(Command::SetMaster(if is { None } else { Some(d) }));
            }
            A::Pitch(d, pct) => {
                let range = decks.get(d).map(|x| x.pitch_range).unwrap_or(8.0).max(1.0);
                self.send(Command::SetPitch(d, (pct / range).clamp(-1.0, 1.0)));
            }
            A::PitchRange(d, r) => self.send(Command::SetPitchRange(d, PitchRange::from_percent(r as u32).unwrap_or_default())),
            A::PitchBend(d, v) => self.send(Command::PitchBend(d, (v / 4.0).clamp(-1.0, 1.0))),
            A::Keylock(d, on) => self.send(Command::SetKeylock(d, on)),
            A::KeyShift(d, s) => self.send(Command::SetKeyShift(d, s.clamp(-12, 12) as f32)),
            A::MatchKey(d) => {
                let st = self.state();
                let master = st.master_deck.filter(|m| *m != d).or_else(|| (0..2).find(|x| *x != d && st.decks[*x].playing));
                if let (Some(m), Some(k)) = (master, st.decks[d].key) {
                    if let Some(mk) = st.decks[m].effective_key {
                        let best = (-3..=3).map(|s: i32| (s, mk.compatibility(k.shifted(s)) - s.abs() as f32 * 0.02)).max_by(|a, b| a.1.total_cmp(&b.1)).map(|x| x.0).unwrap_or(0);
                        self.send(Command::SetKeyShift(d, best as f32));
                    }
                }
            }
            A::HotCue(d, n) => self.send(Command::HotCueDown(d, n as usize)),
            A::HotCueRelease(d, n) => self.send(Command::HotCueUp(d, n as usize)),
            A::DeleteHotCue(d, n) => self.send(Command::DeleteHotCue(d, n as usize)),
            A::AutoLoop(d, b) => {
                let active = decks.get(d).is_some_and(|x| x.loop_active && (x.auto_loop_beats - b).abs() < 1e-6);
                self.send(if active { Command::LoopExit(d) } else { Command::AutoLoop(d, b) });
            }
            A::LoopToggle(d) => {
                let active = decks.get(d).is_some_and(|x| x.loop_active);
                self.send(if active { Command::LoopExit(d) } else { Command::Reloop(d) });
            }
            A::LoopHalve(d) => self.send(Command::LoopHalve(d)),
            A::LoopDouble(d) => self.send(Command::LoopDouble(d)),
            A::LoopIn(d) => self.send(Command::LoopIn(d)),
            A::LoopOut(d) => self.send(Command::LoopOut(d)),
            A::LoopRoll(d, beats, on) => self.send(Command::LoopRoll { deck: d, beats, on }),
            A::BeatJump(d, b) => self.send(Command::BeatJump(d, b)),
            A::Slip(d, on) => self.send(Command::SetSlip(d, on)),
            A::Quantize(d, on) => self.send(Command::SetQuantize(d, on)),
            A::StemGain(d, s, g) => self.send(Command::SetStemGain { deck: d, stem: stem(s), gain: g }),
            A::StemPreset(d, p) => match p {
                StemPreset::All => self.send(Command::StemsReset(d)),
                StemPreset::Acapella => self.send(Command::Acapella(d)),
                StemPreset::Instrumental => self.send(Command::Instrumental(d)),
                other => {
                    for (i, g) in other.gains().iter().enumerate() {
                        self.send(Command::SetStemGain { deck: d, stem: stem(i), gain: *g });
                    }
                }
            },
            A::StemSolo(d, s) => self.send(Command::StemSolo(d, stem(s))),
            A::Eq(d, b, v) => self.send(Command::SetEq(d, [EqBand::Low, EqBand::Mid, EqBand::High][b.min(2)], v)),
            A::Filter(d, v) => self.send(Command::SetFilter(d, v)),
            A::Gain(d, v) => self.send(Command::SetTrim(d, (v - 0.5) * 24.0)),
            A::Fader(d, v) => self.send(Command::SetFader(d, v)),
            A::Pfl(d, on) => self.send(Command::SetPfl(d, on)),
            A::Fx(d, slot, fx) => {
                let kind = FX_KINDS.get(fx.kind).copied().unwrap_or(FxKind::Echo);
                self.send(Command::SetFx { deck: d, slot, kind });
                self.send(Command::SetFxWet { deck: d, slot, wet: fx.amount });
                self.send(Command::SetFxActive { deck: d, slot, on: fx.on });
            }
            A::Crossfader(x) => self.send(Command::SetCrossfader((x + 1.0) * 0.5)),
            A::MasterVol(v) => self.send(Command::SetMasterGain(knob_db(v, 0.8))),
            A::HeadphoneMix(v) => self.send(Command::SetHeadphoneMix(v)),
            A::HeadphoneVol(v) => self.send(Command::SetHeadphoneGain(knob_db(v, 0.7))),
            A::Record(on) => {
                if let Some(h) = &self.handle {
                    if on {
                        let name = format!("ClubCraft {}.wav", chrono_like_now());
                        let _ = std::fs::create_dir_all(clubcraft_core::paths::recordings_dir());
                        if let Err(e) = h.start_recording(&clubcraft_core::paths::recordings_dir().join(name), WavFormat::Pcm24) {
                            log::error!("recording: {e:#}");
                        }
                    } else {
                        let _ = h.stop_recording();
                    }
                }
            }
            A::Automix(on) => {
                if on {
                    let queue = self.automix_queue.clone();
                    if queue.len() >= 2 {
                        let planned = clubcraft_core::planner::plan_mix("Automix", &queue, &|id| sh.analyses.get(&id).cloned(), &Default::default());
                        self.play_mix(&planned, LIVE_DECKS, 0.0);
                    }
                } else {
                    self.stop_mix(LIVE_DECKS);
                }
            }
            A::AutomixQueue(ids) => self.automix_queue = ids,
        }
    }
}

/// FX kinds selectable in the UI (index = `FxUi::kind`).
pub const FX_KINDS: [FxKind; 9] = [FxKind::Echo, FxKind::PingPong, FxKind::Reverb, FxKind::Flanger, FxKind::FilterLfo, FxKind::Gate, FxKind::Roll, FxKind::Bitcrusher, FxKind::Brake];

/// Knob 0..1 → dB with `unity` at 0 dB (−∞ at 0, +6 dB at 1).
fn knob_db(v: f32, unity: f32) -> f32 {
    if v <= 0.001 {
        -80.0
    } else if v <= unity {
        40.0 * (v / unity).log10()
    } else {
        (v - unity) / (1.0 - unity) * 6.0
    }
}

fn chrono_like_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // local time via `date`; fall back to epoch seconds
    std::process::Command::new("date")
        .arg("+%Y-%m-%d %H.%M.%S")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| secs.to_string())
}

/// `.cue` sheet next to an exported mix (75 frames per second).
fn write_cue_sheet(mix: &Mix, cache: &TrackCache, wav: &std::path::Path) -> Result<()> {
    let tl = clubcraft_core::MixTimeline::build(mix, &|id| cache.get(id).map(|t| t.analysis()));
    let mut s = format!("PERFORMER \"ClubCraft\"\nTITLE \"{}\"\nFILE \"{}\" WAVE\n", mix.name.replace('"', "'"), wav.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default());
    for (i, t) in tl.tracks.iter().enumerate() {
        // a track "starts" in the middle of the transition that brings it in
        let start = if i == 0 { 0.0 } else { (tl.transitions[i - 1].start_sec + tl.transitions[i - 1].end_sec) / 2.0 };
        let (title, artist) = cache.get(t.track_id).map(|l| (l.title.clone(), l.artist.clone())).unwrap_or_default();
        let frames = (start * 75.0).round() as u64;
        s += &format!(
            "  TRACK {:02} AUDIO\n    TITLE \"{}\"\n    PERFORMER \"{}\"\n    INDEX 01 {:02}:{:02}:{:02}\n",
            i + 1,
            title.replace('"', "'"),
            artist.replace('"', "'"),
            frames / 75 / 60,
            frames / 75 % 60,
            frames % 75
        );
    }
    std::fs::write(wav.with_extension("cue"), s)?;
    Ok(())
}

/// Build the view-model of deck `d` from engine state + library data.
pub fn deck_ui(st: &EngineState, d: usize, sh: &Shared, fx_ui: &[FxUi; 3], loading: bool) -> DeckUi {
    let ds = &st.decks[d];
    let mut u = DeckUi { loaded: ds.loaded || loading, ..Default::default() };
    if let Some(id) = ds.track_id.filter(|_| ds.loaded) {
        u.track_id = Some(id);
        if let Some(t) = sh.track(id) {
            u.title = t.title.clone();
            u.artist = t.artist.clone();
        }
        u.waveform = sh.waveforms.get(&id).cloned();
        if let Some(a) = sh.analyses.get(&id) {
            u.sections = a.sections.clone();
            u.grid = a.grid;
        }
        // hot cues as the engine has them + markers from the library
        let lib_cues = sh.effective_cues(id);
        for (slot, h) in ds.hot_cues.iter().enumerate() {
            if let Some(h) = h {
                let name = lib_cues.iter().find(|c| c.slot == Some(slot as u8)).map(|c| c.name.clone()).unwrap_or_else(|| h.kind.label().to_string());
                u.cues.push(Cue { slot: Some(slot as u8), kind: h.kind, pos: h.pos, name, color: h.color, loop_beats: h.loop_beats, source: CueSource::Auto });
            }
        }
        u.cues.extend(lib_cues.into_iter().filter(|c| c.slot.is_none()));
    } else if loading {
        u.title = "Loading…".into();
    }
    u.duration = ds.duration;
    u.position = ds.position;
    u.playing = ds.playing;
    u.base_bpm = ds.bpm;
    u.bpm = ds.effective_bpm;
    u.pitch_pct = ds.pitch_percent as f32;
    u.pitch_range = ds.pitch_range.percent() as f32;
    u.keylock = ds.keylock;
    u.key = ds.key;
    u.key_shift = ds.key_shift.round() as i32;
    u.synced = ds.sync;
    u.master = ds.is_master;
    u.slip = ds.slip_mode;
    u.quantize = ds.quantize;
    u.cue_point = ds.cue;
    let ls = ds.loop_state;
    u.loop_region = if ls.defined { Some((ls.start, ls.end)) } else { None };
    u.loop_active = ls.active;
    u.auto_loop_beats = ls.beats.unwrap_or(ls.auto_beats).max(1.0 / 32.0);
    u.stem_gains = ds.stem_gains;
    u.has_stems = ds.has_stems;
    u.stems_ready = ds.stems_ready;
    u.eq = ds.eq;
    u.filter = ds.filter;
    u.gain = (ds.trim_db / 24.0 + 0.5).clamp(0.0, 1.0);
    u.fader = ds.fader;
    u.pfl = ds.pfl;
    u.vu = ds.level.peak;
    u.fx = *fx_ui;
    for (i, f) in ds.fx.iter().enumerate().take(3) {
        u.fx[i].on = f.active;
        u.fx[i].amount = f.wet;
        if let Some(k) = FX_KINDS.iter().position(|k| *k == f.kind) {
            u.fx[i].kind = k;
        }
    }
    u.phase_offset = if ds.sync || st.master_deck.is_some_and(|m| m != d) { ds.phase_offset } else { None };
    u
}

pub fn mixer_ui(st: &EngineState, eng: &LiveEngine) -> MixerUi {
    MixerUi {
        crossfader: st.master.crossfader * 2.0 - 1.0,
        master: db_knob(st.master.gain_db, 0.8),
        headphone_mix: st.master.headphone_mix,
        headphone_vol: db_knob(st.master.headphone_gain_db, 0.7),
        master_vu: st.master.level.peak,
        recording: st.recording.active,
        record_secs: st.recording.seconds,
        master_bpm: st.master_deck.and_then(|m| st.decks[m].effective_bpm),
        automix: eng.live_player.is_some() || (eng.loading_mix.is_some()),
        output_name: eng.handle.as_ref().map(|h| {
            let i = h.stream_info();
            format!("{} · {} Hz", i.device, i.sample_rate)
        }).or_else(|| eng.error.clone()).unwrap_or_default(),
        cpu: st.cpu_load,
    }
}

fn db_knob(db: f32, unity: f32) -> f32 {
    if db <= -79.0 {
        0.0
    } else if db <= 0.0 {
        unity * 10f32.powf(db / 40.0)
    } else {
        unity + db / 6.0 * (1.0 - unity)
    }
}
