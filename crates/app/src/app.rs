use crate::jobs::{Event, JobStatus, Jobs};
use crate::library_view::{LibraryAction, LibraryView};
use crate::theme;
use clubcraft_core::{Analysis, Library, Track, TrackId};
use eframe::egui::{self, RichText};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum View {
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
    }

    pub fn reload_track(&mut self, lib: &Library, id: TrackId) {
        if let Ok(Some(a)) = lib.analysis(id) {
            self.analyses.insert(id, a);
        }
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
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> App {
        theme::apply(&cc.egui_ctx);
        let lib = Library::open_default().expect("open library");
        let mut sh = Shared { deck_count: 2, ..Default::default() };
        sh.reload(&lib);
        let jobs = Jobs::new(Default::default());
        let mut view = cc.storage.and_then(|s| eframe::get_value(s, "view")).unwrap_or(View::Library);
        // Developer conveniences: `--import <path>` and `--view library|studio|live`.
        let args: Vec<String> = std::env::args().collect();
        for w in args.windows(2) {
            match w[0].as_str() {
                "--import" => jobs.import(vec![w[1].clone().into()], true),
                "--view" => {
                    view = match w[1].as_str() {
                        "studio" => View::Studio,
                        "live" => View::Live,
                        _ => View::Library,
                    }
                }
                _ => {}
            }
        }
        App { lib, sh, jobs, view, library: LibraryView::default(), status: String::new(), devctl: crate::devctl::DevCtl::from_env(), live: Default::default(), decks: vec![Default::default(); 2], mixer: Default::default(), browser: LibraryView::default() }
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
        }
        self.sh.job_status = self.jobs.active.clone();
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
                LibraryAction::LoadDeck(..) | LibraryAction::AddToMix(..) => {}
            }
        }
    }

    fn handle_live_actions(&mut self, actions: Vec<crate::live::model::LiveAction>) {
        for a in actions {
            log::debug!("live action {a:?}");
        }
    }

    fn dev_command(&mut self, cmd: &str) {
        log::info!("devctl: {cmd}");
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("ClubCraft").strong().size(17.0).color(theme::ACCENT));
            ui.add_space(12.0);
            for (v, name) in [(View::Library, "Library"), (View::Studio, "Studio"), (View::Live, "Live")] {
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
                let mut s = self.jobs.settings.lock();
                egui::ComboBox::from_id_salt("quality")
                    .selected_text(format!("Stems: {}", s.quality))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s.quality, "fast".to_string(), "Fast (htdemucs)");
                        ui.selectable_value(&mut s.quality, "high".to_string(), "High (htdemucs_ft)");
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
        egui::CentralPanel::default().show(ui, |ui| match self.view {
            View::Library => self.library.ui(ui, &self.sh, false, &mut actions),
            View::Studio => {
                ui.label("Studio");
            }
            View::Live => {
                let mut live_actions = Vec::new();
                let (browser, sh) = (&mut self.browser, &self.sh);
                self.live.ui(ui, &self.decks, &self.mixer, &mut live_actions, |ui| browser.ui(ui, sh, true, &mut actions));
                self.handle_live_actions(live_actions);
            }
        });
        self.handle_library_actions(actions);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "view", &self.view);
    }
}
