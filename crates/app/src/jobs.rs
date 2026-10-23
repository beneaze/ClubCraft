//! Background job system: imports, stem separation (GPU queue) and analysis (CPU pool).
//! Workers own their own SQLite connections and report through a channel the UI drains
//! every frame.

use crate::pipeline;
use clubcraft_core::TrackId;
use crossbeam_channel::{unbounded, Receiver, Sender};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum Event {
    /// Library contents changed (new tracks).
    LibraryChanged,
    /// A track's analysis / stems were (re)written.
    TrackUpdated(TrackId),
    Progress { track: Option<TrackId>, frac: f32, label: String },
    Finished { track: Option<TrackId> },
    Error { track: Option<TrackId>, message: String },
}

#[derive(Debug, Clone)]
enum Job {
    Import(Vec<PathBuf>, bool),
    /// Separate stems; `Some(quality)` overrides the playback quality (export).
    Separate(TrackId, Option<String>),
    /// Fast stems for parts of a track only (track seconds): its stem transitions.
    SeparateRegions(TrackId, Vec<(f64, f64)>),
    Analyze(TrackId),
}

struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    cv: parking_lot::Condvar,
}

impl Queue {
    fn new() -> Arc<Queue> {
        Arc::new(Queue { jobs: Mutex::new(VecDeque::new()), cv: parking_lot::Condvar::new() })
    }
    fn push(&self, j: Job, front: bool) {
        let mut q = self.jobs.lock();
        if front {
            q.push_front(j);
        } else {
            q.push_back(j);
        }
        self.cv.notify_one();
    }
    fn pop(&self) -> Job {
        let mut q = self.jobs.lock();
        loop {
            if let Some(j) = q.pop_front() {
                return j;
            }
            self.cv.wait(&mut q);
        }
    }
    fn len(&self) -> usize {
        self.jobs.lock().len()
    }
}

#[derive(Default, Clone, Debug)]
pub struct JobStatus {
    pub frac: f32,
    pub label: String,
}

pub struct Jobs {
    tx: Sender<Event>,
    rx: Receiver<Event>,
    cpu: Arc<Queue>,
    gpu: Arc<Queue>,
    pub settings: Arc<Mutex<pipeline::Settings>>,
    pub active: HashMap<Option<TrackId>, JobStatus>,
    pub log: VecDeque<String>,
    cancel: Arc<AtomicBool>,
}

impl Jobs {
    pub fn new(settings: pipeline::Settings) -> Jobs {
        let (tx, rx) = unbounded();
        let cpu = Queue::new();
        let gpu = Queue::new();
        let settings = Arc::new(Mutex::new(settings));
        let cancel = Arc::new(AtomicBool::new(false));
        // Analysis / import workers.
        // One analysis worker: with stems and the structure model an analysis holds ~1-3 GB.
        // Separation and analysis additionally share HEAVY so they never run at the same time.
        for i in 0..1 {
            let (q, g, tx, st, cn) = (cpu.clone(), gpu.clone(), tx.clone(), settings.clone(), cancel.clone());
            std::thread::Builder::new()
                .name(format!("cc-analysis-{i}"))
                .spawn(move || worker(q, g, tx, st, cn, false))
                .map_err(|e| log::error!("cannot start the analysis worker: {e}"))
                .ok();
        }
        // One separation worker: the GPU is the bottleneck, and models are large.
        {
            let (q, c, tx, st, cn) = (gpu.clone(), cpu.clone(), tx.clone(), settings.clone(), cancel.clone());
            std::thread::Builder::new().name("cc-stems".into()).spawn(move || worker(q, c, tx, st, cn, true)).map_err(|e| log::error!("cannot start the stems worker: {e}")).ok();
        }
        Jobs { tx, rx, cpu, gpu, settings, active: HashMap::new(), log: VecDeque::new(), cancel }
    }

    pub fn sender(&self) -> Sender<Event> {
        self.tx.clone()
    }

    /// Import files/folders; `analyze` queues analysis (and separation if enabled) afterwards.
    pub fn import(&self, paths: Vec<PathBuf>, analyze: bool) {
        self.cpu.push(Job::Import(paths, analyze), false);
    }

    /// Full processing of a track: stems (if enabled) then analysis.
    /// Analyse now (fast, without stems) and separate stems afterwards (if enabled); the
    /// separation job re-analyses with the stems when it is done.
    /// Separate stems now (front of the queue), then re-analyse.
    /// Fast stems for the parts `regions` (track seconds) of a track, ahead of other separation.
    pub fn separate_regions(&self, id: TrackId, regions: Vec<(f64, f64)>) {
        self.gpu.push(Job::SeparateRegions(id, regions), true);
    }

    pub fn separate_now(&self, id: TrackId) {
        self.gpu.push(Job::Separate(id, None), true);
    }

    /// Separate now at `quality` (e.g. the export quality), unless the cached stems are as good.
    pub fn separate_at(&self, id: TrackId, quality: &str) {
        self.gpu.push(Job::Separate(id, Some(quality.to_string())), true);
    }

    /// Background processing (import, opened mixes): stems only if "Separate in the background" is on.
    pub fn process(&self, id: TrackId, priority: bool) {
        let stems = self.settings.lock().separate_stems;
        self.process_with(id, priority, stems);
    }

    /// Asked for by the user ("Analyse + separate stems"): stems whatever the background setting.
    pub fn process_with_stems(&self, id: TrackId, priority: bool) {
        self.process_with(id, priority, true);
    }

    fn process_with(&self, id: TrackId, priority: bool, stems: bool) {
        self.cpu.push(Job::Analyze(id), priority);
        if stems {
            self.gpu.push(Job::Separate(id, None), priority);
        }
    }

    pub fn analyze_only(&self, id: TrackId) {
        self.cpu.push(Job::Analyze(id), true);
    }

    pub fn pending(&self) -> usize {
        self.cpu.len() + self.gpu.len() + self.active.len()
    }

    pub fn cancel_all(&self) {
        self.cpu.jobs.lock().clear();
        self.gpu.jobs.lock().clear();
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Drain events; returns them for the app to react to.
    pub fn poll(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(e) = self.rx.try_recv() {
            match &e {
                Event::Progress { track, frac, label } => {
                    self.active.insert(*track, JobStatus { frac: *frac, label: label.clone() });
                }
                Event::Finished { track, .. } => {
                    self.active.remove(track);
                }
                Event::Error { track, message } => {
                    // the status line shows it only until the next message
                    log::warn!("job for track {track:?}: {message}");
                    self.active.remove(track);
                    self.log.push_back(message.clone());
                    if self.log.len() > 50 {
                        self.log.pop_front();
                    }
                }
                _ => {}
            }
            out.push(e);
        }
        if self.cpu.len() + self.gpu.len() == 0 && self.active.is_empty() {
            self.cancel.store(false, Ordering::SeqCst);
        }
        out
    }
}

/// Serialises memory-heavy work (stem separation, full analysis) across all workers. Peak use of
/// one job is up to ~3 GB; running several at once ran a 16 GB Mac out of memory.
pub static HEAVY: Mutex<()> = parking_lot::const_mutex(());

/// Held while memory-heavy work runs: the in-process [`HEAVY`] lock plus an OS file lock on
/// `<data>/heavy.lock`, so the app and `clubcraft mcp` never separate at the same time either.
pub struct HeavyGuard {
    _lock: parking_lot::MutexGuard<'static, ()>,
    _file: Option<std::fs::File>,
}

pub fn heavy() -> HeavyGuard {
    let lock = HEAVY.lock();
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(clubcraft_core::paths::data_dir().join("heavy.lock")).ok();
    if let Some(f) = &file {
        if let Err(e) = f.lock() {
            log::warn!("heavy.lock: {e}");
        }
    }
    HeavyGuard { _lock: lock, _file: file }
}

fn worker(q: Arc<Queue>, other: Arc<Queue>, tx: Sender<Event>, settings: Arc<Mutex<pipeline::Settings>>, cancel: Arc<AtomicBool>, gpu: bool) {
    let lib = match clubcraft_core::Library::open_default() {
        Ok(l) => l,
        Err(e) => {
            let _ = tx.send(Event::Error { track: None, message: format!("library: {e}") });
            return;
        }
    };
    let mut ctx = pipeline::WorkerContext::default();
    loop {
        if q.len() == 0 {
            // Idle: free the stem models (~1 GB+) until the next job.
            ctx.release();
        }
        let job = q.pop();
        let s = settings.lock().clone();
        match job {
            Job::Import(paths, analyze) => {
                let files = crate::importer::collect_audio(&paths);
                let n = files.len();
                for (i, f) in files.iter().enumerate() {
                    let _ = tx.send(Event::Progress {
                        track: None,
                        frac: i as f32 / n.max(1) as f32,
                        label: format!("Importing {}/{}", i + 1, n),
                    });
                    match crate::importer::import_file(&lib, f) {
                        Ok(id) => {
                            if analyze {
                                let needs = lib.analysis(id).ok().flatten().map(|a| a.version < clubcraft_core::Analysis::CURRENT_VERSION).unwrap_or(true);
                                if needs {
                                    // Quick analysis first (key, BPM, grid, cues in ~1 s), stems
                                    // later; analysis is redone with the stems afterwards.
                                    other_or_self(&q, &other, gpu, false).push(Job::Analyze(id), false);
                                    if s.separate_stems {
                                        other_or_self(&q, &other, gpu, true).push(Job::Separate(id, None), false);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Event::Error { track: None, message: format!("{}: {e}", f.display()) });
                        }
                    }
                    if i % 20 == 19 {
                        let _ = tx.send(Event::LibraryChanged);
                    }
                }
                let _ = tx.send(Event::LibraryChanged);
                let _ = tx.send(Event::Finished { track: None });
            }
            Job::Separate(id, quality) => {
                let _heavy = heavy();
                let s = match quality {
                    Some(q) => pipeline::Settings { quality: q, ..s.clone() },
                    None => s.clone(),
                };
                let r = pipeline::separate(&lib, &mut ctx, id, &s, &cancel, &mut |frac, label| {
                    let _ = tx.send(Event::Progress { track: Some(id), frac, label: label.to_string() });
                });
                match r {
                    Ok(()) => {
                        let _ = tx.send(Event::Finished { track: Some(id) });
                        let _ = tx.send(Event::TrackUpdated(id));
                    }
                    Err(e) => {
                        let _ = tx.send(Event::Error { track: Some(id), message: format!("stems: {e:#}") });
                    }
                }
                // analysis always follows separation
                other_or_self(&q, &other, gpu, false).push(Job::Analyze(id), true);
            }
            Job::SeparateRegions(id, regions) => {
                let _heavy = heavy();
                let r = pipeline::separate_regions(&lib, &mut ctx, id, &regions, &cancel, &mut |frac, label| {
                    let _ = tx.send(Event::Progress { track: Some(id), frac, label: label.to_string() });
                });
                match r {
                    Ok(changed) => {
                        let _ = tx.send(Event::Finished { track: Some(id) });
                        if changed {
                            let _ = tx.send(Event::TrackUpdated(id));
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Event::Error { track: Some(id), message: format!("stems: {e:#}") });
                    }
                }
            }
            Job::Analyze(id) => {
                // Only analysis with stems (and the optional structure model) is heavy; the quick
                // analysis of a fresh import must not wait behind a long separation.
                let has_stems = lib.track(id).ok().flatten().is_some_and(|t| clubcraft_core::library::stems_path(&t.hash).exists());
                let _heavy = if has_stems { Some(heavy()) } else { None };
                let r = pipeline::analyze(&lib, id, &s, &mut |frac, label| {
                    let _ = tx.send(Event::Progress { track: Some(id), frac, label: label.to_string() });
                });
                match r {
                    Ok(()) => {
                        let _ = tx.send(Event::Finished { track: Some(id) });
                        let _ = tx.send(Event::TrackUpdated(id));
                    }
                    Err(e) => {
                        let _ = tx.send(Event::Error { track: Some(id), message: format!("analysis: {e:#}") });
                    }
                }
            }
        }
    }
}

/// Pick the right queue for a job from inside a worker.
fn other_or_self<'a>(q: &'a Arc<Queue>, other: &'a Arc<Queue>, i_am_gpu: bool, want_gpu: bool) -> &'a Arc<Queue> {
    if i_am_gpu == want_gpu {
        q
    } else {
        other
    }
}
