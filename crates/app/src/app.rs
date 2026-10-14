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
    pub job_status: HashMap<Option<TrackId>, JobStatus>,
    pub cue_counts: HashMap<TrackId, usize>,
    pub waveforms: HashMap<TrackId, std::sync::Arc<clubcraft_core::Waveform>>,
    pub user_cues: HashMap<TrackId, Vec<clubcraft_core::Cue>>,
    pub deck_count: usize,
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
    /// Last seen change from `clubcraft mcp` (marker mtime) and when it was checked.
    external: (Option<std::time::SystemTime>, f64),
    queued: std::collections::HashSet<TrackId>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> App {
        theme::apply(&cc.egui_ctx);
        let lib = Library::open_default().expect("open library");
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
        let mut app = App { lib, sh, jobs, view, library: LibraryView::default(), status: String::new(), devctl: crate::devctl::DevCtl::from_env(), live: Default::default(), decks: vec![Default::default(); 2], mixer: Default::default(), browser: LibraryView::default(), studio: Default::default(), mixes: Vec::new(), eng: crate::live::adapter::LiveEngine::start(audio_cfg.clone()), settings: crate::settings::Settings::new(audio_cfg, midi_port), home: Default::default(), prefs, applied_prefs: None, menu: crate::menu::AppMenu::install(&cc.egui_ctx), ui_ctx_cmd: None, mix_summaries: Vec::new(), prepared: (-1, 0), queued: Default::default(), external: (std::fs::metadata(crate::mcp::change_marker()).and_then(|m| m.modified()).ok(), 0.0) };
        app.settings.connect_midi(app.eng.handle.as_ref());
        if let Some(e) = &app.eng.error {
            app.status = e.clone();
        }
        app.refresh_mixes();
        let last_mix = cc.storage.and_then(|s| eframe::get_value::<i64>(s, "mix"));
        if let Some(m) = last_mix.and_then(|id| app.lib.mix(id).ok().flatten()) {
            app.studio.set_mix(m);
        }
        app
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        let events = self.jobs.poll();
        let mut reload_all = false;
        for e in events {
            match e {
                Event::LibraryChanged => reload_all = true,
                Event::TrackUpdated(id) => self.sh.reload_track(&self.lib, id),
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
        self.sh.job_status = self.jobs.active.clone();
        for m in self.eng.tick(&self.lib, &self.sh, &self.jobs) {
            self.status = m;
        }
        for id in std::mem::take(&mut self.eng.touched) {
            self.sh.reload_track(&self.lib, id);
        }
        let key = (self.studio.mix.id, self.studio.mix.tracks.len());
        if key != self.prepared {
            self.prepared = key;
            self.prepare_mix_tracks();
        }
        let st = self.eng.state();
        self.settings.tick(&st);
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
        let anything_playing = st.decks.iter().any(|d| d.playing) || st.mix.active || self.eng.export.is_some() || self.eng.loading_mix.is_some();
        if self.view == View::Live || anything_playing {
            ctx.request_repaint();
        }
        if self.jobs.pending() > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn handle_library_actions(&mut self, actions: Vec<LibraryAction>) {
        for a in actions {
            match a {
                LibraryAction::Process(ids) => {
                    for id in ids {
                        self.jobs.process(id, true);
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
                A::FindMashups(id) => self.studio.mashup.open_for(id, &self.sh),
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
                    self.eng.export_mix(&self.studio.mix, out, &self.sh);
                    self.status = "Rendering mix…".into();
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
                if let Some((id, _, _)) = self.mixes.iter().find(|m| m.1.to_lowercase().contains(&arg.to_lowercase())) {
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
                    _ => crate::studio::Selection::None,
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
            "engine-state" => {
                let st = self.eng.state();
                log::info!("engine: mix active {} playing {} time {:.2} | decks {:?}", st.mix.active, st.mix.playing, st.mix.time, st.decks.iter().map(|d| (d.loaded, d.playing, (d.position * 10.0).round() / 10.0, d.track_id)).collect::<Vec<_>>());
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
                    self.jobs.process(id, true);
                }
            }
            "process-all" => {
                for t in &self.sh.tracks {
                    self.jobs.process(t.id, false);
                }
            }
            _ => {}
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("ClubCraft").strong().size(17.0).color(theme::accent()));
            ui.add_space(12.0);
            for (v, name) in [(View::Home, "Home"), (View::Library, "Library"), (View::Studio, "Studio"), (View::Live, "Live")] {
                let sel = self.view == v;
                let b = egui::Button::new(RichText::new(name).size(14.0).strong()).selected(sel).min_size(egui::vec2(86.0, 24.0));
                let r = ui.add(b);
                if sel {
                    // accent underline marks the current view
                    let rr = r.rect.shrink2(egui::vec2(10.0, 0.0));
                    ui.painter().line_segment([rr.left_bottom() + egui::vec2(0.0, 2.0), rr.right_bottom() + egui::vec2(0.0, 2.0)], egui::Stroke::new(2.0, theme::accent()));
                }
                if r.clicked() {
                    self.view = v;
                }
            }
            ui.add_space(16.0);
            if ui.button("+ Add music…").clicked() {
                if let Some(folders) = rfd::FileDialog::new().set_title("Add music folder").pick_folders() {
                    self.jobs.import(folders, true);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("⚙ Settings").clicked() {
                    self.settings.open = !self.settings.open;
                }
                let p = &mut self.prefs;
                egui::ComboBox::from_id_salt("quality")
                    .selected_text(format!("Stems: {}", p.stem_quality))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut p.stem_quality, "best".to_string(), "Best: RoFormer vocals + htdemucs_ft (~3.5 min/track)");
                        ui.selectable_value(&mut p.stem_quality, "high".to_string(), "High: htdemucs_ft (~80 s/track)");
                        ui.selectable_value(&mut p.stem_quality, "fast".to_string(), "Fast: htdemucs (~17 s/track)");
                    });
                ui.checkbox(&mut p.separate_stems, "Auto stems");
                let pending = self.jobs.pending();
                if pending > 0 {
                    if ui.small_button("Stop").clicked() {
                        self.jobs.cancel_all();
                    }
                    ui.spinner();
                    let label = self.sh.job_status.values().next().map(|s| s.label.clone()).unwrap_or_default();
                    ui.label(RichText::new(format!("{pending} jobs · {label}")).color(theme::TEXT_DIM));
                }
            });
        });
    }
}

impl eframe::App for App {
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

        // Files dropped from Finder are imported and analysed.
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if !dropped.is_empty() {
            self.jobs.import(dropped, true);
        }

        egui::Panel::top("top").exact_size(36.0).show(ui, |ui| self.top_bar(ui));
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
                        self.jobs.process(t.id, false);
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
