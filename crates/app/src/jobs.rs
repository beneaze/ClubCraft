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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Import,
    Separate,
    Analyze,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// Library contents changed (new tracks).
    LibraryChanged,
    /// A track's analysis / stems were (re)written.
    TrackUpdated(TrackId),
    Progress { track: Option<TrackId>, stage: Stage, frac: f32, label: String },
    Finished { track: Option<TrackId>, stage: Stage },
    Error { track: Option<TrackId>, message: String },
}

#[derive(Debug, Clone)]
enum Job {
    Import(Vec<PathBuf>, bool),
    Separate(TrackId),
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
    pub stage: Option<Stage>,
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
        let n_cpu = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 6) / 2;
        for i in 0..n_cpu.max(1) {
            let (q, g, tx, st, cn) = (cpu.clone(), gpu.clone(), tx.clone(), settings.clone(), cancel.clone());
            std::thread::Builder::new()
                .name(format!("cc-analysis-{i}"))
                .spawn(move || worker(q, g, tx, st, cn, false))
                .expect("spawn worker");
        }
        // One separation worker: the GPU is the bottleneck, and models are large.
        {
            let (q, c, tx, st, cn) = (gpu.clone(), cpu.clone(), tx.clone(), settings.clone(), cancel.clone());
            std::thread::Builder::new().name("cc-stems".into()).spawn(move || worker(q, c, tx, st, cn, true)).expect("spawn stems worker");
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
    pub fn process(&self, id: TrackId, priority: bool) {
        if self.settings.lock().separate_stems {
            self.gpu.push(Job::Separate(id), priority);
        } else {
            self.cpu.push(Job::Analyze(id), priority);
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
                Event::Progress { track, stage, frac, label } => {
                    self.active.insert(*track, JobStatus { stage: Some(*stage), frac: *frac, label: label.clone() });
                }
                Event::Finished { track, .. } => {
                    self.active.remove(track);
                }
                Event::Error { track, message } => {
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
        let job = q.pop();
        let s = settings.lock().clone();
        match job {
            Job::Import(paths, analyze) => {
                let files = crate::importer::collect_audio(&paths);
                let n = files.len();
                for (i, f) in files.iter().enumerate() {
                    let _ = tx.send(Event::Progress {
                        track: None,
                        stage: Stage::Import,
                        frac: i as f32 / n.max(1) as f32,
                        label: format!("Importing {}/{}", i + 1, n),
                    });
                    match crate::importer::import_file(&lib, f) {
                        Ok(id) => {
                            if analyze {
                                let needs = lib.analysis(id).ok().flatten().map(|a| a.version < clubcraft_core::Analysis::CURRENT_VERSION).unwrap_or(true);
                                if needs {
                                    if s.separate_stems {
                                        other_or_self(&q, &other, gpu, true).push(Job::Separate(id), false);
                                    } else {
                                        other_or_self(&q, &other, gpu, false).push(Job::Analyze(id), false);
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
                let _ = tx.send(Event::Finished { track: None, stage: Stage::Import });
            }
            Job::Separate(id) => {
                let r = pipeline::separate(&lib, &mut ctx, id, &s, &cancel, &mut |frac, label| {
                    let _ = tx.send(Event::Progress { track: Some(id), stage: Stage::Separate, frac, label: label.to_string() });
                });
                match r {
                    Ok(()) => {
                        let _ = tx.send(Event::Finished { track: Some(id), stage: Stage::Separate });
                        let _ = tx.send(Event::TrackUpdated(id));
                    }
                    Err(e) => {
                        let _ = tx.send(Event::Error { track: Some(id), message: format!("stems: {e:#}") });
                    }
                }
                // analysis always follows separation
                other_or_self(&q, &other, gpu, false).push(Job::Analyze(id), true);
            }
            Job::Analyze(id) => {
                let r = pipeline::analyze(&lib, id, &s, &mut |frac, label| {
                    let _ = tx.send(Event::Progress { track: Some(id), stage: Stage::Analyze, frac, label: label.to_string() });
                });
                match r {
                    Ok(()) => {
                        let _ = tx.send(Event::Finished { track: Some(id), stage: Stage::Analyze });
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
