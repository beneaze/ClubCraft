use crate::jobs::{Event, JobStatus, Jobs};
use crate::library_view::{LibraryAction, LibraryView};
use crate::theme;
use clubcraft_core::{Analysis, Library, Track, TrackId};
use eframe::egui::{self, RichText};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum View {
    Home,
    Library,
    Studio,
    Live,
}

/// Library data cached for the UI (refreshed on job events).
#[derive(Default)]
pub struct Shared {
    pub tracks: Vec<Track>,
    pub by_id: HashMap<TrackId, usize>,
    pub analyses: HashMap<TrackId, Analysis>,
    pub playlists: Vec<(i64, String)>,
    pub playlist_tracks: HashMap<i64, Vec<TrackId>>,
    /// Every mix (project) with its tracks in order, for the library's mix filter.
    pub mix_tracks: Vec<(i64, String, Vec<TrackId>)>,
    pub job_status: HashMap<Option<TrackId>, JobStatus>,
    pub cue_counts: HashMap<TrackId, usize>,
    pub waveforms: HashMap<TrackId, std::sync::Arc<clubcraft_core::Waveform>>,
    pub user_cues: HashMap<TrackId, Vec<clubcraft_core::Cue>>,
    pub deck_count: usize,
    /// Parts of tracks without complete stems that have stems for mix playback (track seconds,
    /// see `pipeline::separate_regions`).
    pub stem_regions: HashMap<TrackId, Vec<(f64, f64)>>,
}

/// Identity of a region separation job: the track and its parts to 0.1 s.
fn region_key(id: TrackId, regions: &[(f64, f64)]) -> (TrackId, Vec<(i64, i64)>) {
    (id, regions.iter().map(|&(a, b)| ((a * 10.0).round() as i64, (b * 10.0).round() as i64)).collect())
}

/// The parts (track seconds) a track's partial stem file covers, merged into ranges.
fn region_cover(hash: &str) -> Option<Vec<(f64, f64)>> {
    use clubcraft_core::{SAMPLE_RATE, STEM_CHUNK_FRAMES};
    let p = clubcraft_core::library::region_stems_path(hash);
    if !p.exists() {
        return None;
    }
    let mut chunks = clubcraft_stems::read_partial_chunks(&p).ok().flatten()?;
    chunks.sort_unstable();
    let sec = |c: usize| (c * STEM_CHUNK_FRAMES) as f64 / SAMPLE_RATE as f64;
    let mut out: Vec<(f64, f64)> = Vec::new();
    for c in chunks {
        match out.last_mut() {
            Some(last) if (last.1 - sec(c)).abs() < 1e-9 => last.1 = sec(c + 1),
            _ => out.push((sec(c), sec(c + 1))),
        }
    }
    Some(out)
}

impl Shared {
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.by_id.get(&id).map(|&i| &self.tracks[i])
    }

    pub fn reload(&mut self, lib: &Library) {
        self.tracks = lib.tracks().unwrap_or_default();
        self.by_id = self.tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        self.analyses = lib.all_analyses().unwrap_or_default();
        self.playlists = lib.playlists().unwrap_or_default();
        self.playlist_tracks = self.playlists.iter().map(|(id, _)| (*id, lib.playlist_tracks(*id).unwrap_or_default())).collect();
        let ids: Vec<TrackId> = self.tracks.iter().map(|t| t.id).collect();
        for id in ids {
            self.load_side_data(lib, id);
        }
    }

    fn load_side_data(&mut self, lib: &Library, id: TrackId) {
        if let Some(t) = self.track(id) {
            let hash = t.hash.clone();
            if self.analyses.contains_key(&id) {
                if let Some(wf) = clubcraft_core::Waveform::load(&hash) {
                    self.waveforms.insert(id, std::sync::Arc::new(wf));
                }
            }
            match region_cover(&hash) {
                Some(r) => self.stem_regions.insert(id, r),
                None => self.stem_regions.remove(&id),
            };
        }
        let cues = lib.cues(id).unwrap_or_default();
        self.cue_counts.insert(id, cues.len());
        self.user_cues.insert(id, cues);
    }

    /// Auto cues merged with the user's own and imported cues.
    pub fn effective_cues(&self, id: TrackId) -> Vec<clubcraft_core::Cue> {
        let auto = self.analyses.get(&id).map(|a| a.cues.clone()).unwrap_or_default();
        clubcraft_core::effective_cues(&auto, self.user_cues.get(&id).map(|v| v.as_slice()).unwrap_or(&[]))
    }

    pub fn reload_track(&mut self, lib: &Library, id: TrackId) {
        if let Ok(Some(a)) = lib.analysis(id) {
            self.analyses.insert(id, a);
        }
        self.load_side_data(lib, id);
    }
}

struct PendingExport {
    out: std::path::PathBuf,
    tracks: Vec<TrackId>,
    total: usize,
    /// Stem quality the export waits for.
    quality: String,
}

pub struct App {
    pub lib: Library,
    pub sh: Shared,
    pub jobs: Jobs,
    pub view: View,
    pub library: LibraryView,
    pub status: String,
    pub devctl: crate::devctl::DevCtl,
    pub live: crate::live::view::LiveView,
    pub decks: Vec<crate::live::model::DeckUi>,
    pub mixer: crate::live::model::MixerUi,
    pub browser: LibraryView,
    pub studio: crate::studio::StudioView,
    pub mixes: Vec<(i64, String, i64)>,
    pub eng: crate::live::adapter::LiveEngine,
    pub settings: crate::settings::Settings,
    pub home: crate::home::HomeView,
    pub prefs: crate::prefs::Prefs,
    menu: Option<crate::menu::AppMenu>,
    ui_ctx_cmd: Option<egui::ViewportCommand>,
    applied_prefs: Option<crate::prefs::Prefs>,
    pub mix_summaries: Vec<crate::home::MixSummary>,
    prepared: (i64, usize),
    /// [`StudioView::mix_serial`] of the mix the studio preview was last reset for.
    preview_serial: u64,
    /// Parts of tracks queued for separation because stem automation plays in them (see
    /// [`region_key`]).
    stems_queued: std::collections::HashSet<(TrackId, Vec<(i64, i64)>)>,
    /// Tracks with stem automation queued for complete stems.
    full_stems_queued: std::collections::HashSet<TrackId>,
    /// Export waiting for stem separation.
    pending_export: Option<PendingExport>,
    /// Last seen change from `clubcraft mcp` (marker mtime) and when it was checked.
    external: (Option<std::time::SystemTime>, f64),
    queued: std::collections::HashSet<TrackId>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> anyhow::Result<App> {
        theme::apply(&cc.egui_ctx);
        let lib = Library::open_default()?;
        let mut sh = Shared { deck_count: 2, ..Default::default() };
        sh.reload(&lib);
        let prefs = crate::prefs::Prefs::load();
        let job_settings = prefs_job_settings(&prefs);
        let jobs = Jobs::new(job_settings);
        // Start on the home page (preference), otherwise where the user left off.
        let mut view = if prefs.start_on_home { View::Home } else { cc.storage.and_then(|s| eframe::get_value(s, "view")).unwrap_or(View::Home) };
        // Developer conveniences: `--import <path>` and `--view library|studio|live`.
        let args: Vec<String> = std::env::args().collect();
        for w in args.windows(2) {
            match w[0].as_str() {
                "--import" => jobs.import(vec![w[1].clone().into()], true),
                "--view" => {
                    view = match w[1].as_str() {
                        "home" => View::Home,
                        "studio" => View::Studio,
                        "live" => View::Live,
                        _ => View::Library,
                    }
                }
                _ => {}
            }
        }
        let audio_cfg: clubcraft_engine::AudioConfig = cc.storage.and_then(|s| eframe::get_value(s, "audio")).unwrap_or_default();
        let midi_port: Option<String> = cc.storage.and_then(|s| eframe::get_value(s, "midi_port"));
        let mut app = App { lib, sh, jobs, view, library: LibraryView::default(), status: String::new(), devctl: crate::devctl::DevCtl::from_env(), live: Default::default(), decks: vec![Default::default(); 2], mixer: Default::default(), browser: LibraryView::default(), studio: Default::default(), mixes: Vec::new(), eng: crate::live::adapter::LiveEngine::start(audio_cfg.clone()), settings: crate::settings::Settings::new(audio_cfg, midi_port), home: Default::default(), prefs, applied_prefs: None, menu: crate::menu::AppMenu::install(&cc.egui_ctx), ui_ctx_cmd: None, mix_summaries: Vec::new(), prepared: (-1, 0), preview_serial: 0, stems_queued: Default::default(), full_stems_queued: Default::default(), pending_export: None, queued: Default::default(), external: (std::fs::metadata(crate::mcp::change_marker()).and_then(|m| m.modified()).ok(), 0.0) };
        app.settings.connect_midi(app.eng.handle.as_ref());
        if let Some(e) = &app.eng.error {
            app.status = e.clone();
        }
        app.refresh_mixes();
        let last_mix = cc.storage.and_then(|s| eframe::get_value::<i64>(s, "mix"));
        if let Some(m) = last_mix.and_then(|id| app.lib.mix(id).ok().flatten()) {
            app.studio.set_mix(m);
        }
        Ok(app)
    }

    /// Move files dropped onto the studio into the mix, in drop order, as soon as each is in the
    /// library with an analysis. Once the jobs are idle, whatever is left joins as it is (a failed
    /// analysis still gives a playable track) and files that could not be imported are dropped.
    fn add_dropped_to_mix(&mut self) {
        if self.studio.pending_adds.is_empty() {
            return;
        }
        let id_of = |sh: &Shared, p: &std::path::Path| sh.tracks.iter().find(|t| t.path == p).map(|t| t.id);
        let idle = self.jobs.pending() == 0;
        let mut ready = Vec::new();
        let mut n = 0;
        for p in &self.studio.pending_adds {
            match id_of(&self.sh, p) {
                Some(id) if idle || self.sh.analyses.get(&id).is_some_and(|a| a.grid.is_some()) => ready.push(id),
                None if idle => {}
                _ => break,
            }
            n += 1;
        }
        self.studio.pending_adds.drain(..n);
        if !ready.is_empty() {
            self.studio.add_tracks(&self.sh, &ready);
            self.status = format!("Added {} track{} to the mix", ready.len(), if ready.len() == 1 { "" } else { "s" });
        }
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        let events = self.jobs.poll();
        let mut reload_all = false;
        for e in events {
            match e {
                Event::LibraryChanged => reload_all = true,
                Event::TrackUpdated(id) => {
                    self.sh.reload_track(&self.lib, id);
                    // the cached audio may predate the new stems: a mix would keep playing the
                    // stem-less copy and only imitate its stem curves
                    self.eng.cache.invalidate(id);
                    // a new grid moves the open mix's beats and tempo lane
                    if self.studio.mix.tracks.iter().any(|t| t.track_id == id) {
                        self.studio.timeline_dirty = true;
                    }
                }
                Event::Error { message, .. } => self.status = message,
                _ => {}
            }
        }
        // Changes made by `clubcraft mcp` (another process) arrive through a marker file.
        let now = ctx.input(|i| i.time);
        if now - self.external.1 > 1.0 {
            self.external.1 = now;
            let m = std::fs::metadata(crate::mcp::change_marker()).and_then(|m| m.modified()).ok();
            if m != self.external.0 {
                self.external.0 = m;
                reload_all = true;
                // Pick up edits to the open mix unless it has unsaved changes of its own.
                if !self.studio.dirty && self.studio.mix.id != 0 {
                    if let Ok(Some(mut mix)) = self.lib.mix(self.studio.mix.id) {
                        mix.normalize();
                        if mix != self.studio.mix {
                            self.studio.mix = mix;
                            self.studio.timeline_dirty = true;
                        }
                    }
                }
            }
        }
        if reload_all {
            self.sh.reload(&self.lib);
            self.refresh_mixes();
        }
        self.add_dropped_to_mix();
        self.sh.job_status = self.jobs.active.clone();
        for m in self.eng.tick(&self.lib, &self.sh, &self.jobs) {
            self.status = m;
        }
        for id in std::mem::take(&mut self.eng.touched) {
            self.sh.reload_track(&self.lib, id);
        }
        // another mix was opened: the running preview still plays the old one, so stop it (the
        // next play builds the preview from the open mix)
        if self.studio.mix_serial != self.preview_serial {
            self.preview_serial = self.studio.mix_serial;
            self.refresh_savepoints();
            self.studio.at_savepoint = self.matching_savepoint();
            if self.eng.studio_player.is_some() {
                self.eng.stop_mix(crate::live::adapter::STUDIO_DECKS);
                self.studio.playing = false;
            }
        }
        let key = (self.studio.mix.id, self.studio.mix.tracks.len());
        if key != self.prepared {
            self.prepared = key;
            self.prepare_mix_tracks();
        }
        // Stem automation (stem transitions, a track's own stem lanes) on tracks without stems is
        // only imitated with volume and EQ, so the parts it plays in are separated right away, at
        // the Fast quality (seconds per track; complete Best stems are made for export), whatever
        // the background setting. Mix order (front of the queue, so pushed in reverse); each part
        // once per session; not while the timeline is stale or being dragged.
        if !self.studio.timeline_dirty && !ctx.input(|i| i.pointer.any_down()) {
            let fresh: Vec<(TrackId, Vec<(f64, f64)>)> = self.studio.missing_stem_regions(&self.sh).into_iter().filter(|(id, r)| !self.stems_queued.contains(&region_key(*id, r))).collect();
            if !fresh.is_empty() {
                for (id, r) in fresh.iter().rev() {
                    self.jobs.separate_regions(*id, r.clone());
                }
                self.stems_queued.extend(fresh.iter().map(|(id, r)| region_key(*id, r)));
                self.status = format!("Separating stems for the stem transitions of {} track(s)", fresh.len());
            }
            // Then the whole tracks, also without "Separate in the background": stem automation
            // needs stems, and the stem rows, the library and the decks need complete ones.
            for id in self.studio.tracks_with_stem_automation() {
                if !self.sh.analyses.get(&id).is_some_and(|a| a.stems_model.is_some()) && self.full_stems_queued.insert(id) {
                    self.jobs.separate_later(id);
                }
            }
        }
        let st = self.eng.state();
        self.settings.tick(&st);
        if self.eng.preview_player.is_some() && st.mix.active {
            self.studio.preview_pos = st.mix.time;
        }
        self.decks = (0..2).map(|d| crate::live::adapter::deck_ui(&st, d, &self.sh, &Default::default(), self.eng.is_loading(d))).collect();
        self.mixer = crate::live::adapter::mixer_ui(&st, &self.eng);
        // studio transport state
        if self.eng.studio_player.is_some() && self.status.starts_with("Preparing") {
            self.status.clear();
        }
        if self.eng.studio_player.is_some() {
            self.studio.playing = st.mix.active && st.mix.playing;
            if st.mix.active {
                self.studio.position = st.mix.time;
            }
            if st.mix.finished {
                self.studio.playing = false;
            }
        } else {
            self.studio.playing = false;
        }
        self.studio.render_progress = self.eng.export.as_ref().map(|e| e.progress.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000.0);
        if self.studio.edited {
            self.studio.edited = false;
            if self.eng.studio_player.is_some() {
                self.studio.refresh_timeline(&self.sh);
                self.eng.studio_update(&self.studio.mix, &self.sh);
            }
        }
        // Redraw only while something moves, at most 60 fps (ProMotion displays would do 120).
        let anything_playing = st.decks.iter().any(|d| d.playing) || st.mix.playing || self.eng.export.is_some() || self.eng.loading_mix.is_some() || self.pending_export.is_some() || self.studio.extracting > 0;
        if anything_playing {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        } else if self.view == View::Live {
            ctx.request_repaint_after(std::time::Duration::from_millis(250)); // meters settle, clock
        }
        self.tick_pending_export();
        if self.jobs.pending() > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn handle_library_actions(&mut self, actions: Vec<LibraryAction>) {
        for a in actions {
            match a {
                LibraryAction::Process(ids) => {
                    for id in ids {
                        self.jobs.process_with_stems(id, true);
                    }
                }
                LibraryAction::Reanalyze(ids) => {
                    for id in ids {
                        self.jobs.analyze_only(id);
                    }
                }
                LibraryAction::Remove(ids) => {
                    for id in ids {
                        let _ = self.lib.remove_track(id);
                    }
                    self.sh.reload(&self.lib);
                }
                LibraryAction::FindMashups(id) => {
                    self.studio.mashup.open_for(Some(id), &self.sh);
                    self.view = View::Studio;
                }
                LibraryAction::Reveal(id) => {
                    if let Some(t) = self.sh.track(id) {
                        crate::platform::reveal(&t.path);
                    }
                }
                LibraryAction::AddToMix(ids) => {
                    self.studio.add_tracks(&self.sh, &ids);
                    self.view = View::Studio;
                }
                LibraryAction::LoadDeck(id, deck) => self.load_to_deck(id, deck),
            }
        }
    }

    fn menu_command(&mut self, c: crate::menu::MenuCmd) {
        use crate::home::HomeAction as H;
        use crate::menu::MenuCmd as M;
        use crate::studio::StudioAction as S;
        match c {
            M::Settings => self.settings.open = true,
            M::MashupFinder => {
                let id = self.studio.mix.tracks.first().map(|t| t.track_id);
                self.studio.mashup.open_for(id, &self.sh);
                self.view = View::Studio;
            }
            M::Undo => self.studio.undo(),
            M::Redo => self.studio.redo(),
            M::NewMix => self.handle_home_actions(vec![H::NewMix { name: "New mix".into(), tracks: vec![], harmonize: false }]),
            M::AddMusic => self.handle_home_actions(vec![H::AddMusic]),
            M::ImportDjStudio => self.handle_studio_actions(vec![S::ImportDjStudio]),
            M::ExportVirtualDj => self.handle_studio_actions(vec![S::ExportVirtualDj]),
            M::ExportMix => {
                self.view = View::Studio;
                self.studio_transport(S::Export);
            }
            M::SaveMix => self.save_mix(),
            M::ViewHome => self.view = View::Home,
            M::ViewLibrary => self.view = View::Library,
            M::ViewStudio => self.view = View::Studio,
            M::ViewLive => self.view = View::Live,
            M::PlayPause => self.studio_transport(S::PlayPause),
            M::OpenDataFolder => crate::platform::open(&clubcraft_core::paths::data_dir()),
            M::Help => crate::platform::open(std::path::Path::new("https://github.com/beneaze/ClubCraft#readme")),
        }
    }

    /// Push changed preferences into the engine, the job system and the views, and save them.
    fn apply_prefs(&mut self, ctx: &egui::Context) {
        if self.applied_prefs.as_ref() == Some(&self.prefs) {
            return;
        }
        let p = self.prefs.clone();
        let first = self.applied_prefs.is_none();
        crate::theme::set_key_notation(p.key_notation);
        crate::theme::set_soft_stems(p.soft_stem_colors);
        if first || self.applied_prefs.as_ref().map(|o| o.accent) != Some(p.accent) {
            crate::theme::set_accent(p.accent.rgb());
            crate::theme::apply(ctx);
        }
        ctx.set_zoom_factor(p.ui_scale);
        if let Some(h) = &self.eng.handle {
            for c in p.engine_commands() {
                let _ = h.send(c);
            }
        }
        *self.jobs.settings.lock() = prefs_job_settings(&p);
        self.live.wave_style = if p.stem_waveforms { crate::waveform::WaveStyle::Stems } else { crate::waveform::WaveStyle::Bands };
        if first || self.applied_prefs.as_ref().map(|o| o.wave_zoom_secs) != Some(p.wave_zoom_secs) {
            self.live.zoom = p.wave_zoom_secs;
        }
        self.studio.plan_opts = clubcraft_core::planner::PlanOptions {
            transition_beats: p.transition_bars.map(|b| b * 4),
            style: Some(p.transition_style),
            allow_key_shift: p.allow_key_shift,
            has_stems: true,
        };
        self.eng.prefs = p.clone();
        *self.eng.sep.quality.lock() = p.stem_quality.clone();
        if !first {
            p.save();
        }
        self.applied_prefs = Some(p);
    }

    /// Make sure every track of the open mix gets analysed and separated (high priority).
    pub fn prepare_mix_tracks(&mut self) {
        for t in &self.studio.mix.tracks {
            let a = self.sh.analyses.get(&t.track_id);
            let needs = a.is_none_or(|a| a.version < clubcraft_core::Analysis::CURRENT_VERSION || a.stems_model.is_none());
            if needs && self.queued.insert(t.track_id) {
                self.jobs.process(t.track_id, true);
            }
        }
    }

    pub fn refresh_mixes(&mut self) {
        self.mixes = self.lib.mixes().unwrap_or_default();
        self.sh.mix_tracks = self.mixes.iter().filter_map(|(id, name, _)| self.lib.mix(*id).ok().flatten().map(|m| (*id, name.clone(), m.tracks.iter().map(|t| t.track_id).collect()))).collect();
        self.mix_summaries = self
            .mixes
            .iter()
            .filter_map(|(id, _, modified)| self.lib.mix(*id).ok().flatten().map(|m| crate::home::MixSummary::from_mix(&m, *modified, &self.sh)))
            .collect();
    }

    fn handle_home_actions(&mut self, actions: Vec<crate::home::HomeAction>) {
        use crate::home::HomeAction as H;
        for a in actions {
            match a {
                H::NewMix { name, tracks, harmonize } => {
                    self.save_mix();
                    let order = if harmonize && tracks.len() > 2 {
                        let items: Vec<_> = tracks.iter().map(|id| clubcraft_core::planner::OrderItem::from_analysis(*id, self.sh.analyses.get(id))).collect();
                        clubcraft_core::planner::order_tracks(&items, &Default::default())
                    } else {
                        tracks
                    };
                    let mut mix = clubcraft_core::planner::plan_mix(&name, &order, &|id| self.sh.analyses.get(&id).cloned(), &self.studio.plan_opts);
                    mix.keylock = self.prefs.mix_keylock;
                    mix.name = name;
                    if let Err(e) = self.lib.save_mix(&mut mix) {
                        self.status = format!("create mix: {e}");
                    }
                    self.studio.set_mix(mix);
                    self.refresh_mixes();
                    self.view = View::Studio;
                }
                H::OpenMix(id) => {
                    self.save_mix();
                    if let Ok(Some(m)) = self.lib.mix(id) {
                        self.studio.set_mix(m);
                        self.view = View::Studio;
                    }
                }
                H::DeleteMix(id) => {
                    let _ = self.lib.delete_mix(id);
                    if self.studio.mix.id == id {
                        self.studio.set_mix(clubcraft_core::Mix::default());
                    }
                    self.refresh_mixes();
                }
                H::AddMusic => {
                    if let Some(folders) = rfd::FileDialog::new().set_title("Add music folder").pick_folders() {
                        self.jobs.import(folders, true);
                    }
                }
                H::ImportDjStudio => self.handle_studio_actions(vec![crate::studio::StudioAction::ImportDjStudio]),
                H::GoLibrary => self.view = View::Library,
                H::GoLive => self.view = View::Live,
            }
        }
    }

    pub fn save_mix(&mut self) {
        if self.studio.dirty && !self.studio.mix.tracks.is_empty() {
            match self.lib.save_mix(&mut self.studio.mix) {
                Ok(_) => self.studio.dirty = false,
                Err(e) => self.status = format!("save mix: {e}"),
            }
            self.refresh_mixes();
        }
    }

    /// Save the open mix even when nothing changed, so it has a library id. False (with a status
    /// message) when it can't be saved.
    fn save_mix_now(&mut self) -> bool {
        if self.studio.mix.tracks.is_empty() {
            return false;
        }
        if self.studio.mix.id <= 0 {
            self.studio.dirty = true;
        }
        self.save_mix();
        self.studio.mix.id > 0 && !self.studio.dirty
    }

    /// Keep the open mix's current state as savepoint `name`; returns its id.
    fn add_savepoint(&mut self, name: &str) -> Option<i64> {
        if !self.save_mix_now() {
            return None;
        }
        match self.lib.add_savepoint(self.studio.mix.id, name, &self.studio.mix) {
            Ok(id) => {
                self.studio.at_savepoint = Some(id);
                self.refresh_savepoints();
                self.status = format!("Savepoint \"{name}\" kept");
                Some(id)
            }
            Err(e) => {
                self.status = format!("savepoint: {e}");
                None
            }
        }
    }

    fn refresh_savepoints(&mut self) {
        self.studio.savepoints = if self.studio.mix.id > 0 { self.lib.savepoints(self.studio.mix.id).unwrap_or_default() } else { Vec::new() };
    }

    /// The newest savepoint the open mix equals (its name aside), if any.
    fn matching_savepoint(&self) -> Option<i64> {
        let mut cur = self.studio.mix.clone();
        cur.normalize();
        self.studio.savepoints.iter().rev().map(|s| s.0).find(|&id| {
            self.lib.savepoint(id).ok().flatten().is_some_and(|mut m| {
                m.name = cur.name.clone();
                m.normalize();
                m == cur
            })
        })
    }

    /// Start a waiting export once its stems are separated (or the jobs ended / were stopped).
    fn tick_pending_export(&mut self) {
        let Some(pe) = &self.pending_export else { return };
        let missing = pe.tracks.iter().filter(|id| !crate::pipeline::stems_meet(self.sh.analyses.get(id).and_then(|a| a.stems_model.as_deref()), &pe.quality)).count();
        self.studio.render_progress = Some((pe.total - missing) as f32 / pe.total.max(1) as f32);
        self.studio.render_label = format!("Stems {}/{}", pe.total - missing, pe.total);
        if missing == 0 || self.jobs.pending() == 0 {
            let Some(pe) = self.pending_export.take() else { return };
            self.studio.render_label.clear();
            self.eng.export_mix(&self.studio.mix, pe.out, &self.sh);
            self.status = if missing == 0 { "Stems ready, rendering mix…".into() } else { format!("Rendering mix ({missing} track(s) without stems imitate them with volume and EQ)…") };
        }
    }

    fn handle_mashup_actions(&mut self, actions: Vec<crate::mashup_view::MashupAction>) {
        use crate::mashup_view::MashupAction as M;
        let short = |sh: &Shared, id: TrackId| sh.track(id).map(|t| if t.title.is_empty() { t.display_name() } else { t.title.clone() }).unwrap_or_default();
        for a in actions {
            match a {
                M::Preview(m) => {
                    if let Some(mix) = crate::mashup::mashup_mix("Preview", &m, &self.sh.analyses, 4) {
                        let tl = clubcraft_core::MixTimeline::build(&mix, &|id| self.sh.analyses.get(&id).cloned());
                        let bar = self.sh.analyses.get(&m.vocal_track).and_then(|a| a.grid).map(|g| g.bar_len()).unwrap_or(2.0);
                        let from = tl.transitions.first().map(|t| (t.start_sec - 2.0 * bar).max(0.0)).unwrap_or(0.0);
                        self.eng.play_preview(&mix, from, &self.sh);
                        self.status = format!("Preview: vocals of {} over {}", short(&self.sh, m.vocal_track), short(&self.sh, m.inst_track));
                    }
                }
                M::StopPreview => self.eng.stop_preview(),
                M::NewMix(m) => {
                    self.eng.stop_preview();
                    self.save_mix();
                    let name = format!("{} × {}", short(&self.sh, m.vocal_track), short(&self.sh, m.inst_track));
                    if let Some(mut mix) = crate::mashup::mashup_mix(&name, &m, &self.sh.analyses, 16) {
                        mix.keylock = self.prefs.mix_keylock;
                        if let Err(e) = self.lib.save_mix(&mut mix) {
                            self.status = format!("save mix: {e}");
                        }
                        self.status = format!("Created the mix '{}'", mix.name);
                        self.studio.set_mix(mix);
                        self.studio.selection = crate::studio::Selection::Transition(0);
                        self.refresh_mixes();
                        self.view = View::Studio;
                        self.studio.mashup.open = false;
                    }
                }
                M::Insert(m) => {
                    if let Some(k) = self.studio.mix.tracks.iter().position(|t| t.track_id == m.vocal_track) {
                        if crate::mashup::insert_after(&mut self.studio.mix, k, &m, &self.sh.analyses) {
                            self.studio.changed();
                            self.studio.selection = crate::studio::Selection::Transition(k);
                            self.view = View::Studio;
                            self.studio.mashup.open = false;
                            self.eng.stop_preview();
                            self.status = "Mashup inserted into the mix".into();
                        }
                    }
                }
                M::Apply(m, k) => {
                    if crate::mashup::apply_to_transition(&mut self.studio.mix, k, &m, &self.sh.analyses) {
                        self.studio.changed();
                        self.studio.selection = crate::studio::Selection::Transition(k);
                        self.status = "Transition set to the mashup".into();
                        self.studio.mashup.open = false;
                        self.eng.stop_preview();
                    }
                }
            }
        }
    }

    fn handle_studio_actions(&mut self, actions: Vec<crate::studio::StudioAction>) {
        use crate::studio::StudioAction as A;
        for a in actions {
            match a {
                A::SaveMix => self.save_mix(),
                A::PreviewTrack(id, from) => {
                    // a one-track mix from the start of the file, so mix time is track time
                    let dur = self.sh.analyses.get(&id).map(|a| a.duration).filter(|d| *d > 0.0).or_else(|| self.sh.track(id).map(|t| t.duration)).unwrap_or(0.0);
                    let mut mix = clubcraft_core::Mix { name: "Preview".into(), tempo: clubcraft_core::TempoMode::Glide, ..Default::default() };
                    mix.tracks.push(clubcraft_core::MixTrack { track_id: id, mix_in: 0.0, mix_out: dur.max(1.0), key_shift: 0, gain_db: 0.0, stems: Vec::new() });
                    mix.normalize();
                    self.studio.playing = false;
                    self.eng.play_preview(&mix, from.clamp(0.0, dur.max(0.0)), &self.sh);
                    self.status = format!("Preview: {}", self.sh.track(id).map(|t| t.display_name()).unwrap_or_default());
                }
                A::StopPreview => {
                    self.eng.stop_preview();
                    self.status.clear();
                }
                A::DuplicateMix => {
                    if !self.save_mix_now() {
                        continue;
                    }
                    let base = format!("{} (copy)", self.studio.mix.name);
                    let mut name = base.clone();
                    let mut n = 2;
                    while self.mixes.iter().any(|m| m.1 == name) {
                        name = format!("{base} {n}");
                        n += 1;
                    }
                    match self.lib.duplicate_mix(self.studio.mix.id, &name) {
                        Ok(Some(copy)) => {
                            self.studio.set_mix(copy);
                            self.refresh_mixes();
                            self.status = format!("Duplicated as \"{name}\"");
                        }
                        Ok(None) => {}
                        Err(e) => self.status = format!("duplicate mix: {e}"),
                    }
                }
                A::AddSavepoint(name) => {
                    let name = if name.trim().is_empty() { format!("Savepoint {}", self.studio.savepoints.len() + 1) } else { name.trim().to_string() };
                    self.add_savepoint(&name);
                }
                A::RestoreSavepoint(id) => {
                    let Some(target) = self.studio.savepoints.iter().find(|s| s.0 == id).map(|s| s.1.clone()) else { continue };
                    // the state being left is kept, so going back and forth loses nothing
                    if self.studio.at_savepoint.is_none() && self.matching_savepoint().is_none() && !self.studio.mix.tracks.is_empty() && self.add_savepoint(&format!("Before going to \"{target}\"")).is_none() {
                        continue;
                    }
                    match self.lib.savepoint(id) {
                        Ok(Some(mut m)) => {
                            m.name = self.studio.mix.name.clone();
                            self.studio.go_to_savepoint(m);
                            self.save_mix();
                            self.studio.at_savepoint = Some(id);
                            self.status = format!("Went to savepoint \"{target}\"");
                        }
                        Ok(None) => self.refresh_savepoints(),
                        Err(e) => self.status = format!("savepoint: {e}"),
                    }
                }
                A::DeleteSavepoint(id) => {
                    if let Err(e) = self.lib.delete_savepoint(id) {
                        self.status = format!("delete savepoint: {e}");
                    }
                    if self.studio.at_savepoint == Some(id) {
                        self.studio.at_savepoint = None;
                    }
                    self.refresh_savepoints();
                }
                A::FindMashups(id) => self.studio.mashup.open_for(id, &self.sh),
                A::Harmonize => self.studio.harmonize(&self.sh),
                A::SeparateStems(ids) => {
                    // the parts the mix's stem automation plays in (Fast); whole tracks otherwise
                    let regions = self.studio.missing_stem_regions(&self.sh);
                    for id in ids.iter().rev() {
                        match regions.iter().find(|e| e.0 == *id) {
                            Some((_, r)) => self.jobs.separate_regions(*id, r.clone()),
                            None => self.jobs.separate_now(*id),
                        }
                    }
                    self.status = format!("Separating stems for {} track(s)", ids.len());
                }
                A::AutoMixPoints => {
                    let order: Vec<TrackId> = self.studio.mix.tracks.iter().map(|t| t.track_id).collect();
                    self.studio.replan(&self.sh, &order);
                }
                A::FindVocalOver(k) => {
                    let mix = self.studio.mix.clone();
                    self.studio.mashup.open_for_transition(&mix, k, &self.sh);
                }
                A::ImportDjStudio => {
                    let tx = self.jobs.sender();
                    std::thread::spawn(move || {
                        let r = clubcraft_core::Library::open_default().and_then(|lib| crate::interop::djstudio::import_all(&lib, &crate::interop::djstudio::default_root()));
                        let msg = match r {
                            Ok(r) => format!("DJ.Studio: imported {} tracks, {} cues, {} mixes ({} files not found)", r.tracks_imported, r.cues_imported, r.mixes.len(), r.missing_files.len()),
                            Err(e) => format!("DJ.Studio import failed: {e:#}"),
                        };
                        let _ = tx.send(crate::jobs::Event::Error { track: None, message: msg });
                        let _ = tx.send(crate::jobs::Event::LibraryChanged);
                    });
                    self.status = "Importing from DJ.Studio…".into();
                }
                A::ExportVirtualDj => self.export_virtualdj(),
                other => self.studio_transport(other),
            }
        }
    }

    fn export_virtualdj(&mut self) {
        let Some(path) = rfd::FileDialog::new().set_title("VirtualDJ database.xml (VirtualDJ must be closed)").set_file_name("database.xml").save_file() else { return };
        let songs: Vec<crate::interop::virtualdj::VdjSong> = self
            .sh
            .tracks
            .iter()
            .filter(|t| self.sh.analyses.get(&t.id).is_some_and(|a| a.version > 0))
            .map(|t| crate::interop::virtualdj::VdjSong { track: t, analysis: self.sh.analyses.get(&t.id), cues: self.sh.effective_cues(t.id) })
            .collect();
        self.status = match crate::interop::virtualdj::export(&path, &songs) {
            Ok((u, a)) => format!("VirtualDJ: updated {u}, added {a} songs in {}", path.display()),
            Err(e) => format!("VirtualDJ export failed: {e:#}"),
        };
    }

    fn studio_transport(&mut self, a: crate::studio::StudioAction) {
        use crate::live::adapter::{LIVE_DECKS, STUDIO_DECKS};
        use crate::studio::StudioAction as A;
        match a {
            A::PlayPause => {
                if !self.studio.playing {
                    self.studio.follow = true;
                }
                if self.eng.studio_player.is_none() {
                    if self.eng.loading_mix.is_none() && !self.studio.mix.tracks.is_empty() {
                        self.eng.stop_mix(LIVE_DECKS);
                        self.status = "Preparing mix preview…".into();
                        self.eng.play_mix(&self.studio.mix, STUDIO_DECKS, self.studio.position, &self.sh);
                    }
                } else {
                    self.eng.studio_pause(self.studio.playing);
                }
            }
            A::Seek(t) => {
                self.studio.position = t;
                self.eng.studio_seek(t);
            }
            A::Export => {
                let dir = clubcraft_core::paths::exports_dir();
                let _ = std::fs::create_dir_all(&dir);
                let name: String = self.studio.mix.name.chars().map(|c| if c == '/' || c == ':' { '-' } else { c }).collect();
                if let Some(out) = rfd::FileDialog::new().set_directory(&dir).set_file_name(format!("{name}.wav")).add_filter("WAV", &["wav"]).save_file() {
                    // the heavy part happens here: stems for the tracks whose stem automation lacks them
                    let quality = self.prefs.export_stem_quality.clone();
                    let need = self.studio.tracks_needing_stems(&self.sh, Some(&quality));
                    if need.is_empty() {
                        self.eng.export_mix(&self.studio.mix, out, &self.sh);
                        self.status = "Rendering mix…".into();
                    } else {
                        for id in &need {
                            self.jobs.separate_at(*id, &quality);
                        }
                        self.status = format!("Separating stems ({quality}) for {} track(s), then rendering…", need.len());
                        self.pending_export = Some(PendingExport { out, total: need.len(), tracks: need, quality });
                    }
                }
            }
            A::SendToLive => {
                self.eng.stop_mix(STUDIO_DECKS);
                self.eng.play_mix(&self.studio.mix, LIVE_DECKS, 0.0, &self.sh);
                self.view = View::Live;
                self.status = format!("Playing '{}' live with automix", self.studio.mix.name);
            }
            _ => {}
        }
    }

    fn handle_live_actions(&mut self, actions: Vec<crate::live::model::LiveAction>) {
        for a in actions {
            self.eng.apply(a, &self.decks, &self.sh);
        }
    }

    /// VirtualDJ-style keyboard shortcuts for the live screen.
    fn keyboard(&mut self, ctx: &egui::Context) {
        use crate::live::model::LiveAction as L;
        use egui::Key;
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let mut acts = Vec::new();
        ctx.input(|i| {
            let shift = i.modifiers.shift;
            let pressed = |k: Key| i.key_pressed(k);
            // deck A: Z cue, X play, C sync · deck B: M cue, Comma play, Period sync
            for (d, cue, play, sync) in [(0usize, Key::Z, Key::X, Key::C), (1, Key::M, Key::Comma, Key::Period)] {
                if pressed(cue) {
                    acts.push(L::CueDown(d));
                }
                if i.key_released(cue) {
                    acts.push(L::CueUp(d));
                }
                if pressed(play) {
                    acts.push(L::PlayPause(d));
                }
                if pressed(sync) {
                    acts.push(L::Sync(d));
                }
            }
            // hot cues: 1-8 deck A, shift + 1-8 deck B
            let nums = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8];
            for (n, k) in nums.iter().enumerate() {
                let d = if shift { 1 } else { 0 };
                if pressed(*k) {
                    acts.push(L::HotCue(d, n as u8));
                }
                if i.key_released(*k) {
                    acts.push(L::HotCueRelease(d, n as u8));
                }
            }
            if pressed(Key::ArrowLeft) {
                acts.push(L::Crossfader((self.mixer.crossfader - 0.1).max(-1.0)));
            }
            if pressed(Key::ArrowRight) {
                acts.push(L::Crossfader((self.mixer.crossfader + 0.1).min(1.0)));
            }
            if pressed(Key::Space) {
                acts.push(L::Crossfader(if self.mixer.crossfader < 0.0 { 1.0 } else { -1.0 }));
            }
        });
        self.handle_live_actions(acts);
    }

    /// Pick a deck for "load": the requested one, else an idle deck (not playing), A first.
    fn load_to_deck(&mut self, id: TrackId, deck: usize) {
        let st = self.eng.state();
        let d = if deck < 2 { deck } else { (0..2).find(|d| !st.decks[*d].loaded).or_else(|| (0..2).find(|d| !st.decks[*d].playing)).unwrap_or(0) };
        self.eng.load(d, id);
    }

    fn dev_command(&mut self, cmd: &str) {
        log::info!("devctl: {cmd}");
        let (c, arg) = cmd.split_once(' ').unwrap_or((cmd, ""));
        match c {
            "djs-import" => self.handle_studio_actions(vec![crate::studio::StudioAction::ImportDjStudio]),
            "mix" => {
                // an exact name first, else the first name containing `arg`
                let q = arg.trim().to_lowercase();
                if let Some((id, _, _)) = self.mixes.iter().find(|m| m.1.to_lowercase() == q).or_else(|| self.mixes.iter().find(|m| m.1.to_lowercase().contains(&q))) {
                    if let Ok(Some(m)) = self.lib.mix(*id) {
                        self.studio.set_mix(m);
                        self.view = View::Studio;
                    }
                }
            }
            "select" => {
                self.studio.selection = match arg.split_once(' ') {
                    Some(("track", n)) => crate::studio::Selection::Track(n.parse().unwrap_or(0)),
                    Some(("transition", n)) => crate::studio::Selection::Transition(n.parse().unwrap_or(0)),
                    Some(("sample", n)) => crate::studio::Selection::Sample(n.parse().unwrap_or(0)),
                    _ => crate::studio::Selection::None,
                }
            }
            "mute" => self.handle_live_actions(vec![crate::live::model::LiveAction::MasterVol(0.0)]),
            "add-tracks" => {
                self.view = View::Studio;
                self.studio.picker.show_with(arg.trim());
            }
            "picker-preview" => {
                // `picker-preview <track query>`: listen to that track in the open dialog
                let q = arg.to_lowercase();
                if let Some(t) = self.sh.tracks.iter().find(|t| t.display_name().to_lowercase().contains(&q)) {
                    let (id, a) = (t.id, self.sh.analyses.get(&t.id));
                    let from = crate::track_picker::preview_start(a, a.map(|a| a.duration).unwrap_or(t.duration));
                    self.studio.picker.set_previewing(id);
                    self.handle_studio_actions(vec![crate::studio::StudioAction::PreviewTrack(id, from)]);
                }
            }
            "mashup" => {
                // `mashup <track query>`: open the finder for that track's vocals
                let q = arg.to_lowercase();
                let id = self.sh.tracks.iter().find(|t| t.display_name().to_lowercase().contains(&q)).map(|t| t.id);
                self.studio.mashup.open_for(id, &self.sh);
                self.view = View::Studio;
            }
            "mashup-preview" | "mashup-newmix" | "mashup-use" => {
                // act on result `arg` of the finder (tests)
                use crate::mashup_view::MashupAction as M;
                if let Some(m) = self.studio.mashup.result(arg.trim().parse().unwrap_or(0)) {
                    let a = match c {
                        "mashup-preview" => M::Preview(m),
                        "mashup-newmix" => M::NewMix(m),
                        _ => M::Apply(m, self.studio.mashup.transition_index().unwrap_or(0)),
                    };
                    self.handle_mashup_actions(vec![a]);
                }
            }
            "sample" => {
                // `sample <lane> <beat> <path>` (tests)
                let mut it = arg.splitn(3, ' ');
                let (l, b, path) = (it.next().and_then(|v| v.parse().ok()).unwrap_or(0u8), it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0), it.next().unwrap_or(""));
                self.studio.mix.samples.push(clubcraft_core::SampleClip { lane: l, path: path.into(), start_beat: b, gain_db: 0.0 });
                self.studio.changed();
                self.save_mix();
            }
            "range" => {
                // `range <track> <beat0> <beat1>` (mix beats, tests)
                let v: Vec<f64> = arg.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                if v.len() == 3 {
                    self.studio.range = Some(crate::studio_timeline::RangeSel { track: v[0] as usize, b0: v[1], b1: v[2] });
                }
            }
            "lib-mix" => {
                // `lib-mix <name>`: filter the library to a mix's tracks (tests)
                let q = arg.trim().to_lowercase();
                let m = self.sh.mix_tracks.iter().find(|m| m.1.to_lowercase().contains(&q)).map(|m| m.0);
                for v in [&mut self.library, &mut self.browser] {
                    v.mix = m;
                    v.mix_ordered = m.is_some();
                }
            }
            "stem-range" => {
                // `stem-range <stem 0-3> <gain>`: set a stem in the selected range (tests)
                let v: Vec<f64> = arg.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                if let (Some(rs), [st, g]) = (self.studio.range, v.as_slice()) {
                    let (t0, t1) = (self.studio.timeline.track_pos(rs.track, rs.b0), self.studio.timeline.track_pos(rs.track, rs.b1));
                    if let Some(t) = self.studio.mix.tracks.get_mut(rs.track) {
                        t.set_stem_range(crate::studio_timeline::STEM_ROWS[(*st as usize).min(3)], t0, t1, *g as f32);
                        self.studio.changed();
                    }
                }
            }
            "extract" => {
                let part = crate::extract::Part::ALL[arg.trim().parse::<usize>().unwrap_or(0).min(6)];
                self.studio.extract_requests.push((part, false));
            }
            "lock" => {
                if let Some(t) = self.studio.mix.transitions.get_mut(arg.trim().parse().unwrap_or(0)) {
                    t.locked = !t.locked;
                    self.studio.changed();
                }
            }
            "undo" => self.studio.undo(),
            "redo" => self.studio.redo(),
            "harmonize" => self.studio.harmonize(&self.sh),
            "duplicate" => self.handle_studio_actions(vec![crate::studio::StudioAction::DuplicateMix]),
            "savepoint" => self.handle_studio_actions(vec![crate::studio::StudioAction::AddSavepoint(arg.trim().to_string())]),
            "goto-savepoint" => {
                // `goto-savepoint <name>` (tests)
                if let Some(id) = self.studio.savepoints.iter().find(|s| s.1 == arg.trim()).map(|s| s.0) {
                    self.handle_studio_actions(vec![crate::studio::StudioAction::RestoreSavepoint(id)]);
                }
            }
            "tcat" => {
                // `tcat <stems|equalizer|effects|curves>`: transition editor category (tests)
                use crate::studio_transition::TCategory;
                if let Some(c) = TCategory::ALL.into_iter().find(|c| format!("{c:?}").eq_ignore_ascii_case(arg.trim())) {
                    self.studio.tcat = c;
                }
            }
            "tab" => {
                use crate::studio::Tab;
                if let Some(t) = Tab::ALL.into_iter().find(|t| format!("{t:?}").eq_ignore_ascii_case(arg.trim())) {
                    self.studio.toggle_tab(t);
                }
            }
            "engine-state" => {
                let st = self.eng.state();
                log::info!("engine: mix active {} playing {} time {:.2} duration {:.2} (studio {:.2}) schedule {} | decks {:?}", st.mix.active, st.mix.playing, st.mix.time, st.mix.duration, self.studio.timeline.total_secs, st.mix.schedule_id, st.decks.iter().map(|d| (d.loaded, d.playing, (d.position * 10.0).round() / 10.0, d.track_id)).collect::<Vec<_>>());
                log::info!("audio: buffer {} frames, load {:.2} (peak {:.2}, last 2 s {:.2}), overruns {}, late callbacks {}, stream errors {}, page faults in {} callbacks ({} overran)", st.audio.buffer_frames, st.cpu_load, st.audio.peak_load, st.audio.recent_peak, st.audio.overruns, st.audio.late, st.stream_errors, st.audio.faulted, st.audio.faulted_overruns);
                log::info!("audio peaks µs: commands {} conductor {} decks {:?} mixer {} publish {}", st.audio.section_peak_us[0], st.audio.section_peak_us[1], &st.audio.section_peak_us[2..6], st.audio.section_peak_us[6], st.audio.section_peak_us[7]);
                log::info!("track memory locked in RAM: {} MB", clubcraft_engine::pin::PINNED_BYTES.load(std::sync::atomic::Ordering::Relaxed) / (1 << 20));
                log::info!("deck peaks µs: {:?}", clubcraft_engine::prof::NAMES.iter().zip(st.audio.deck_peak_us).collect::<Vec<_>>());
            }
            "mashup-transition" => {
                let k: usize = arg.trim().parse().unwrap_or(0);
                let mix = self.studio.mix.clone();
                self.studio.mashup.open_for_transition(&mix, k, &self.sh);
            }
            "view" => {
                self.view = match arg.trim() {
                    "library" => View::Library,
                    "studio" => View::Studio,
                    "live" => View::Live,
                    _ => View::Home,
                };
            }
            "load" => {
                let mut it = arg.split_whitespace();
                let deck: usize = it.next().and_then(|d| d.parse().ok()).unwrap_or(0);
                let q = it.collect::<Vec<_>>().join(" ").to_lowercase();
                if let Some(t) = self.sh.tracks.iter().find(|t| t.display_name().to_lowercase().contains(&q)) {
                    let id = t.id;
                    self.eng.load(deck, id);
                }
            }
            "live" => {
                // raw live action for tests, e.g. `live play 0`
                let mut it = arg.split_whitespace();
                let (a, d) = (it.next().unwrap_or(""), it.next().and_then(|d| d.parse().ok()).unwrap_or(0));
                use crate::live::model::LiveAction as L;
                let act = match a {
                    "play" => Some(L::PlayPause(d)),
                    "sync" => Some(L::Sync(d)),
                    "seek" => it.next().and_then(|t| t.parse().ok()).map(|t| L::Seek(d, t)),
                    "fader" => it.next().and_then(|t| t.parse().ok()).map(|t| L::Fader(d, t)),
                    "acapella" => Some(L::StemPreset(d, crate::live::model::StemPreset::Acapella)),
                    "instrumental" => Some(L::StemPreset(d, crate::live::model::StemPreset::Instrumental)),
                    "master" => it.next().and_then(|t| t.parse().ok()).map(L::MasterVol),
                    _ => None,
                };
                if let Some(act) = act {
                    self.handle_live_actions(vec![act]);
                }
            }
            "settings" => {
                self.settings.open = true;
                match arg.trim() {
                    "decks" => self.settings.tab = crate::settings::Tab::Decks,
                    "mixer" => self.settings.tab = crate::settings::Tab::Mixer,
                    "analysis" => self.settings.tab = crate::settings::Tab::Analysis,
                    "studio" => self.settings.tab = crate::settings::Tab::Studio,
                    _ => {}
                }
            }
            "settings-close" => self.settings.open = false,
            "search" => {
                self.browser.search = arg.trim().to_string();
                self.library.search = arg.trim().to_string();
            }
            "sort" => {
                use crate::library_view::SortBy;
                let mut it = arg.split_whitespace();
                let by = match it.next().unwrap_or("") {
                    "bpm" => SortBy::Bpm,
                    "key" => SortBy::Key,
                    "energy" => SortBy::Energy,
                    "title" => SortBy::Title,
                    _ => SortBy::Artist,
                };
                let asc = it.next() != Some("desc");
                for v in [&mut self.browser, &mut self.library] {
                    v.sort = by;
                    v.ascending = asc;
                }
            }
            "size" => {
                let v: Vec<f32> = arg.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                if v.len() == 2 {
                    self.ui_ctx_cmd = Some(egui::ViewportCommand::InnerSize(egui::vec2(v[0], v[1])));
                }
            }
            "zoom" => {
                if let Ok(z) = arg.trim().parse::<f32>() {
                    self.studio.zoom = z;
                }
            }
            "lane-show" => {
                // `lane-show <lane> <mode>`, mode as the dropdown's label, e.g. `lane-show 0 EQ & filter`
                use crate::studio_timeline::ShowMode;
                if let Some((l, m)) = arg.trim().split_once(' ') {
                    if let (Ok(l), Some(m)) = (l.parse::<usize>(), ShowMode::ALL.into_iter().find(|s| s.label().eq_ignore_ascii_case(m.trim()))) {
                        if let Some(s) = self.studio.lane_show.get_mut(l) {
                            *s = m;
                        }
                    }
                }
            }
            "scroll" => {
                if let Ok(x) = arg.trim().parse::<f32>() {
                    self.studio.scroll = x;
                    self.studio.follow = false;
                }
            }
            "collapse" => {
                if let Ok(l) = arg.trim().parse::<usize>() {
                    if l < self.studio.lane_collapsed.len() {
                        self.studio.lane_collapsed[l] = !self.studio.lane_collapsed[l];
                    }
                }
            }
            "send-live" => self.studio_transport(crate::studio::StudioAction::SendToLive),
            "mix-seek" => {
                if let Ok(t) = arg.trim().parse::<f64>() {
                    if self.eng.live_player.is_some() {
                        self.eng.live_mix_seek(t);
                    } else {
                        self.eng.studio_seek(t);
                    }
                }
            }
            "studio-play" => self.studio_transport(crate::studio::StudioAction::PlayPause),
            "studio-export" => {
                let out = clubcraft_core::paths::exports_dir().join(format!("{}.wav", self.studio.mix.name));
                let _ = std::fs::create_dir_all(clubcraft_core::paths::exports_dir());
                self.eng.export_mix(&self.studio.mix, out, &self.sh);
            }
            "process" => {
                let q = arg.to_lowercase();
                if let Some(t) = self.sh.tracks.iter().find(|t| t.display_name().to_lowercase().contains(&q)) {
                    let id = t.id;
                    self.jobs.process_with_stems(id, true);
                }
            }
            "stop-jobs" => self.jobs.cancel_all(),
            "process-all" => {
                for t in &self.sh.tracks {
                    self.jobs.process_with_stems(t.id, false);
                }
            }
            _ => {}
        }
    }

    /// The app header: window buttons (macOS), mark and name, the view switcher in the middle,
    /// background jobs and tools on the right. Empty space drags the window; a double click
    /// zooms it.
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        use egui::{pos2, vec2, Align2, FontId, Rect, Sense, Stroke};
        let full = ui.max_rect();
        let p = ui.painter().clone();
        // window drag area (widgets placed later take the pointer first)
        let bg = ui.interact(full, ui.id().with("header-drag"), Sense::click_and_drag());
        if bg.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if bg.double_clicked() {
            let max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
        }
        p.line_segment([full.left_bottom(), full.right_bottom()], Stroke::new(1.0, theme::HEADER_LINE));
        let cy = full.center().y;

        // ---- left: mark + name
        let x0 = full.left() + crate::window_chrome::LIGHTS_W + 6.0;
        let mut left = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(x0, full.top()), pos2(full.center().x - 200.0, full.bottom()))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        left.spacing_mut().item_spacing.x = 9.0;
        crate::widgets::logo_mark(&mut left, 24.0, theme::TEXT);
        left.label(RichText::new("ClubCraft").size(15.0).strong().color(theme::TEXT));

        // ---- centre: view switcher (segmented control)
        let views = [(View::Home, "Home", "⌘1"), (View::Library, "Library", "⌘2"), (View::Studio, "Studio", "⌘3"), (View::Live, "Live", "⌘4")];
        let (seg_w, seg_h, pad) = (92.0, 28.0, 3.0);
        let track = Rect::from_center_size(pos2(full.center().x, cy), vec2(seg_w * views.len() as f32 + 2.0 * pad, seg_h + 2.0 * pad));
        p.rect_filled(track, 9.0, theme::SEG_TRACK);
        for (k, (v, name, key)) in views.into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(track.left() + pad + seg_w * k as f32, track.top() + pad), vec2(seg_w, seg_h));
            let resp = ui.interact(r, ui.id().with(("view", k)), Sense::click()).on_hover_text(format!("{name}  {key}"));
            let sel = self.view == v;
            if sel {
                p.rect_filled(r, 7.0, theme::SEG_ON);
                p.rect_stroke(r, 7.0, Stroke::new(1.0, theme::SEG_ON_LINE), egui::StrokeKind::Inside);
            } else if resp.hovered() {
                p.rect_filled(r, 7.0, theme::SEG_HOVER);
            }
            let col = if sel || resp.hovered() { theme::TEXT } else { theme::TEXT_DIM };
            p.text(r.center(), Align2::CENTER_CENTER, name, FontId::proportional(13.5), col);
            if resp.clicked() {
                self.view = v;
            }
        }

        // ---- right: jobs, stems, add music, settings
        let mut right = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(track.right() + 24.0, full.top()), pos2(full.right() - 14.0, full.bottom()))).layout(egui::Layout::right_to_left(egui::Align::Center)));
        right.spacing_mut().item_spacing.x = 8.0;
        let ui = &mut right;
        let icon = |t: &str| egui::Button::new(RichText::new(t).size(15.0).color(theme::TEXT_DIM)).frame(false).min_size(vec2(30.0, 30.0)).corner_radius(7.0);
        if ui.add(icon("⚙")).on_hover_text("Settings  ⌘,").clicked() {
            self.settings.open = !self.settings.open;
        }
        if ui.add(icon("+")).on_hover_text("Add a music folder").clicked() {
            if let Some(folders) = rfd::FileDialog::new().set_title("Add music folder").pick_folders() {
                self.jobs.import(folders, true);
            }
        }
        ui.add_space(2.0);
        // stems: quality and background separation in one compact menu
        let p2 = &mut self.prefs;
        let q = match p2.stem_quality.as_str() {
            "best" => "Best",
            "high" => "High",
            _ => "Fast",
        };
        let name = |q: &str| match q {
            "best" => "Best",
            "high" => "High",
            _ => "Fast",
        };
        let qe = name(&p2.export_stem_quality);
        let both = if qe == q { q.to_string() } else { format!("{q} · export {qe}") };
        let label = RichText::new(format!("Stems · {both}{}", if p2.separate_stems { " · auto" } else { "" })).size(12.5).color(theme::TEXT_DIM);
        ui.menu_button(label, |ui| {
            ui.set_min_width(300.0);
            ui.label(RichText::new("Stem quality for playback").size(11.5).color(theme::TEXT_DIM));
            ui.selectable_value(&mut p2.stem_quality, "best".to_string(), "Best: RoFormer vocals + htdemucs_ft (~2.5 min/track)");
            ui.selectable_value(&mut p2.stem_quality, "high".to_string(), "High: htdemucs_ft (~80 s/track)");
            ui.selectable_value(&mut p2.stem_quality, "fast".to_string(), "Fast: htdemucs (~17 s/track)");
            ui.separator();
            ui.label(RichText::new("Stem quality for export").size(11.5).color(theme::TEXT_DIM));
            ui.horizontal(|ui| {
                for (q, l) in [("best", "Best"), ("high", "High"), ("fast", "Fast")] {
                    ui.selectable_value(&mut p2.export_stem_quality, q.to_string(), l);
                }
            });
            ui.separator();
            ui.checkbox(&mut p2.separate_stems, "Separate in the background").on_hover_text("Separate stems for every imported and opened track in the background (uses a lot of CPU / GPU for a while). Off: stems are separated on demand, e.g. when you export a mix or press 'Stems missing' in the studio.");
        });
        let pending = self.jobs.pending();
        if pending > 0 {
            ui.add_space(6.0);
            if ui.add(egui::Button::new(RichText::new("Stop").size(12.0)).corner_radius(6.0)).on_hover_text("Stop all background jobs").clicked() {
                self.jobs.cancel_all();
            }
            let label = self.sh.job_status.values().next().map(|s| s.label.clone()).unwrap_or_default();
            let txt = format!("{pending} job{} · {label}", if pending == 1 { "" } else { "s" });
            ui.add(egui::Label::new(RichText::new(txt).size(12.0).color(theme::TEXT_DIM)).truncate()).on_hover_text("Background jobs: analysis and stem separation");
            ui.add(egui::Spinner::new().size(13.0));
        }
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.devctl.feed(ctx, raw_input);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_events(&ctx);
        if let Some(c) = self.ui_ctx_cmd.take() {
            ctx.send_viewport_cmd(c);
        }
        let menu_cmds = self.menu.as_ref().map(|m| m.poll()).unwrap_or_default();
        for c in menu_cmds {
            self.menu_command(c);
        }
        for cmd in self.devctl.poll(&ctx) {
            match cmd {
                crate::devctl::DevCmd::View(v) => self.view = v,
                crate::devctl::DevCmd::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                crate::devctl::DevCmd::Other(s) => self.dev_command(&s),
            }
        }

        // Files dropped from Finder are imported and analysed (unless they went onto a sample
        // lane in the studio, see below).
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        self.studio.consumed_drop = false;

        crate::window_chrome::place_traffic_lights();
        let header = egui::Frame::new().fill(theme::HEADER).inner_margin(egui::Margin::ZERO);
        egui::Panel::top("top").exact_size(crate::window_chrome::HEADER_H).resizable(false).frame(header).show(ui, |ui| self.top_bar(ui));
        let import_dropped = |app: &mut Self| {
            if !dropped.is_empty() && !app.studio.consumed_drop {
                if app.view == View::Studio {
                    let files = crate::importer::collect_audio(&dropped);
                    if !files.is_empty() {
                        app.status = format!("Adding {} track{} to the mix after analysis…", files.len(), if files.len() == 1 { "" } else { "s" });
                    }
                    app.studio.pending_adds.extend(files);
                }
                app.jobs.import(dropped.clone(), true);
            }
        };
        egui::Panel::bottom("status").exact_size(20.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&self.status).size(11.0).color(theme::TEXT_DIM));
            });
        });
        let mut actions = Vec::new();
        let mut home_actions = Vec::new();
        egui::CentralPanel::default().show(ui, |ui| match self.view {
            View::Home => {
                let selected: Vec<TrackId> = self.library.selected.iter().copied().collect();
                self.home.ui(ui, &self.sh, &self.mix_summaries, &selected, &mut home_actions);
            }
            View::Library => self.library.ui(ui, &self.sh, false, &mut actions),
            View::Studio => {
                let mut sa = Vec::new();
                let (mut load, mut new) = (None, false);
                let mixes = self.mixes.clone();
                self.studio.preview_playing = self.eng.preview_player.is_some();
                self.studio.ui(ui, &self.sh, &mixes, &mut sa, &mut load, &mut new);
                sa.append(&mut self.studio.pending);
                let ma = std::mem::take(&mut self.studio.mashup_actions);
                self.handle_mashup_actions(ma);
                if new {
                    self.save_mix();
                    self.studio.set_mix(clubcraft_core::Mix::default());
                }
                if let Some(id) = load {
                    self.save_mix();
                    if let Ok(Some(m)) = self.lib.mix(id) {
                        self.studio.set_mix(m);
                    }
                }
                self.handle_studio_actions(sa);
            }
            View::Live => {
                let mut live_actions = Vec::new();
                let (browser, sh, queue) = (&mut self.browser, &self.sh, &mut self.eng.automix_queue);
                self.live.ui(ui, &self.decks, &self.mixer, &mut live_actions, |ui| live_browser(ui, browser, sh, queue, &mut actions));
                self.handle_live_actions(live_actions);
            }
        });
        self.handle_library_actions(actions);
        self.handle_home_actions(home_actions);
        let mut sa = Vec::new();
        self.settings.ui(&ctx, self.eng.handle.as_ref(), &mut sa, &mut self.prefs);
        self.apply_prefs(&ctx);
        for a in sa {
            match a {
                crate::settings::SettingsAction::ProcessAll => {
                    for t in &self.sh.tracks {
                        self.jobs.process_with_stems(t.id, false);
                    }
                }
                crate::settings::SettingsAction::ReanalyseAll => {
                    for t in &self.sh.tracks {
                        self.jobs.analyze_only(t.id);
                    }
                }
            }
        }
        if self.view == View::Live {
            self.keyboard(&ctx);
        }
        import_dropped(self);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "view", &self.view);
        eframe::set_value(storage, "audio", &self.settings.audio);
        eframe::set_value(storage, "midi_port", &self.settings.midi_port);
        self.save_mix();
        if self.studio.mix.id > 0 {
            eframe::set_value(storage, "mix", &self.studio.mix.id);
        }
    }
}

/// Live screen browser: library table plus the automix queue.
fn live_browser(ui: &mut egui::Ui, browser: &mut LibraryView, sh: &Shared, queue: &mut Vec<TrackId>, actions: &mut Vec<LibraryAction>) {
    let w = ui.available_width();
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(w - 290.0);
            browser.ui(ui, sh, true, actions);
        });
        ui.separator();
        ui.vertical(|ui| {
            ui.set_width(270.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Automix queue").strong());
                if ui.small_button("+ selected").on_hover_text("Add the selected library tracks").clicked() {
                    let mut sel: Vec<TrackId> = browser.visible(sh).iter().map(|t| t.id).filter(|id| browser.selected.contains(id)).collect();
                    sel.retain(|id| !queue.contains(id));
                    queue.extend(sel);
                }
                if !queue.is_empty() && ui.small_button("Clear").clicked() {
                    queue.clear();
                }
            });
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(270.0, ui.available_height().max(60.0)), egui::Sense::hover());
            if let Some(p) = resp.dnd_release_payload::<crate::library_view::DraggedTracks>() {
                for id in &p.0 {
                    if !queue.contains(id) {
                        queue.push(*id);
                    }
                }
            }
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
            child.painter().rect_filled(rect, 4.0, theme::BG);
            if queue.is_empty() {
                child.label(RichText::new("Drag tracks here, then press AUTOMIX. Tracks are mixed at their analysed mix points.").color(theme::TEXT_DIM).size(11.0));
            }
            let mut remove = None;
            egui::ScrollArea::vertical().id_salt("queue").show(&mut child, |ui| {
                for (i, id) in queue.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let name = sh.track(*id).map(|t| t.display_name()).unwrap_or_default();
                        let a = sh.analyses.get(id);
                        ui.label(RichText::new(format!("{}.", i + 1)).color(theme::TEXT_DIM));
                        ui.add(egui::Label::new(crate::studio::truncate(&name, 26)).truncate());
                        if let Some(k) = a.and_then(|a| a.key) {
                            ui.label(RichText::new(crate::theme::key_label(k)).color(theme::key_color(k)).size(11.0));
                        }
                        if ui.small_button("✕").clicked() {
                            remove = Some(i);
                        }
                    });
                }
            });
            if let Some(i) = remove {
                queue.remove(i);
            }
        });
    });
}

pub(crate) fn prefs_job_settings(p: &crate::prefs::Prefs) -> crate::pipeline::Settings {
    crate::pipeline::Settings {
        separate_stems: p.separate_stems,
        quality: p.stem_quality.clone(),
        deep_structure: p.deep_structure,
        bpm_min: p.bpm_min,
        bpm_max: p.bpm_max,
    }
}
