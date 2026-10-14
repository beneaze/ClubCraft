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
    pub mix_summaries: Vec<crate::home::MixSummary>,
    prepared: (i64, usize),
    queued: std::collections::HashSet<TrackId>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> App {
        theme::apply(&cc.egui_ctx);
        let lib = Library::open_default().expect("open library");
        let mut sh = Shared { deck_count: 2, ..Default::default() };
        sh.reload(&lib);
        let mut job_settings = crate::pipeline::Settings::default();
        if let Some(q) = cc.storage.and_then(|s| eframe::get_value::<String>(s, "stems_quality")) {
            job_settings.quality = q;
        }
        if let Some(a) = cc.storage.and_then(|s| eframe::get_value::<bool>(s, "auto_stems")) {
            job_settings.separate_stems = a;
        }
        let jobs = Jobs::new(job_settings);
        // Always start on the home page (unless `--view` says otherwise).
        let mut view = View::Home;
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
        let mut app = App { lib, sh, jobs, view, library: LibraryView::default(), status: String::new(), devctl: crate::devctl::DevCtl::from_env(), live: Default::default(), decks: vec![Default::default(); 2], mixer: Default::default(), browser: LibraryView::default(), studio: Default::default(), mixes: Vec::new(), eng: crate::live::adapter::LiveEngine::start(audio_cfg.clone()), settings: crate::settings::Settings::new(audio_cfg, midi_port), home: Default::default(), mix_summaries: Vec::new(), prepared: (-1, 0), queued: Default::default() };
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
                LibraryAction::Reveal(id) => {
                    if let Some(t) = self.sh.track(id) {
                        let _ = std::process::Command::new("open").arg("-R").arg(&t.path).spawn();
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
                    let mut mix = clubcraft_core::planner::plan_mix(&name, &order, &|id| self.sh.analyses.get(&id).cloned(), &Default::default());
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
                H::Settings => self.settings.open = true,
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

    fn handle_studio_actions(&mut self, actions: Vec<crate::studio::StudioAction>) {
        use crate::studio::StudioAction as A;
        for a in actions {
            match a {
                A::SaveMix => self.save_mix(),
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
            "settings" => self.settings.open = true,
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
            ui.label(RichText::new("ClubCraft").strong().size(17.0).color(theme::ACCENT));
            ui.add_space(12.0);
            for (v, name) in [(View::Home, "Home"), (View::Library, "Library"), (View::Studio, "Studio"), (View::Live, "Live")] {
                let sel = self.view == v;
                let b = egui::Button::new(RichText::new(name).size(14.0).strong()).selected(sel).min_size(egui::vec2(86.0, 24.0));
                if ui.add(b).clicked() {
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
                let mut s = self.jobs.settings.lock();
                egui::ComboBox::from_id_salt("quality")
                    .selected_text(format!("Stems: {}", s.quality))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s.quality, "best".to_string(), "Best: RoFormer vocals + htdemucs_ft (~3.5 min/track)");
                        ui.selectable_value(&mut s.quality, "fast".to_string(), "Fast: htdemucs (~17 s/track)");
                        ui.selectable_value(&mut s.quality, "high".to_string(), "High: htdemucs_ft (~80 s/track)");
                        ui.selectable_value(&mut s.quality, "ultra".to_string(), "Ultra (ft + shifts)");
                    });
                ui.checkbox(&mut s.separate_stems, "Auto stems");
                drop(s);
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
                self.studio.ui(ui, &self.sh, &mixes, &mut sa, &mut load, &mut new);
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
        self.settings.ui(&ctx, self.eng.handle.as_ref(), &mut sa);
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
        {
            let js = self.jobs.settings.lock();
            eframe::set_value(storage, "stems_quality", &js.quality);
            eframe::set_value(storage, "auto_stems", &js.separate_stems);
        }
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
                            ui.label(RichText::new(k.camelot()).color(theme::key_color(k)).size(11.0));
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
