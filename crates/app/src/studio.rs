//! Studio screen (DJ.Studio-style mix designer).

use crate::app::Shared;
use crate::library_view::DraggedTracks;
use crate::theme;

use clubcraft_core::planner::{self, OrderItem, OrderOptions, PlanOptions};
use clubcraft_core::{Analysis, Mix, MixTimeline, MixTrack, Param, TempoMode, TrackId, TransitionStyle};
use eframe::egui::{self, pos2, vec2, Align, Color32, Layout, RichText, Sense, Stroke, StrokeKind, Ui};

#[derive(Debug, Clone, PartialEq)]
pub enum StudioAction {
    PlayPause,
    Seek(f64),
    Export,
    SendToLive,
    SaveMix,
    ImportDjStudio,
    ExportVirtualDj,
    /// Open the mashup finder for the vocals of a track.
    FindMashups(Option<clubcraft_core::TrackId>),
    /// Find the best vocal-over point for transition `k`.
    FindVocalOver(usize),
    Harmonize,
    AutoMixPoints,
    /// Separate stems for these tracks now.
    SeparateStems(Vec<TrackId>),
    /// Save the open mix as a new mix and open that copy.
    DuplicateMix,
    /// Keep the open mix's current state as a savepoint with this name (empty: numbered).
    AddSavepoint(String),
    /// Go back (or forward) to a savepoint; every other savepoint stays.
    RestoreSavepoint(i64),
    DeleteSavepoint(i64),
    /// Listen to a library track from this many seconds (the "Add tracks" dialog).
    PreviewTrack(TrackId, f64),
    StopPreview,
}

/// Tabs of the bottom bar; each opens a panel above the transport.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Zoom,
    Playlist,
    Transition,
    Track,
    Mashups,
    Master,
}

impl Tab {
    pub const ALL: [Tab; 6] = [Tab::Zoom, Tab::Playlist, Tab::Transition, Tab::Track, Tab::Mashups, Tab::Master];

    fn label(self) -> (&'static str, &'static str) {
        match self {
            Tab::Zoom => ("🔍", "Zoom"),
            Tab::Playlist => ("☰", "Playlist"),
            Tab::Transition => ("🔀", "Transition"),
            Tab::Track => ("🎵", "Track"),
            Tab::Mashups => ("🎤", "Mashups"),
            Tab::Master => ("🔊", "Master"),
        }
    }
}

/// Several tracks and sample clips selected at once (box selection, Shift-click in the timeline).
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct MultiSel {
    pub tracks: std::collections::BTreeSet<usize>,
    pub samples: std::collections::BTreeSet<usize>,
}

impl MultiSel {
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty() && self.samples.is_empty()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Selection {
    None,
    Track(usize),
    Transition(usize),
    /// A clip on a sample lane (index into `Mix::samples`).
    Sample(usize),
}

pub struct StudioView {
    pub mix: Mix,
    pub timeline: MixTimeline,
    pub dirty: bool,
    /// Actions raised outside `ui`'s `out` (inspector buttons), drained by the app.
    pub pending: Vec<StudioAction>,
    /// The mashup finder, shown in the bottom section instead of the inspector while open.
    pub mashup: crate::mashup_view::MashupView,
    pub mashup_actions: Vec<crate::mashup_view::MashupAction>,
    /// A mashup preview is playing (set by the app).
    pub preview_playing: bool,
    /// Play position of the running preview (s, track time; set by the app).
    pub preview_pos: f64,
    /// The "Add tracks" dialog.
    pub picker: crate::track_picker::TrackPicker,
    last_selection: Selection,
    /// The transition the playhead was in on the last frame.
    playhead_transition: Option<usize>,
    /// Counts [`Self::set_mix`] calls: a running preview belongs to the mix it was started with.
    pub mix_serial: u64,
    /// Savepoints of the open mix, oldest first: (id, name, created at). Kept by the app.
    pub savepoints: Vec<(i64, String, i64)>,
    /// The savepoint the mix is at (saved or restored, unchanged since).
    pub at_savepoint: Option<i64>,
    savepoint_name: String,
    /// Open panel of the bottom bar.
    pub tab: Option<Tab>,
    /// Undo / redo history of the mix (one step per edit gesture).
    undo: Vec<Mix>,
    redo: Vec<Mix>,
    committed: Mix,
    last_change: Option<std::time::Instant>,
    /// Width of the centred transport group (measured the previous frame).
    transport_w: f32,
    /// Distance from the transport group's left edge to the centre of its play button (last
    /// frame), so the play button itself sits in the middle of the window.
    play_offset: f32,
    /// Width the transport row needs at full size (left + centre + right groups, measured the
    /// last time it was laid out that way); narrower rows use the compact labels.
    transport_full_w: f32,
    pub selection: Selection,
    /// Pixels per second in the timeline.
    pub zoom: f32,
    pub scroll: f32,
    pub follow: bool,
    pub order_opts: OrderOptions,
    pub playing: bool,
    /// Playlist row of the track on air last frame (the playlist follows it when it changes).
    playlist_on_air: Option<usize>,
    pub position: f64,
    pub render_progress: Option<f32>,
    /// Text on the render progress bar (empty = "Rendering…").
    pub render_label: String,
    pub message: String,
    pub timeline_dirty: bool,
    /// Set when the mix changed; the app pushes edits into a running preview.
    pub edited: bool,
    pub lane_show: [crate::studio_timeline::ShowMode; clubcraft_core::MIX_LANES],
    /// What the timeline draws and edits inside the selected transition.
    pub curve_target: crate::studio_timeline::CurveTarget,
    /// What it draws and edits on the selected sample clip (never the stems).
    pub sample_target: crate::studio_timeline::CurveTarget,
    /// Curves of stem rows switched off in the timeline, to bring back when switched on again:
    /// (transition, incoming side, stem index).
    pub stem_stash: std::collections::HashMap<(usize, bool, usize), clubcraft_core::Lane>,
    /// Effect rows shown in the timeline although the effect is not used yet (chosen with
    /// "Manual"): (transition, effect).
    pub fx_shown: std::collections::HashSet<(usize, Param)>,
    /// The deck lanes take all the room they can while effect rows are edited.
    pub fx_expanded: bool,
    pub tempo_open: bool,
    pub drag: Option<crate::studio_timeline::Drag>,
    pub drag_acc: f32,
    /// Tracks and samples selected together (alongside `selection`, which is then `None`).
    pub multi: MultiSel,
    /// The multi-selection a box selection adds to (Shift), and the starts of the samples a
    /// group drag moves (index, start beat).
    pub(crate) multi_base: MultiSel,
    pub(crate) drag_origins: Vec<(usize, f64)>,
    pub tcat: crate::studio_transition::TCategory,
    /// Zoom to fit the whole mix on the next frame.
    pub fit_request: bool,
    /// Defaults for new transitions (from the preferences).
    pub plan_opts: PlanOptions,
    /// Collapsed lanes in the timeline and the track strip.
    pub lane_collapsed: [bool; clubcraft_core::MIX_LANES],
    /// Sample lanes expanded.
    pub sample_open: [bool; clubcraft_core::SAMPLE_LANES],
    /// Decoded sample overviews (duration, peaks).
    pub sample_cache: std::collections::HashMap<std::path::PathBuf, Option<(f64, std::sync::Arc<Vec<f32>>)>>,
    /// Files dropped this frame went onto a sample lane (the app must not import them).
    pub consumed_drop: bool,
    /// Files dropped from Finder onto the studio, in drop order: they are imported like any drop
    /// and join the end of the mix once their analysis is in (mix points need the grid and cues).
    pub pending_adds: Vec<std::path::PathBuf>,
    /// Selected time range on a track and the drag anchor.
    pub range: Option<crate::studio_timeline::RangeSel>,
    pub range_anchor: f64,
    /// Extractions asked for this frame: (part, save to a file instead of a sample lane).
    pub extract_requests: Vec<(crate::extract::Part, bool)>,
    extract_tx: crossbeam_channel::Sender<ExtractDone>,
    /// Sample files being cut again from their track (see [`StudioView::recut_sample`]), and
    /// those that couldn't be (not tried again this session).
    recutting: std::collections::HashSet<std::path::PathBuf>,
    recut_failed: std::collections::HashSet<std::path::PathBuf>,
    /// The linked sample clips are to be checked for better stems: stems or settings changed,
    /// or an export starts (see [`StudioView::check_linked_samples`]).
    linked_check: bool,
    extract_rx: crossbeam_channel::Receiver<ExtractDone>,
    pub extracting: usize,
}

impl Default for StudioView {
    fn default() -> Self {
        let (extract_tx, extract_rx) = crossbeam_channel::unbounded();
        StudioView {
            mix: Mix::default(),
            timeline: MixTimeline::default(),
            dirty: false,
            pending: Vec::new(),
            mashup: Default::default(),
            mashup_actions: Vec::new(),
            preview_playing: false,
            preview_pos: 0.0,
            picker: Default::default(),
            last_selection: Selection::None,
            playhead_transition: None,
            mix_serial: 0,
            savepoints: Vec::new(),
            at_savepoint: None,
            savepoint_name: String::new(),
            tab: Some(Tab::Playlist),
            undo: Vec::new(),
            redo: Vec::new(),
            committed: Mix::default(),
            last_change: None,
            transport_w: 420.0,
            play_offset: 210.0,
            transport_full_w: 0.0,
            selection: Selection::None,
            zoom: 6.0,
            scroll: 0.0,
            follow: true,
            order_opts: OrderOptions::default(),
            playing: false,
            playlist_on_air: None,
            position: 0.0,
            render_progress: None,
            render_label: String::new(),
            message: String::new(),
            timeline_dirty: true,
            edited: false,
            lane_show: Default::default(),
            curve_target: Default::default(),
            sample_target: crate::studio_timeline::CurveTarget::Volume,
            stem_stash: Default::default(),
            fx_shown: Default::default(),
            fx_expanded: false,
            tempo_open: true,
            drag: None,
            drag_acc: 0.0,
            multi: MultiSel::default(),
            multi_base: MultiSel::default(),
            drag_origins: Vec::new(),
            tcat: Default::default(),
            fit_request: true,
            plan_opts: PlanOptions::default(),
            lane_collapsed: [false; clubcraft_core::MIX_LANES],
            sample_open: [true, false],
            sample_cache: Default::default(),
            consumed_drop: false,
            pending_adds: Vec::new(),
            range: None,
            range_anchor: 0.0,
            extract_requests: Vec::new(),
            extract_tx,
            recutting: Default::default(),
            recut_failed: Default::default(),
            linked_check: true,
            extract_rx,
            extracting: 0,
        }
    }
}

/// A finished extraction: the WAV and its length, and what to do with it.
struct ExtractDone {
    /// The file, its length (s) and the model of the stems it was cut from.
    result: Result<(std::path::PathBuf, f64, Option<String>), String>,
    then: ExtractThen,
    label: String,
}

enum ExtractThen {
    /// It was saved as a file.
    Saved,
    /// Put it on a sample lane at this mix beat, locked to its track.
    Place(f64, clubcraft_core::SampleSource, Option<clubcraft_core::SampleLock>),
    /// It replaces the file `old` of the clips cut from the same part of a track, reaching
    /// `s0` seconds (of the old file) further back.
    /// `better`: cut for better stems (when they don't come out, it isn't tried again).
    Recut { old: std::path::PathBuf, s0: f64, source: clubcraft_core::SampleSource, better: bool },
}

/// A drop indicator line along the top of a hovered playlist row.
/// Three small level bars bouncing at different rates: the playlist's "now playing" marker.
fn playing_bars(ui: &mut Ui) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
    let t = ui.input(|i| i.time) as f32;
    let w = r.width() / 3.0;
    for (k, (rate, phase)) in [(7.0f32, 0.0f32), (9.5, 1.7), (6.0, 3.1)].into_iter().enumerate() {
        let level = 0.3 + 0.7 * (0.5 + 0.5 * (t * rate + phase).sin());
        let x0 = r.left() + k as f32 * w;
        let bar = egui::Rect::from_min_max(pos2(x0 + 1.0, r.bottom() - level * r.height()), pos2(x0 + w - 1.0, r.bottom()));
        ui.painter().rect_filled(bar, 1.0, theme::accent());
    }
    ui.ctx().request_repaint();
    resp
}

/// A playlist cell with the spacing around it, so row tints and link lines join up between cells
/// and rows, except on the side where the list's edge cuts the cell (a row scrolled under the
/// header): nothing is painted past the visible list.
fn cell_area(ui: &Ui) -> egui::Rect {
    let (cell, clip, gap) = (ui.max_rect(), ui.clip_rect(), ui.spacing().item_spacing * 0.5);
    let top = if clip.top() > cell.top() + 0.5 { clip.top() } else { cell.top() - gap.y };
    let bottom = if clip.bottom() < cell.bottom() - 0.5 { clip.bottom() } else { cell.bottom() + gap.y };
    egui::Rect::from_x_y_ranges((cell.left() - gap.x)..=(cell.right() + gap.x), top..=bottom.max(top))
}

/// Runs of tracks joined by locked transitions (only runs of two or more).
fn link_groups(mix: &Mix) -> Vec<std::ops::RangeInclusive<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..=mix.tracks.len() {
        if !mix.transitions.get(i - 1).is_some_and(|t| t.locked) || i == mix.tracks.len() {
            if i - 1 > start {
                out.push(start..=i - 1);
            }
            start = i;
        }
    }
    out
}

fn ui_line(r: &egui::Response) {
    r.ctx.layer_painter(r.layer_id).line_segment([r.rect.left_top(), r.rect.right_top()], Stroke::new(2.0, theme::accent()));
}

/// Key badge in the playlist.
fn key_chip(ui: &mut Ui, k: clubcraft_core::Key) {
    let (r, _) = ui.allocate_exact_size(vec2(38.0, 20.0), Sense::hover());
    ui.painter().rect_stroke(r, 4.0, Stroke::new(1.5, theme::key_color(k)), StrokeKind::Inside);
    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, theme::key_label(k), egui::FontId::proportional(11.5), theme::key_color(k));
}

fn analysis_of(sh: &Shared, id: TrackId) -> Option<Analysis> {
    sh.analyses.get(&id).cloned()
}

impl StudioView {
    pub fn set_mix(&mut self, mix: Mix) {
        self.mix_serial += 1;
        self.at_savepoint = None;
        self.committed = mix.clone();
        self.undo.clear();
        self.redo.clear();
        self.last_change = None;
        self.mix = mix;
        self.selection = Selection::None;
        self.multi = MultiSel::default();
        self.position = 0.0;
        self.scroll = 0.0;
        self.timeline_dirty = true;
        self.dirty = false;
        self.fit_request = true;
        self.follow = true;
    }

    pub fn changed(&mut self) {
        self.mix.normalize();
        // Drags call this every frame: changes less than 0.6 s apart form one undo step.
        let now = std::time::Instant::now();
        if self.last_change.is_none_or(|t| now.duration_since(t).as_secs_f32() > 0.6) && self.committed != self.mix {
            self.undo.push(self.committed.clone());
            if self.undo.len() > 100 {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        self.last_change = Some(now);
        self.committed = self.mix.clone();
        self.timeline_dirty = true;
        self.dirty = true;
        self.edited = true;
        self.at_savepoint = None;
    }

    /// Replace the mix with the state of a savepoint, as one undo step.
    pub fn go_to_savepoint(&mut self, m: Mix) {
        self.undo.push(self.mix.clone());
        self.redo.clear();
        self.restore(m);
    }

    fn restore(&mut self, m: Mix) {
        self.mix = m;
        self.multi = MultiSel::default();
        self.committed = self.mix.clone();
        self.last_change = None;
        self.timeline_dirty = true;
        self.dirty = true;
        self.edited = true;
        if matches!(self.selection, Selection::Track(i) if i >= self.mix.tracks.len()) || matches!(self.selection, Selection::Transition(k) if k >= self.mix.transitions.len()) {
            self.selection = Selection::None;
        }
    }

    pub fn undo(&mut self) {
        if let Some(m) = self.undo.pop() {
            self.redo.push(self.mix.clone());
            self.restore(m);
        }
    }

    pub fn redo(&mut self) {
        if let Some(m) = self.redo.pop() {
            self.undo.push(self.mix.clone());
            self.restore(m);
        }
    }

    pub fn refresh_timeline(&mut self, sh: &Shared) {
        self.timeline = MixTimeline::build(&self.mix, &|id| analysis_of(sh, id));
        self.timeline_dirty = false;
        self.apply_sample_locks();
    }

    fn picker_ui(&mut self, ctx: &egui::Context, sh: &Shared, out: &mut Vec<StudioAction>) {
        use crate::track_picker::PickerAction;
        let in_mix: Vec<TrackId> = self.mix.tracks.iter().map(|t| t.track_id).collect();
        let mut acts = Vec::new();
        self.picker.ui(ctx, sh, &in_mix, in_mix.last().copied(), self.preview_playing, self.preview_pos, &mut acts);
        for a in acts {
            match a {
                PickerAction::Preview(id, from) => out.push(StudioAction::PreviewTrack(id, from)),
                PickerAction::StopPreview => out.push(StudioAction::StopPreview),
                PickerAction::Add(ids) => self.add_tracks(sh, &ids),
            }
        }
    }

    /// Add tracks at the end with automatically planned mix points.
    pub fn add_tracks(&mut self, sh: &Shared, ids: &[TrackId]) {
        let mut order: Vec<TrackId> = self.mix.tracks.iter().map(|t| t.track_id).collect();
        let start = order.len();
        order.extend(ids.iter().copied().filter(|id| !self.mix.tracks.iter().any(|t| t.track_id == *id)));
        let planned = planner::plan_mix(&self.mix.name, &order, &|id| analysis_of(sh, id), &self.plan_opts);
        for (i, t) in planned.tracks.into_iter().enumerate().skip(start) {
            self.mix.tracks.push(t);
            if i > 0 && self.mix.transitions.len() < i {
                self.mix.transitions.push(planned.transitions[i - 1].clone());
            }
        }
        if start > 0 && start < self.mix.tracks.len() {
            // the previous last track now needs a proper mix-out
            if let Some(a) = analysis_of(sh, self.mix.tracks[start - 1].track_id) {
                self.mix.tracks[start - 1].mix_out = planner::default_mix_out(&a);
            }
        }
        self.changed();
    }

    /// Zoom by `f`, keeping the playhead at the same place on screen.
    pub fn zoom_around_playhead(&mut self, f: f32) {
        let old = self.zoom;
        self.zoom = (self.zoom * f).clamp(crate::studio_timeline::MIN_ZOOM, crate::studio_timeline::MAX_ZOOM);
        let x = self.position as f32 * old - self.scroll; // playhead offset in the view
        self.scroll = (self.position as f32 * self.zoom - x).max(0.0);
    }

    pub fn harmonize(&mut self, sh: &Shared) {
        let item = |t: &MixTrack| OrderItem::from_analysis(t.track_id, sh.analyses.get(&t.track_id));
        let order: Vec<TrackId> = if self.mix.transitions.iter().any(|t| t.locked) {
            // tracks joined by locked transitions move as one group, in their order
            let mut groups: Vec<Vec<usize>> = vec![vec![0]];
            for i in 1..self.mix.tracks.len() {
                if self.mix.transitions.get(i - 1).is_some_and(|t| t.locked) {
                    if let Some(g) = groups.last_mut() {
                        g.push(i);
                    }
                } else {
                    groups.push(vec![i]);
                }
            }
            let items: Vec<Vec<OrderItem>> = groups.iter().map(|g| g.iter().map(|&i| item(&self.mix.tracks[i])).collect()).collect();
            planner::order_groups(&items, &self.order_opts).into_iter().flat_map(|g| groups[g].iter().map(|&i| self.mix.tracks[i].track_id).collect::<Vec<_>>()).collect()
        } else {
            let items: Vec<OrderItem> = self.mix.tracks.iter().map(item).collect();
            planner::order_tracks(&items, &self.order_opts)
        };
        self.replan(sh, &order);
        self.message = "Re-ordered for harmonic flow, tempo and energy".into();
    }

    pub fn replan(&mut self, sh: &Shared, order: &[TrackId]) {
        let styles: Vec<TransitionStyle> = self.mix.transitions.iter().map(|t| t.style).collect();
        let keep_style = styles.iter().all(|s| *s == styles.first().copied().unwrap_or(TransitionStyle::StemBlend));
        let mut planned = planner::plan_mix(&self.mix.name, order, &|id| analysis_of(sh, id), &PlanOptions { style: if keep_style { styles.first().copied() } else { self.plan_opts.style }, ..self.plan_opts });
        planned.id = self.mix.id;
        planned.tempo = self.mix.tempo;
        planned.keylock = self.mix.keylock;
        planned.lanes = self.mix.lanes.clone();
        // the sample lanes aren't part of the plan
        planned.samples = self.mix.samples.clone();
        planned.sample_lanes = self.mix.sample_lanes.clone();
        // locked transitions keep their mix points, curves and key relation
        let old = &self.mix;
        for k in 0..planned.transitions.len() {
            let (a, b) = (planned.tracks[k].track_id, planned.tracks[k + 1].track_id);
            if let Some(j) = (0..old.transitions.len()).find(|&j| old.transitions[j].locked && old.tracks[j].track_id == a && old.tracks[j + 1].track_id == b) {
                planned.tracks[k].mix_out = old.tracks[j].mix_out;
                planned.tracks[k + 1].mix_in = old.tracks[j + 1].mix_in;
                planned.tracks[k + 1].key_shift = planned.tracks[k].key_shift + (old.tracks[j + 1].key_shift - old.tracks[j].key_shift);
                planned.transitions[k] = old.transitions[j].clone();
            }
        }
        self.mix = planned;
        self.changed();
    }

    pub fn ui(&mut self, ui: &mut Ui, sh: &Shared, mixes: &[(i64, String, i64)], out: &mut Vec<StudioAction>, load_mix: &mut Option<i64>, new_mix: &mut bool) {
        self.picker_ui(ui.ctx(), sh, out);
        if self.timeline_dirty {
            self.refresh_timeline(sh);
        }
        self.follow_playhead_into_transitions(ui.ctx());
        self.keyboard(ui.ctx(), sh, out);
        self.tick_samples(sh);
        // the finder was opened from elsewhere (library, menu, inspector button)
        if self.mashup.take_opened() {
            self.tab = Some(Tab::Mashups);
        }
        // Bottom bar (DJ.Studio style), from the bottom up: tabs, transport, the open panel.
        let bar = egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(12, 4));
        egui::Panel::bottom("studio-tabs").exact_size(40.0).resizable(false).frame(bar).show(ui, |ui| self.tab_bar(ui));
        egui::Panel::bottom("studio-transport")
            .exact_size(48.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(theme::blend(theme::PANEL, theme::BG, 0.4)).inner_margin(egui::Margin::symmetric(12, 4)))
            .show(ui, |ui| self.transport(ui, sh, mixes, out, load_mix, new_mix));
        if let Some(tab) = self.tab {
            let (default, min) = match tab {
                Tab::Zoom => (54.0, 50.0),
                Tab::Playlist => (100.0, 90.0),
                Tab::Master => (90.0, 80.0),
                Tab::Track => (330.0, 220.0),
                Tab::Transition => (260.0, 230.0),
                Tab::Mashups => (430.0, 240.0),
            };
            egui::Panel::bottom(format!("studio-tab2-{tab:?}"))
                .default_size(default)
                .min_size(min)
                .max_size((ui.available_height() * 0.7).max(min + 20.0))
                .resizable(!matches!(tab, Tab::Zoom))
                .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(12, 8)))
                .show(ui, |ui| self.tab_panel(ui, sh, tab));
        }
        // The keys and the panels above may have added or removed tracks this frame; the timeline
        // indexes the mix's tracks by its own, so it must match them before it's drawn.
        if self.timeline_dirty {
            self.refresh_timeline(sh);
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::symmetric(8, 6))).show(ui, |ui| {
            self.timeline_ui(ui, sh, out);
        });
        // selecting a clip / transition opens its tab
        if self.selection != self.last_selection {
            match self.selection {
                Selection::Track(_) => self.tab = Some(Tab::Track),
                Selection::Transition(_) => self.tab = Some(Tab::Transition),
                Selection::Sample(_) | Selection::None => {}
            }
        }
        self.last_selection = self.selection;
        if self.tab != Some(Tab::Mashups) && self.mashup.open {
            self.mashup.close(&mut self.mashup_actions);
        }
    }

    /// Studio keyboard shortcuts (ignored while typing in a text field).
    fn keyboard(&mut self, ctx: &egui::Context, sh: &Shared, out: &mut Vec<StudioAction>) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        use egui::Key;
        let bar = self.timeline.tracks.first().map(|t| t.grid.bar_len()).unwrap_or(2.0);
        let total = self.timeline.total_secs;
        let mut seek: Option<f64> = None;
        let mut remove: Option<Selection> = None;
        let (mut undo, mut redo) = (false, false);
        let (cmd, shift) = ctx.input(|i| (i.modifiers.command, i.modifiers.shift));
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, Key::Space) {
                out.push(StudioAction::PlayPause);
            }
            let step = if shift { 8.0 * bar } else { bar };
            if i.key_pressed(Key::ArrowLeft) && !cmd {
                seek = Some(self.position - step);
            }
            if i.key_pressed(Key::ArrowRight) && !cmd {
                seek = Some(self.position + step);
            }
            if i.key_pressed(Key::Home) || (cmd && i.key_pressed(Key::ArrowLeft)) {
                seek = Some(0.0);
            }
            if i.key_pressed(Key::End) || (cmd && i.key_pressed(Key::ArrowRight)) {
                seek = Some(total);
            }
            if i.key_pressed(Key::F) && !cmd {
                self.follow = !self.follow;
            }
            if i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals) {
                self.zoom_around_playhead(1.5);
            }
            if i.key_pressed(Key::Minus) {
                self.zoom_around_playhead(1.0 / 1.5);
            }
            if i.key_pressed(Key::Escape) {
                if self.range.is_some() {
                    self.range = None;
                } else {
                    self.selection = Selection::None;
                    self.multi = MultiSel::default();
                }
            }
            if self.range.is_some() && cmd {
                use crate::extract::Part;
                if i.key_pressed(Key::L) {
                    self.extract_requests.push((Part::Full, false));
                }
                if shift {
                    for (key, part) in [(Key::Num1, Part::Drums), (Key::Num2, Part::Bass), (Key::Num3, Part::Melody), (Key::Num4, Part::Vocals), (Key::Num5, Part::Instrumental), (Key::Num6, Part::DrumsBass)] {
                        if i.key_pressed(key) {
                            self.extract_requests.push((part, false));
                        }
                    }
                }
            }
            if i.key_pressed(Key::Backspace) || i.key_pressed(Key::Delete) {
                remove = Some(self.selection);
            }
            if cmd && i.key_pressed(Key::S) {
                out.push(StudioAction::SaveMix);
            }
            if cmd && i.key_pressed(Key::Z) {
                if shift {
                    redo = true;
                } else {
                    undo = true;
                }
            }
        });
        if undo {
            self.undo();
        }
        if redo {
            self.redo();
        }
        match remove {
            Some(_) if !self.multi.is_empty() => self.remove_multi(sh),
            Some(Selection::Track(t)) => self.remove_track(sh, t),
            Some(Selection::Sample(j)) => self.remove_sample(j),
            _ => {}
        }
        // previous / next transition: [ and ]
        let (prev, next) = ctx.input(|i| (i.key_pressed(Key::OpenBracket), i.key_pressed(Key::CloseBracket)));
        if prev || next {
            let n = self.timeline.transitions.len();
            if n > 0 {
                let cur = match self.selection {
                    Selection::Transition(k) => k as isize,
                    _ => self.timeline.transitions.iter().position(|x| x.start_sec > self.position).map(|k| k as isize - if next { 1 } else { 0 }).unwrap_or(n as isize),
                };
                let k = (cur + if next { 1 } else { -1 }).clamp(0, n as isize - 1) as usize;
                self.selection = Selection::Transition(k);
                let x = &self.timeline.transitions[k];
                seek = Some((x.start_sec - 4.0 * bar).max(0.0));
            }
        }
        if let Some(t) = seek {
            let t = t.clamp(0.0, total.max(0.0));
            self.follow = true;
            out.push(StudioAction::Seek(t));
        }
    }

    /// Re-plan the mix for a new order, keeping the transition (and mix points) of every pair of
    /// tracks that stays adjacent when `keep_all`, else only of locked pairs.
    pub fn replan_keep(&mut self, sh: &Shared, order: &[TrackId], keep_all: bool) {
        let old = self.mix.clone();
        let mut planned = planner::plan_mix(&self.mix.name, order, &|id| analysis_of(sh, id), &self.plan_opts);
        planned.id = old.id;
        planned.tempo = old.tempo;
        planned.keylock = old.keylock;
        planned.lanes = old.lanes.clone();
        // the sample lanes aren't part of the plan
        planned.samples = old.samples.clone();
        planned.sample_lanes = old.sample_lanes.clone();
        for k in 0..planned.transitions.len() {
            let (a, b) = (planned.tracks[k].track_id, planned.tracks[k + 1].track_id);
            if let Some(j) = (0..old.transitions.len()).find(|&j| (keep_all || old.transitions[j].locked) && old.tracks[j].track_id == a && old.tracks[j + 1].track_id == b) {
                planned.tracks[k].mix_out = old.tracks[j].mix_out;
                planned.tracks[k + 1].mix_in = old.tracks[j + 1].mix_in;
                planned.tracks[k + 1].key_shift = planned.tracks[k].key_shift + (old.tracks[j + 1].key_shift - old.tracks[j].key_shift);
                planned.transitions[k] = old.transitions[j].clone();
            }
        }
        // tracks keep their own gain
        for t in planned.tracks.iter_mut() {
            if let Some(o) = old.tracks.iter().find(|o| o.track_id == t.track_id) {
                t.gain_db = o.gain_db;
            }
        }
        self.mix = planned;
        // indices into the old order
        self.multi = MultiSel::default();
        self.changed();
    }

    /// The playlist: one row per track (like DJ.Studio's playlist view).
    fn playlist_panel(&mut self, ui: &mut Ui, sh: &Shared) {
        #[derive(Clone)]
        struct RowDrag(usize);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if ui.button("+ Add tracks").on_hover_text("Search the library, listen and add tracks to the mix").clicked() {
                self.picker.show();
            }
            if ui.add_enabled(self.mix.tracks.len() > 2, egui::Button::new("✨ Harmonize")).on_hover_text("Re-order for harmonic mixing, smooth tempo and an energy arc (locked transitions stay together)").clicked() {
                self.harmonize(sh);
            }
            ui.menu_button("Order options ⏷", |ui| {
                ui.set_min_width(240.0);
                ui.label("Priority");
                ui.horizontal(|ui| {
                    ui.label("Tempo");
                    ui.add(egui::Slider::new(&mut self.order_opts.key_weight, 0.0..=1.0).show_value(false));
                    ui.label("Key");
                });
                ui.add(egui::Slider::new(&mut self.order_opts.energy_weight, 0.0..=1.0).text("Energy arc"));
                ui.checkbox(&mut self.order_opts.lock_first, "Keep first track");
                ui.checkbox(&mut self.order_opts.lock_last, "Keep last track");
            });
            if ui.add_enabled(self.mix.tracks.len() > 1, egui::Button::new("Auto mix points")).on_hover_text("Recompute mix-in/out points and transition lengths from the analysed structure (locked transitions stay)").clicked() {
                let order: Vec<TrackId> = self.mix.tracks.iter().map(|t| t.track_id).collect();
                self.replan(sh, &order);
            }
            ui.label(RichText::new(format!("{} tracks · {} · drag ☰ to reorder", self.mix.tracks.len(), theme::fmt_time_short(self.timeline.total_secs))).size(11.0).color(theme::TEXT_DIM));
        });
        ui.add_space(4.0);
        if self.mix.tracks.is_empty() {
            ui.label(RichText::new("No tracks yet: use + Add tracks, or drag tracks here from the Library.").color(theme::TEXT_DIM));
        }
        let mut moved: Option<(usize, usize)> = None;
        let mut remove: Option<usize> = None;
        let mut select: Option<Selection> = None;
        let played_key = |i: usize| self.mix.tracks.get(i).and_then(|t| sh.analyses.get(&t.track_id).and_then(|a| a.key).map(|k| k.shifted(t.key_shift)));
        let groups = link_groups(&self.mix);
        let group_color = |i: usize| groups.iter().position(|g| g.contains(&i)).map(|n| theme::LINK_GROUPS[n % theme::LINK_GROUPS.len()]);
        let playing_row = if self.playing { self.on_air_track() } else { None };
        let follow_row = playing_row.filter(|r| self.playlist_on_air != Some(*r));
        self.playlist_on_air = playing_row;
        let fixed = match self.mix.tempo {
            TempoMode::Fixed(b) => Some(b),
            TempoMode::Glide => None,
        };
        let resp = ui
            .scope(|ui| {
                let mut table = egui_extras::TableBuilder::new(ui);
                if let Some(r) = follow_row {
                    table = table.scroll_to_row(r, Some(Align::Center));
                }
                table
                    .id_salt("playlist")
                    .striped(true)
                    .sense(Sense::click())
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .column(egui_extras::Column::exact(22.0))
                    .column(egui_extras::Column::exact(14.0))
                    .column(egui_extras::Column::exact(30.0))
                    .column(egui_extras::Column::remainder().at_least(180.0).clip(true))
                    .column(egui_extras::Column::initial(150.0).clip(true))
                    .column(egui_extras::Column::initial(90.0).clip(true))
                    .column(egui_extras::Column::exact(130.0))
                    .column(egui_extras::Column::exact(52.0))
                    .column(egui_extras::Column::exact(46.0))
                    .column(egui_extras::Column::exact(46.0))
                    .column(egui_extras::Column::exact(40.0))
                    .column(egui_extras::Column::exact(70.0))
                    .column(egui_extras::Column::exact(28.0))
                    .header(20.0, |mut h| {
                        for t in ["", "", "#", "TITLE", "ARTIST", "GENRE", "TRANSITION IN", "TIME", "KEY", "MATCH", "NRG", "BPM", ""] {
                            h.col(|ui| {
                                ui.label(RichText::new(t).size(10.5).color(theme::TEXT_DIM));
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(30.0, self.mix.tracks.len(), |mut row| {
                            let i = row.index();
                            let mt = &self.mix.tracks[i];
                            let t = sh.track(mt.track_id);
                            let a = sh.analyses.get(&mt.track_id);
                            row.set_selected(self.selection == Selection::Track(i));
                            let linked = group_color(i);
                            // the row in its link group's colour, the playing track in the accent;
                            // per cell, as each cell paints its own background
                            let tint_color = if playing_row == Some(i) { Some(theme::accent().linear_multiply(0.18)) } else { linked.map(|c| c.linear_multiply(0.2)) };
                            let tint = |ui: &Ui| {
                                if let Some(c) = tint_color {
                                    let r = cell_area(ui);
                                    egui::Painter::new(ui.ctx().clone(), ui.layer_id(), r).rect_filled(r, 0.0, c);
                                }
                            };
                            row.col(|ui| {
                                tint(ui);
                                let tip = if linked.is_some() { "Drag to reorder (with the tracks linked to it)" } else { "Drag to reorder" };
                                let h = ui.add(egui::Label::new(RichText::new("☰").size(13.0).color(theme::TEXT_DIM)).sense(Sense::drag())).on_hover_text(tip);
                                if h.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                                }
                                h.dnd_set_drag_payload(RowDrag(i));
                            });
                            // a line through tracks linked by locked transitions (they stay together)
                            row.col(|ui| {
                                tint(ui);
                                let up = i > 0 && self.mix.transitions.get(i - 1).is_some_and(|t| t.locked);
                                let down = self.mix.transitions.get(i).is_some_and(|t| t.locked);
                                let Some(color) = linked else { return };
                                let r = cell_area(ui);
                                let p = egui::Painter::new(ui.ctx().clone(), ui.layer_id(), r);
                                let (x, cy) = (r.center().x, ui.max_rect().center().y);
                                let stroke = Stroke::new(2.0, color);
                                if up {
                                    p.line_segment([pos2(x, r.top()), pos2(x, cy)], stroke);
                                }
                                if down {
                                    p.line_segment([pos2(x, cy), pos2(x, r.bottom())], stroke);
                                }
                                p.circle_filled(pos2(x, cy), 3.5, color);
                                let tip = match (up, down) {
                                    (false, false) => "Linked",
                                    (true, true) => "Linked to the tracks before and after it by locked transitions",
                                    (true, false) => "Linked to the track before it by a locked transition",
                                    _ => "Linked to the track after it by a locked transition",
                                };
                                ui.allocate_exact_size(ui.available_size(), Sense::hover()).1.on_hover_text(format!("{tip}: they stay together when you drag, harmonize or re-plan the mix"));
                            });
                            row.col(|ui| {
                                tint(ui);
                                if playing_row == Some(i) {
                                    playing_bars(ui).on_hover_text("Playing");
                                } else {
                                    ui.label(RichText::new(format!("{}", i + 1)).color(theme::TEXT_DIM));
                                }
                            });
                            row.col(|ui| {
                                tint(ui);
                                ui.label(t.map(|t| if t.title.is_empty() { t.display_name() } else { t.title.clone() }).unwrap_or_default());
                            });
                            row.col(|ui| {
                                tint(ui);
                                ui.label(RichText::new(t.map(|t| t.artist.clone()).unwrap_or_default()).color(theme::TEXT_DIM));
                            });
                            row.col(|ui| {
                                tint(ui);
                                ui.label(RichText::new(t.map(|t| t.genre.clone()).unwrap_or_default()).color(theme::TEXT_DIM).size(11.5));
                            });
                            row.col(|ui| {
                                tint(ui);
                                if i > 0 {
                                    if let Some(tr) = self.mix.transitions.get(i - 1) {
                                        let c = theme::style_color(tr.style);
                                        let txt = format!("{}{} · {}", if tr.locked { "🔒 " } else { "" }, tr.style.label(), tr.beats / 4);
                                        if ui.add(egui::Button::new(RichText::new(txt).size(11.5).color(Color32::WHITE)).fill(theme::blend(theme::PANEL, c, 0.5)).min_size(vec2(120.0, 20.0))).on_hover_text("Edit this transition").clicked() {
                                            select = Some(Selection::Transition(i - 1));
                                        }
                                    }
                                }
                            });
                            row.col(|ui| {
                                tint(ui);
                                ui.label(RichText::new(theme::fmt_time_short(a.map(|a| a.duration).unwrap_or(0.0))).monospace().size(12.0));
                            });
                            row.col(|ui| {
                                tint(ui);
                                if let Some(k) = played_key(i) {
                                    key_chip(ui, k);
                                }
                            });
                            row.col(|ui| {
                                tint(ui);
                                if let (true, Some(prev), Some(cur)) = (i > 0, played_key(i.wrapping_sub(1)), played_key(i)) {
                                    let c = prev.compatibility(cur);
                                    let (txt, col, tip) = if c >= 0.85 {
                                        ("ok", theme::OK, "Harmonic with the previous track")
                                    } else if c >= 0.5 {
                                        ("boost", theme::WARN, "Acceptable: energy boost / diagonal move")
                                    } else {
                                        ("clash", Color32::from_rgb(255, 90, 90), "Key clash with the previous track: try a key shift (Track tab)")
                                    };
                                    let (r, resp) = ui.allocate_exact_size(vec2(40.0, 20.0), Sense::hover());
                                    ui.painter().rect_stroke(r, 4.0, Stroke::new(1.5, col), StrokeKind::Inside);
                                    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, txt, egui::FontId::proportional(10.5), col);
                                    resp.on_hover_text(tip);
                                }
                            });
                            row.col(|ui| {
                                tint(ui);
                                if let Some(e) = a.and_then(|a| a.energy) {
                                    ui.label(RichText::new(format!("{e}")).color(theme::WARN));
                                }
                            });
                            row.col(|ui| {
                                tint(ui);
                                if let Some(b) = a.and_then(|a| a.bpm()) {
                                    ui.label(RichText::new(format!("{b:.0}")).monospace());
                                    if let Some(f) = fixed {
                                        let pct = (f / b - 1.0) * 100.0;
                                        if pct.abs() >= 0.5 {
                                            ui.label(RichText::new(format!("{pct:+.0}%")).size(10.5).color(theme::TEXT_DIM));
                                        }
                                    }
                                }
                            });
                            row.col(|ui| {
                                tint(ui);
                                if ui.add(egui::Button::new(RichText::new("🗑").size(13.0)).frame(false)).on_hover_text("Remove from the mix").clicked() {
                                    remove = Some(i);
                                }
                            });
                            let r = row.response();
                            if let Some(p) = r.dnd_release_payload::<RowDrag>() {
                                moved = Some((p.0, i));
                            }
                            if r.dnd_hover_payload::<RowDrag>().is_some() {
                                ui_line(&r);
                            }
                            if r.clicked() {
                                select = Some(Selection::Track(i));
                            }
                        });
                    });
            })
            .response;
        if let Some(p) = resp.dnd_release_payload::<DraggedTracks>() {
            self.add_tracks(sh, &p.0);
        }
        if let Some(sel) = select {
            self.selection = sel;
            if matches!(sel, Selection::Track(_)) {
                self.last_selection = sel; // stay in the playlist
            }
        }
        if let Some((from, to)) = moved.filter(|(f, t)| f != t) {
            self.move_track(sh, from, to);
        } else if let Some(i) = remove {
            self.remove_track(sh, i);
        }
    }

    /// Move track `from`, with the tracks linked to it by locked transitions, to where track
    /// `to` is. A drop inside another linked group lands next to that group, so no locked pair is
    /// split.
    pub fn move_track(&mut self, sh: &Shared, from: usize, to: usize) {
        let groups = link_groups(&self.mix);
        let span = |i: usize| groups.iter().find(|g| g.contains(&i)).cloned().unwrap_or(i..=i);
        let (block, target) = (span(from), span(to));
        if from >= self.mix.tracks.len() || to >= self.mix.tracks.len() || block.contains(&to) {
            return;
        }
        let mut order: Vec<TrackId> = self.mix.tracks.iter().map(|t| t.track_id).collect();
        let moved: Vec<TrackId> = order.drain(block.clone()).collect();
        // moving down the block ends where the target ends, moving up it starts where it starts
        let at = if to > *block.end() { target.end() + 1 - moved.len() } else { *target.start() };
        order.splice(at..at, moved);
        self.selection = Selection::None;
        self.replan_keep(sh, &order, true);
    }

    /// The track on air at the playhead: the incoming one from the middle of a transition.
    fn on_air_track(&self) -> Option<usize> {
        let i = self.track_at_playhead()?;
        match i.checked_sub(1).and_then(|k| self.timeline.transitions.get(k)) {
            Some(x) if self.position < (x.start_sec + x.end_sec) * 0.5 => Some(i - 1),
            _ => Some(i),
        }
    }

    /// Delete everything in the multi-selection: its sample clips, and its tracks (the
    /// transitions around them are planned again).
    pub fn remove_multi(&mut self, sh: &Shared) {
        let multi = std::mem::take(&mut self.multi);
        self.selection = Selection::None;
        for j in multi.samples.iter().rev() {
            if *j < self.mix.samples.len() {
                self.mix.samples.remove(*j);
            }
        }
        if multi.tracks.is_empty() {
            self.changed();
            return;
        }
        let order: Vec<TrackId> = self.mix.tracks.iter().enumerate().filter(|(i, _)| !multi.tracks.contains(i)).map(|(_, t)| t.track_id).collect();
        self.replan_keep(sh, &order, true);
    }

    /// Delete sample clip `j`.
    pub fn remove_sample(&mut self, j: usize) {
        if j >= self.mix.samples.len() {
            return;
        }
        self.mix.samples.remove(j);
        self.multi = MultiSel::default();
        self.selection = match self.selection {
            Selection::Sample(s) if s == j => Selection::None,
            Selection::Sample(s) if s > j => Selection::Sample(s - 1),
            other => other,
        };
        self.changed();
    }

    /// Take the `i`-th track out of the mix; the transitions around it are planned again.
    pub fn remove_track(&mut self, sh: &Shared, i: usize) {
        let mut order: Vec<TrackId> = self.mix.tracks.iter().map(|t| t.track_id).collect();
        if i >= order.len() {
            return;
        }
        order.remove(i);
        self.selection = Selection::None;
        self.replan_keep(sh, &order, true);
    }

    /// Start the extractions asked for (menu / shortcuts) and place finished ones on a sample lane.
    fn run_extractions(&mut self, sh: &Shared) {
        for (part, save) in std::mem::take(&mut self.extract_requests) {
            let Some(rs) = self.range.filter(|r| r.b1 > r.b0 && r.track < self.mix.tracks.len()) else { continue };
            let i = rs.track;
            let mt = self.mix.tracks[i].clone();
            let Some(t) = sh.track(mt.track_id) else { continue };
            let tt = &self.timeline.tracks[i];
            let (t0, t1) = (self.timeline.track_pos(i, rs.b0), self.timeline.track_pos(i, rs.b1));
            let rate = self.timeline.bpm_at_beat((rs.b0 + rs.b1) * 0.5) / tt.grid.bpm.max(1.0);
            let semis = mt.key_shift as f32 + if self.mix.keylock { 0.0 } else { 12.0 * (rate as f32).log2() };
            let bar = |b: f64| ((b - tt.start_beat + tt.track_beat0) / tt.grid.beats_per_bar as f64).floor() as i64 + 1;
            let title = if t.title.is_empty() { t.display_name() } else { t.title.clone() };
            let name: String = format!("{} – bars {}-{} – {}", title, bar(rs.b0), bar(rs.b1) - 1, part.label()).chars().map(|c| if "/:\\".contains(c) { '-' } else { c }).collect();
            let out = if save {
                let dir = clubcraft_core::paths::data_dir().join("Samples");
                let _ = std::fs::create_dir_all(&dir);
                match rfd::FileDialog::new().set_directory(&dir).set_file_name(format!("{name}.wav")).add_filter("WAV", &["wav"]).save_file() {
                    Some(p) => p,
                    None => continue,
                }
            } else {
                clubcraft_core::paths::data_dir().join("Samples").join(format!("{name}.wav"))
            };
            let req = crate::extract::Request { audio_path: t.path.clone(), hash: t.hash.clone(), t0, t1, rate, semis, weights: part.weights(), out };
            let source = clubcraft_core::SampleSource { audio_path: t.path.clone(), hash: t.hash.clone(), stems: part.weights(), t0, track_secs: t.duration, rate, semis, model: None };
            // a copy starts at the spot it was cut from, so it stays there when the track moves
            let lock = clubcraft_core::SampleLock { track_id: mt.track_id, pos: t0 };
            let then = if save { ExtractThen::Saved } else { ExtractThen::Place(rs.b0, source, Some(lock)) };
            self.start_extraction(req, then, format!("{} of {}", part.label(), title));
        }
        while let Ok(d) = self.extract_rx.try_recv() {
            self.extracting = self.extracting.saturating_sub(1);
            match (d.result, d.then) {
                (Ok((path, secs, model)), ExtractThen::Recut { old, s0, source, better }) => {
                    self.recutting.remove(&old);
                    let rank = |m: &Option<String>| m.as_deref().map_or(0, crate::pipeline::model_rank);
                    if better && rank(&model) <= rank(&source.model) {
                        self.recut_failed.insert(path.clone());
                    }
                    let source = clubcraft_core::SampleSource { model, ..source };
                    for c in self.mix.samples.iter_mut().filter(|c| c.path == old) {
                        c.path = path.clone();
                        c.source = Some(source.clone());
                        c.offset -= s0;
                        if c.offset.abs() < 1e-3 && c.length.is_some_and(|l| (l - secs).abs() < 1e-3) {
                            (c.offset, c.length) = (0.0, None);
                        }
                    }
                    // dragged further while it was being cut
                    for j in 0..self.mix.samples.len() {
                        if self.mix.samples[j].path == path {
                            self.recut_sample(j, secs, false);
                        }
                    }
                    self.message = format!("{} extended", d.label);
                    self.changed();
                }
                (Err(e), ExtractThen::Recut { old, .. }) => {
                    self.recutting.remove(&old);
                    self.recut_failed.insert(old);
                    self.message = format!("Couldn't extend {}: {e}", d.label);
                }
                (Ok((path, secs, model)), ExtractThen::Place(beat, source, lock)) => {
                    // first sample lane that is free over the clip's span
                    let end = self.timeline.beat_at_sec(self.timeline.sec_at_beat(beat) + secs);
                    let busy = |l: u8, s: &StudioView| {
                        s.mix.samples.iter().any(|c| {
                            let len = c.secs(s.sample_cache.get(&c.path).and_then(|x| x.as_ref().map(|x| x.0)).unwrap_or(4.0));
                            let ce = s.timeline.beat_at_sec(s.timeline.sec_at_beat(c.start_beat) + len);
                            c.lane == l && c.start_beat < end && ce > beat
                        })
                    };
                    let lane = (0..clubcraft_core::SAMPLE_LANES as u8).find(|&l| !busy(l, self)).unwrap_or(0);
                    let source = clubcraft_core::SampleSource { model, ..source };
                    self.mix.samples.push(clubcraft_core::SampleClip { source: Some(source), lock, ..clubcraft_core::SampleClip::new(lane, path, beat) });
                    self.sample_open[lane as usize] = true;
                    self.message = format!("{} copied to Sample {}", d.label, lane + 1);
                    self.changed();
                }
                (Ok((path, _, _)), ExtractThen::Saved) => self.message = format!("Saved {}", path.display()),
                (Err(e), _) => self.message = format!("Extract failed: {e}"),
            }
        }
    }

    fn start_extraction(&mut self, req: crate::extract::Request, then: ExtractThen, label: String) {
        let tx = self.extract_tx.clone();
        self.extracting += 1;
        self.message = format!("Extracting {label}…");
        std::thread::spawn(move || {
            let result = crate::extract::run(&req).map(|(secs, model)| (req.out.clone(), secs, model)).map_err(|e| format!("{e:#}"));
            let _ = tx.send(ExtractDone { result, then, label });
        });
    }

    /// Cut sample clip `j` (its file `file_secs` long) again from its track: when it was dragged
    /// past its file, so it continues the track instead of falling silent, or (`force`) to take
    /// better stems.
    pub(crate) fn recut_sample(&mut self, j: usize, file_secs: f64, force: bool) {
        let Some(c) = self.mix.samples.get(j) else { return };
        let Some(src) = c.source.clone() else { return };
        let (lo, hi) = src.file_bounds();
        let (s0, s1) = (c.offset.max(lo), (c.offset + c.secs(file_secs)).min(hi));
        let inside = s0 > -1e-4 && s1 < file_secs + 1e-4;
        if (inside && !force) || s1 <= s0 || self.recutting.contains(&c.path) {
            return;
        }
        let (t0, t1) = src.track_range(c.offset, c.secs(file_secs));
        // a new name: the engine and the timeline cache files by path
        let stem = c.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let base = stem.split(" @").next().unwrap_or(&stem).to_string();
        let name = |n: u32| c.path.with_file_name(format!("{base} @{t0:.2}-{t1:.2}s{}.wav", if n > 0 { format!(" ({n})") } else { String::new() }));
        let Some(out) = (0..1000).map(name).find(|p| !p.exists()) else { return };
        let req = crate::extract::Request { audio_path: src.audio_path.clone(), hash: src.hash.clone(), t0, t1, rate: src.rate, semis: src.semis, weights: src.stems, out };
        let old = c.path.clone();
        self.recutting.insert(old.clone());
        let source = clubcraft_core::SampleSource { t0, ..src };
        self.start_extraction(req, ExtractThen::Recut { old, s0, source, better: force }, base);
    }

    /// The parts of tracks (track seconds) that sample clips were cut from as stems.
    pub(crate) fn sample_stem_regions(&self, sh: &Shared) -> Vec<(TrackId, Vec<(f64, f64)>)> {
        let mut out: Vec<(TrackId, Vec<(f64, f64)>)> = Vec::new();
        for c in &self.mix.samples {
            let Some(src) = c.source.as_ref().filter(|s| s.stems.is_some()) else { continue };
            let Some(id) = sh.tracks.iter().find(|t| t.hash == src.hash).map(|t| t.id) else { continue };
            let file_secs = self.sample_cache.get(&c.path).and_then(|x| x.as_ref().map(|x| x.0)).unwrap_or(0.0);
            let r = src.track_range(c.offset, c.secs(file_secs));
            match out.iter_mut().find(|e| e.0 == id) {
                Some(e) => e.1.push(r),
                None => out.push((id, vec![r])),
            }
        }
        out
    }

    /// Model of better stems than sample clip `c` was cut from, now there for its part of the
    /// track (complete stems, or parts separated for the mix or an export).
    fn better_stems_for(&self, sh: &Shared, c: &clubcraft_core::SampleClip) -> Option<String> {
        let src = c.source.as_ref().filter(|s| s.stems.is_some())?;
        let id = sh.tracks.iter().find(|t| t.hash == src.hash)?.id;
        let have = src.model.as_deref().map_or(0, crate::pipeline::model_rank);
        let full = sh.analyses.get(&id).and_then(|a| a.stems_model.clone());
        let file_secs = self.sample_cache.get(&c.path).and_then(|x| x.as_ref().map(|x| x.0)).unwrap_or(0.0);
        let (a, b) = src.track_range(c.offset, c.secs(file_secs));
        let covered = sh.stem_regions.get(&id).is_some_and(|r| r.iter().any(|&(x, y)| x <= a + 1e-6 && y >= b - 1e-6));
        let parts = covered.then(|| sh.stem_regions_model.get(&id).cloned()).flatten();
        let model = [full, parts].into_iter().flatten().max_by_key(|m| crate::pipeline::model_rank(m))?;
        (crate::pipeline::model_rank(&model) > have).then_some(model)
    }

    /// Look for better stems for the linked sample clips on the next tick.
    pub(crate) fn check_linked_samples(&mut self) {
        self.linked_check = true;
    }

    /// Cut clips again whose track has better stems by now, when asked to look.
    fn refresh_linked_samples(&mut self, sh: &Shared) {
        if !std::mem::take(&mut self.linked_check) {
            return;
        }
        for j in 0..self.mix.samples.len() {
            let path = self.mix.samples[j].path.clone();
            // the file's length, read now if the timeline hasn't drawn the clip yet
            let Some(file_secs) = self.sample_info(&path).map(|x| x.0) else { continue };
            let c = &self.mix.samples[j];
            if self.recut_failed.contains(&c.path) || self.better_stems_for(sh, c).is_none() {
                continue;
            }
            self.recut_sample(j, file_secs, true);
        }
    }

    /// Extractions and the clips linked to their tracks' stems, also while the studio isn't shown.
    pub(crate) fn tick_samples(&mut self, sh: &Shared) {
        self.run_extractions(sh);
        self.refresh_linked_samples(sh);
    }

    /// Whether linked clips are still being cut again (or have better stems waiting): an export
    /// waits for them.
    pub(crate) fn linked_samples_waiting(&self, sh: &Shared) -> bool {
        !self.recutting.is_empty() || self.mix.samples.iter().any(|c| !self.recut_failed.contains(&c.path) && self.sample_cache.get(&c.path).is_some_and(|x| x.is_some()) && self.better_stems_for(sh, c).is_some())
    }

    /// Put the clips locked to a track back at their place in it, after the timeline changed.
    fn apply_sample_locks(&mut self) {
        let mut moved = false;
        for c in &mut self.mix.samples {
            let Some(lock) = c.lock else { continue };
            let Some(i) = self.mix.tracks.iter().position(|t| t.track_id == lock.track_id).filter(|&i| i < self.timeline.tracks.len()) else { continue };
            let b = self.timeline.beat_of_track_pos(i, lock.pos);
            if b.is_finite() && (b - c.start_beat).abs() > 1e-6 {
                c.start_beat = b.max(0.0);
                moved = true;
            }
        }
        if moved {
            // part of the edit that moved the track: no undo step of its own
            self.committed = self.mix.clone();
            self.dirty = true;
            self.edited = true;
        }
    }

    /// Sample clip `j` moved: a locked one keeps its new place in its track.
    pub(crate) fn relock_sample(&mut self, j: usize) {
        let Some(c) = self.mix.samples.get(j) else { return };
        let Some(lock) = c.lock else { return };
        let Some(i) = self.mix.tracks.iter().position(|t| t.track_id == lock.track_id).filter(|&i| i < self.timeline.tracks.len()) else { return };
        let pos = self.timeline.track_pos(i, c.start_beat);
        if let Some(l) = self.mix.samples[j].lock.as_mut() {
            l.pos = pos;
        }
    }

    pub(crate) fn track_at_playhead(&self) -> Option<usize> {
        let n = self.timeline.tracks.len();
        (n > 0).then(|| self.timeline.tracks.iter().rposition(|t| t.start_sec <= self.position + 1e-6).unwrap_or(0).min(n - 1))
    }

    /// When the playhead enters a transition (playing or seeking), select it, which opens its
    /// settings. Only on entering: inside, the user may close the panel or select something else.
    /// Not while a mouse button is held, so a drag or curve edit keeps its selection.
    fn follow_playhead_into_transitions(&mut self, ctx: &egui::Context) {
        let pos = self.position;
        let now = self.timeline.transitions.iter().position(|t| t.start_sec <= pos && pos < t.end_sec);
        if now == self.playhead_transition || ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        self.playhead_transition = now;
        if let Some(k) = now.filter(|&k| k + 1 < self.mix.tracks.len()) {
            self.selection = Selection::Transition(k);
        }
    }

    fn transition_at_playhead(&self) -> Option<usize> {
        let n = self.timeline.transitions.len();
        (n > 0).then(|| self.timeline.transitions.iter().position(|t| t.end_sec >= self.position).unwrap_or(n - 1))
    }

    pub fn toggle_tab(&mut self, t: Tab) {
        if self.tab == Some(t) {
            self.tab = None;
            return;
        }
        self.tab = Some(t);
        match t {
            Tab::Track if !matches!(self.selection, Selection::Track(_)) => {
                if let Some(i) = self.track_at_playhead() {
                    self.selection = Selection::Track(i);
                }
            }
            Tab::Transition if !matches!(self.selection, Selection::Transition(_)) => {
                if let Some(k) = self.transition_at_playhead() {
                    self.selection = Selection::Transition(k);
                }
            }
            Tab::Mashups if !self.mashup.open => {
                let id = match self.selection {
                    Selection::Track(i) => self.mix.tracks.get(i).map(|t| t.track_id),
                    _ => self.track_at_playhead().and_then(|i| self.mix.tracks.get(i)).map(|t| t.track_id),
                };
                self.pending.push(StudioAction::FindMashups(id));
            }
            _ => {}
        }
        self.last_selection = self.selection;
    }

    fn tab_bar(&mut self, ui: &mut Ui) {
        // tabs sized to their label, the group centred exactly
        let full = ui.available_rect_before_wrap();
        let font = egui::FontId::proportional(14.0);
        let (pad, icon_w, gap) = (16.0, 18.0, 10.0);
        let widths: Vec<f32> = Tab::ALL.iter().map(|t| pad + icon_w + 6.0 + ui.fonts_mut(|f| f.layout_no_wrap(t.label().1.to_string(), font.clone(), Color32::WHITE).size().x) + pad).collect();
        let total = widths.iter().sum::<f32>() + gap * (widths.len() as f32 - 1.0);
        let mut x = full.center().x - total / 2.0;
        for (t, w) in Tab::ALL.into_iter().zip(widths) {
            let r = egui::Rect::from_min_size(egui::pos2(x, full.center().y - 15.0), vec2(w, 30.0));
            x += w + gap;
            let resp = ui.interact(r, egui::Id::new(("studio-tab", t as u8)), Sense::click());
            let active = self.tab == Some(t);
            let p = ui.painter();
            if active {
                p.rect_filled(r, 6.0, theme::SELECT);
                p.rect_stroke(r, 6.0, Stroke::new(1.5, theme::accent()), StrokeKind::Inside);
            } else if resp.hovered() {
                p.rect_filled(r, 6.0, theme::PANEL_HI);
            }
            let (icon, name) = t.label();
            let col = if active { theme::accent() } else if resp.hovered() { theme::TEXT } else { theme::TEXT_DIM };
            p.text(egui::pos2(r.left() + pad + icon_w / 2.0, r.center().y), egui::Align2::CENTER_CENTER, icon, font.clone(), col);
            p.text(egui::pos2(r.left() + pad + icon_w + 6.0, r.center().y), egui::Align2::LEFT_CENTER, name, font.clone(), col);
            if resp.on_hover_text(if active { "Close this panel" } else { "Open this panel" }).clicked() {
                self.toggle_tab(t);
            }
        }
        ui.allocate_rect(full, Sense::hover());
    }

    /// Transport row: mixes / edit menus on the left, transport centred, output on the right.
    /// Tracks whose stem automation (transition stem lanes or their own lanes) plays without stems.
    pub(crate) fn tracks_missing_stems(&self, sh: &Shared) -> Vec<TrackId> {
        self.tracks_needing_stems(sh, None)
    }

    /// Tracks whose stem automation needs stems they don't have: none at all, or (with
    /// `quality`) only weaker ones than that quality.
    pub(crate) fn tracks_needing_stems(&self, sh: &Shared, quality: Option<&str>) -> Vec<TrackId> {
        // for playback, the parts of the tracks the stem automation plays in are enough
        let Some(quality) = quality else { return self.missing_stem_regions(sh).into_iter().map(|e| e.0).collect() };
        let mut ids: Vec<TrackId> = Vec::new();
        for (i, t) in self.mix.tracks.iter().enumerate() {
            let uses = !t.stems.is_empty() || (i > 0 && self.mix.transitions.get(i - 1).is_some_and(|x| x.uses_stems())) || self.mix.transitions.get(i).is_some_and(|x| x.uses_stems());
            let model = sh.analyses.get(&t.track_id).and_then(|a| a.stems_model.as_deref());
            let has = crate::pipeline::stems_meet(model, quality);
            if uses && !has && !ids.contains(&t.track_id) {
                ids.push(t.track_id);
            }
        }
        ids
    }

    /// Savepoints: keep the current state under a name, go back or forward to any of them.
    fn savepoints_menu(&mut self, ui: &mut Ui, has: bool, compact: bool, out: &mut Vec<StudioAction>) {
        let n = self.savepoints.len();
        let label = match (compact, n) {
            (true, 0) => "⏺ ⏷".to_string(),
            (true, n) => format!("⏺ {n} ⏷"),
            (false, 0) => "⏺ Savepoints ⏷".to_string(),
            (false, n) => format!("⏺ Savepoints ({n}) ⏷"),
        };
        ui.menu_button(label, |ui| {
            ui.set_min_width(300.0);
            ui.horizontal(|ui| {
                let hint = format!("Savepoint {}", n + 1);
                ui.add(egui::TextEdit::singleline(&mut self.savepoint_name).hint_text(hint).desired_width(170.0));
                if ui.add_enabled(has, egui::Button::new("Save point")).on_hover_text("Keep the mix as it is now").clicked() {
                    out.push(StudioAction::AddSavepoint(std::mem::take(&mut self.savepoint_name)));
                    ui.close();
                }
            });
            if n > 0 {
                ui.separator();
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    // newest first
                    for (id, name, at) in self.savepoints.iter().rev() {
                        ui.horizontal(|ui| {
                            let here = self.at_savepoint == Some(*id);
                            let text = format!("{}{name}", if here { "● " } else { "" });
                            let tip = if here { "The mix is at this savepoint" } else { "Go to this savepoint (the current state is kept as a savepoint first)" };
                            if ui.add_sized([200.0, 20.0], egui::Button::selectable(here, text)).on_hover_text(tip).clicked() && !here {
                                out.push(StudioAction::RestoreSavepoint(*id));
                                ui.close();
                            }
                            ui.label(RichText::new(crate::home::fmt_date(*at)).size(11.0).color(theme::TEXT_DIM));
                            if ui.small_button("🗑").on_hover_text("Delete this savepoint").clicked() {
                                out.push(StudioAction::DeleteSavepoint(*id));
                            }
                        });
                    }
                });
            }
            ui.separator();
            ui.label(RichText::new("Going to a savepoint keeps all the others, so you can switch back and forth.").size(11.0).color(theme::TEXT_DIM));
        });
    }

    fn transport(&mut self, ui: &mut Ui, sh: &Shared, mixes: &[(i64, String, i64)], out: &mut Vec<StudioAction>, load_mix: &mut Option<i64>, new_mix: &mut bool) {
        let full = ui.available_rect_before_wrap();
        let has = !self.mix.tracks.is_empty();
        let compact = full.width() < self.transport_full_w;
        // ---- left: mixes, name, edit menu, undo / redo
        let mut left = ui.new_child(egui::UiBuilder::new().max_rect(full).layout(Layout::left_to_right(Align::Center)));
        left.spacing_mut().item_spacing.x = 6.0;
        left.menu_button(RichText::new("☰ Mixes").size(13.0), |ui| {
            ui.set_min_width(260.0);
            if ui.button("+ New mix").clicked() {
                *new_mix = true;
                ui.close();
            }
            if ui.add_enabled(has, egui::Button::new("Duplicate this mix")).on_hover_text("Save a copy of this mix and open it, to try something out").clicked() {
                out.push(StudioAction::DuplicateMix);
                ui.close();
            }
            ui.separator();
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for (id, name, _) in mixes {
                    if ui.selectable_label(self.mix.id == *id, name).clicked() {
                        if self.mix.id != *id {
                            *load_mix = Some(*id);
                        }
                        ui.close();
                    }
                }
            });
            ui.separator();
            if ui.button("Import DJ.Studio projects").clicked() {
                out.push(StudioAction::ImportDjStudio);
                ui.close();
            }
            if ui.button("Export cues to VirtualDJ…").clicked() {
                out.push(StudioAction::ExportVirtualDj);
                ui.close();
            }
        });
        if left.add(egui::TextEdit::singleline(&mut self.mix.name).font(egui::FontId::proportional(15.0)).frame(egui::Frame::NONE).desired_width(if compact { 110.0 } else { 190.0 })).changed() {
            self.dirty = true;
        }
        if self.dirty {
            if left.small_button("Save").on_hover_text("⌘S").clicked() {
                out.push(StudioAction::SaveMix);
            }
        } else {
            left.label(RichText::new("saved").size(11.0).color(theme::TEXT_DIM));
        }
        self.savepoints_menu(&mut left, has, compact, out);
        left.menu_button("Edit ⏷", |ui| {
            ui.set_min_width(240.0);
            if ui.add_enabled(self.mix.tracks.len() > 2, egui::Button::new("✨ Harmonize order")).on_hover_text("Re-order for harmonic mixing, smooth tempo and an energy arc").clicked() {
                self.pending.push(StudioAction::Harmonize);
                ui.close();
            }
            if ui.add_enabled(self.mix.tracks.len() > 1, egui::Button::new("Auto mix points")).on_hover_text("Recompute mix-in/out points and transition lengths from the analysed structure").clicked() {
                self.pending.push(StudioAction::AutoMixPoints);
                ui.close();
            }
            ui.separator();
            if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Undo  ⌘Z")).clicked() {
                self.undo();
                ui.close();
            }
            if ui.add_enabled(!self.redo.is_empty(), egui::Button::new("Redo  Shift+⌘Z")).clicked() {
                self.redo();
                ui.close();
            }
        });
        // (compact: undo / redo stay in the Edit menu and on ⌘Z)
        if !compact {
            if left.add_enabled(!self.undo.is_empty(), egui::Button::new("↩").min_size(vec2(28.0, 26.0))).on_hover_text("Undo (⌘Z)").clicked() {
                self.undo();
            }
            if left.add_enabled(!self.redo.is_empty(), egui::Button::new("↪").min_size(vec2(28.0, 26.0))).on_hover_text("Redo (Shift+⌘Z)").clicked() {
                self.redo();
            }
        }
        // ---- right: output
        let mut right = ui.new_child(egui::UiBuilder::new().max_rect(full).layout(Layout::right_to_left(Align::Center)));
        right.spacing_mut().item_spacing.x = 6.0;
        if let Some(p) = self.render_progress {
            right.add(egui::ProgressBar::new(p).desired_width(140.0).text(if self.render_label.is_empty() { "Rendering…" } else { self.render_label.as_str() }));
        } else if right.add_enabled(has, egui::Button::new("Export…")).on_hover_text("Render the mix to WAV with a cue sheet").clicked() {
            out.push(StudioAction::Export);
        }
        if right.add_enabled(has, egui::Button::new("Play live ▶")).on_hover_text("Play this mix on the live decks").clicked() {
            out.push(StudioAction::SendToLive);
        }
        let missing = self.tracks_missing_stems(sh);
        if !missing.is_empty() {
            let text = if compact { format!("⚠ {}", missing.len()) } else { format!("Stems missing ({})", missing.len()) };
            let b = egui::Button::new(RichText::new(text).color(theme::WARN).size(12.0));
            if right.add(b).on_hover_text("These tracks have stem automation (in transitions or on the track) but no stems yet, so it is imitated with volume and EQ. Click to separate their stems for the real sound.").clicked() {
                out.push(StudioAction::SeparateStems(missing));
            }
        }
        // (the status message is drawn below, where it fits; it doesn't count as width the row needs)
        let right_w = right.min_rect().width();
        // ---- centre: transport, placed so the play button is in the middle of the window (by
        // last frame's measurements), but moved right of the left group when that reaches into
        // it (and kept off the right group while there is room)
        let cw = self.transport_w.max(100.0);
        let gap = 12.0;
        let (left_end, right_start) = (left.min_rect().right() + gap, right.min_rect().left() - gap);
        let cx = (full.center().x - self.play_offset).min(right_start - cw).max(left_end);
        let crect = egui::Rect::from_min_size(egui::pos2(cx, full.top()), vec2(cw + 8.0, full.height()));
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(crect).layout(Layout::left_to_right(Align::Center)));
        c.spacing_mut().item_spacing.x = 8.0;
        let round = |t: &str| egui::Button::new(RichText::new(t).size(15.0)).corner_radius(17.0).min_size(vec2(34.0, 34.0));
        if c.add(round("⏮")).on_hover_text("To start (Home)").clicked() {
            out.push(StudioAction::Seek(0.0));
        }
        if c.add_enabled(!self.timeline.transitions.is_empty(), round("⏪")).on_hover_text("Previous transition ([)").clicked() {
            let k = self.timeline.transitions.iter().rposition(|t| t.start_sec < self.position - 0.5);
            if let Some(k) = k {
                self.selection = Selection::Transition(k);
                out.push(StudioAction::Seek(self.timeline.transitions[k].start_sec));
            }
        }
        let play = crate::widgets::play_button(&mut c, self.playing, has, 40.0);
        self.play_offset = play.rect.center().x - crect.left();
        if play.on_hover_text("Play / pause (Space)").clicked() {
            out.push(StudioAction::PlayPause);
        }
        if c.add_enabled(!self.timeline.transitions.is_empty(), round("⏩")).on_hover_text("Next transition (])").clicked() {
            if let Some(k) = self.timeline.transitions.iter().position(|t| t.start_sec > self.position + 0.5) {
                self.selection = Selection::Transition(k);
                out.push(StudioAction::Seek(self.timeline.transitions[k].start_sec));
            }
        }
        if c.add(round("⏭")).on_hover_text("To end (End)").clicked() {
            out.push(StudioAction::Seek(self.timeline.total_secs));
        }
        c.add_space(6.0);
        c.label(RichText::new(theme::fmt_time(self.position)).monospace().size(16.0).strong());
        c.label(RichText::new(format!("/ {}", theme::fmt_time(self.timeline.total_secs))).monospace().color(theme::TEXT_DIM));
        c.label(RichText::new(format!("{:.1} BPM", self.timeline.bpm_at_beat(self.timeline.beat_at_sec(self.position)))).monospace().color(theme::WARN));
        if crate::widgets::toggle(&mut c, self.follow, "Follow", theme::accent(), vec2(56.0, 22.0)).on_hover_text("Keep the playhead in view (F)").clicked() {
            self.follow = !self.follow;
        }
        self.transport_w = c.min_rect().width();
        // status message between the transport and the right group, when it fits there
        if !self.message.is_empty() {
            let text = RichText::new(&self.message).color(theme::TEXT_DIM).size(11.0);
            let w = ui.fonts_mut(|f| f.layout_no_wrap(self.message.clone(), egui::FontId::proportional(11.0), theme::TEXT_DIM).size().x);
            let free = egui::Rect::from_x_y_ranges((cx + cw + gap)..=(right_start), full.y_range());
            if free.width() >= w {
                let mut m = ui.new_child(egui::UiBuilder::new().max_rect(free).layout(Layout::right_to_left(Align::Center)));
                m.label(text);
            }
        }
        if !compact {
            self.transport_full_w = left.min_rect().width() + self.transport_w + right_w + 4.0 * gap;
        }
        ui.allocate_rect(full, Sense::hover());
    }

    fn tab_panel(&mut self, ui: &mut Ui, sh: &Shared, tab: Tab) {
        let hint = |ui: &mut Ui, t: &str| {
            ui.add_space(6.0);
            ui.label(RichText::new(t).color(theme::TEXT_DIM));
        };
        match tab {
            Tab::Zoom => {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if ui.button("−").on_hover_text("Zoom out (−, or ⌘/Ctrl/Alt + scroll, or pinch)").clicked() {
                        self.zoom_around_playhead(1.0 / 1.5);
                    }
                    let mut lz = self.zoom.ln();
                    let zr = crate::studio_timeline::MIN_ZOOM.ln()..=crate::studio_timeline::MAX_ZOOM.ln();
                    ui.spacing_mut().slider_width = 360.0;
                    if ui.add(egui::Slider::new(&mut lz, zr).show_value(false)).on_hover_text("Zoom").changed() {
                        let f = lz.exp() / self.zoom;
                        self.zoom_around_playhead(f);
                    }
                    if ui.button("+").on_hover_text("Zoom in (+, or ⌘/Ctrl/Alt + scroll, or pinch)").clicked() {
                        self.zoom_around_playhead(1.5);
                    }
                    if ui.button("Fit").on_hover_text("Show the whole mix").clicked() {
                        self.fit_request = true;
                    }
                    let visible = ui.ctx().content_rect().width() / self.zoom.max(0.01);
                    ui.label(RichText::new(format!("{} visible", theme::fmt_time_short(visible as f64))).color(theme::TEXT_DIM));
                    ui.separator();
                    ui.label(RichText::new("Space play · Left/Right 1 bar (Shift: 8) · [ ] transitions · F follow · + − zoom · ⌘Z undo").size(11.0).color(theme::TEXT_DIM));
                });
            }
            Tab::Playlist => self.playlist_panel(ui, sh),
            Tab::Transition => match self.selection {
                Selection::Transition(k) if k < self.mix.transitions.len() && k + 1 < self.mix.tracks.len() => {
                    egui::ScrollArea::vertical().id_salt("tr-editor").auto_shrink([false, false]).show(ui, |ui| self.transition_editor(ui, sh, k));
                }
                _ => hint(ui, "Click a transition (the coloured chip above the lanes) to edit it. A mix needs at least two tracks."),
            },
            Tab::Track => match self.selection {
                Selection::Track(i) if i < self.mix.tracks.len() => self.track_inspector(ui, sh, i),
                _ => hint(ui, "Click a track in the timeline to edit its mix points, key and level."),
            },
            Tab::Mashups => {
                let playing = self.preview_playing;
                self.mashup.panel(ui, sh, &self.mix, playing, &mut self.mashup_actions);
                if !self.mashup.open {
                    self.tab = None;
                }
            }
            Tab::Master => {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(RichText::new("Tempo").color(theme::TEXT_DIM));
                    let fixed = matches!(self.mix.tempo, TempoMode::Fixed(_));
                    if ui.selectable_label(!fixed, "Glide").on_hover_text("Each track plays at its own BPM; tempo glides during transitions").clicked() && fixed {
                        self.mix.tempo = TempoMode::Glide;
                        self.changed();
                    }
                    if ui.selectable_label(fixed, "Fixed").on_hover_text("Every track is stretched to one BPM").clicked() && !fixed {
                        self.mix.tempo = TempoMode::Fixed(self.timeline.bpm_at_beat(0.0).round());
                        self.changed();
                    }
                    if let TempoMode::Fixed(mut b) = self.mix.tempo {
                        if ui.add(egui::DragValue::new(&mut b).range(60.0..=200.0).speed(0.1).suffix(" BPM")).changed() {
                            self.mix.tempo = TempoMode::Fixed(b);
                            self.changed();
                        }
                    }
                    ui.separator();
                    if crate::widgets::toggle(ui, self.mix.keylock, "Keylock", theme::accent(), vec2(62.0, 22.0)).on_hover_text("Keep the original key when the tempo changes").clicked() {
                        self.mix.keylock = !self.mix.keylock;
                        self.changed();
                    }
                    ui.separator();
                    let (lo, hi) = self.timeline.tracks.iter().fold((f64::MAX, 0.0f64), |(lo, hi), t| (lo.min(t.grid.bpm), hi.max(t.grid.bpm)));
                    if hi > 0.0 {
                        ui.label(RichText::new(format!("{} · {} tracks · {:.0}–{:.0} BPM", theme::fmt_time_short(self.timeline.total_secs), self.mix.tracks.len(), lo, hi)).color(theme::TEXT_DIM));
                    }
                });
                hint(ui, "Lane volume, EQ and filter are on the lane headers left of the timeline. Export format and tail: Settings › Export & recording.");
            }
        }
    }

    fn track_inspector(&mut self, ui: &mut Ui, sh: &Shared, i: usize) {
        let id = self.mix.tracks[i].track_id;
        let a = sh.analyses.get(&id).cloned();
        let t = sh.track(id);
        let grid = a.as_ref().and_then(|a| a.grid);
        let bar = grid.map(|g| g.bar_len()).unwrap_or(2.0);
        let neighbour_key = |j: usize| self.mix.tracks.get(j).and_then(|m| sh.analyses.get(&m.track_id).and_then(|a| a.key).map(|k| k.shifted(m.key_shift)));
        let (prev_key, next_key) = (if i > 0 { neighbour_key(i - 1) } else { None }, neighbour_key(i + 1));
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}.", i + 1)).size(15.0).color(theme::TEXT_DIM));
            ui.label(RichText::new(t.map(|t| t.title.clone()).unwrap_or_default()).size(15.0).strong());
            ui.label(RichText::new(t.map(|t| t.artist.clone()).unwrap_or_default()).color(theme::TEXT_DIM));
            if let Some(a) = &a {
                ui.add_space(10.0);
                ui.label(RichText::new(format!("{:.2} BPM", a.bpm().unwrap_or(0.0))).monospace());
                if let Some(k) = a.key {
                    ui.label(RichText::new(crate::theme::key_label(k)).strong().color(theme::key_color(k)));
                    ui.label(RichText::new(k.name()).color(theme::TEXT_DIM));
                }
                if let Some(e) = a.energy {
                    ui.label(RichText::new(format!("energy {e}")).color(theme::WARN));
                }
                if a.stems_model.is_some() {
                    ui.add_space(12.0);
                    if ui.button("🎤 Find a backing for these vocals").on_hover_text("Search the library for instrumentals that carry this track's vocals").clicked() {
                        self.pending.push(StudioAction::FindMashups(Some(id)));
                    }
                }
            } else {
                ui.label(RichText::new("analysing…").color(theme::TEXT_DIM));
            }
        });
        ui.add_space(6.0);
        let mt: &mut MixTrack = &mut self.mix.tracks[i];
        ui.horizontal_top(|ui| {
            // mix points
            card(ui, 470.0, |ui| {
                ui.label(RichText::new("Mix points").strong());
                for (label, which, help) in [("In", 0, "Where this track starts (aligned with the previous track's mix-out)"), ("Out", 1, "Where the transition to the next track starts")] {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(label).color(theme::TEXT_DIM)).on_hover_text(help);
                        let v = if which == 0 { &mut mt.mix_in } else { &mut mt.mix_out };
                        ui.label(RichText::new(format!("{} · bar {:.0}", theme::fmt_time(*v), grid.map(|g| g.bar_at(*v).round() + 1.0).unwrap_or(0.0))).monospace());
                        for (txt, d) in [("−8", -8.0), ("−1", -1.0), ("+1", 1.0), ("+8", 8.0)] {
                            if ui.small_button(txt).on_hover_text("bars").clicked() {
                                *v = (*v + d * bar).max(0.0);
                                changed = true;
                            }
                        }
                        if let Some(a) = &a {
                            egui::ComboBox::from_id_salt(("cuepick", which, i)).width(90.0).selected_text("to cue…").show_ui(ui, |ui| {
                                for c in &a.cues {
                                    if ui.selectable_label(false, format!("{} · {}", c.name, theme::fmt_time(c.pos))).clicked() {
                                        *v = c.pos;
                                        changed = true;
                                    }
                                }
                            });
                        }
                    });
                }
            });
            // key + gain
            card(ui, 420.0, |ui| {
                ui.label(RichText::new("Key & level").strong());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Key shift").color(theme::TEXT_DIM));
                    for s in -2..=2 {
                        let lbl = if s == 0 { "0".to_string() } else { format!("{s:+}") };
                        if ui.selectable_label(mt.key_shift == s, lbl).clicked() && mt.key_shift != s {
                            mt.key_shift = s;
                            changed = true;
                        }
                    }
                    if let Some(k) = a.as_ref().and_then(|a| a.key) {
                        let k2 = k.shifted(mt.key_shift);
                        ui.label(RichText::new(crate::theme::key_label(k2)).strong().color(theme::key_color(k2)));
                        for (n, other) in [("prev", prev_key), ("next", next_key)] {
                            if let Some(o) = other {
                                let c = if n == "prev" { o.compatibility(k2) } else { k2.compatibility(o) };
                                let col = if c >= 0.85 { theme::OK } else if c >= 0.5 { theme::WARN } else { Color32::from_rgb(255, 90, 90) };
                                ui.label(RichText::new(format!("{n} {}", crate::theme::key_label(o))).size(11.0).color(col));
                            }
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Gain").color(theme::TEXT_DIM));
                    changed |= ui.add(egui::Slider::new(&mut mt.gain_db, -12.0..=6.0).suffix(" dB").step_by(0.5)).changed();
                    if mt.gain_db != 0.0 && ui.small_button("0 dB").clicked() {
                        mt.gain_db = 0.0;
                        changed = true;
                    }
                });
                ui.label(RichText::new("Loudness is matched automatically; gain is an extra offset.").size(10.5).color(theme::TEXT_DIM));
            });
        });
        if changed {
            self.changed();
        }
    }
}

fn card(ui: &mut Ui, w: f32, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new().fill(theme::PANEL_HI).corner_radius(8.0).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(w);
        ui.vertical(|ui| add(ui));
    });
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clubcraft_core::{MixTrack, Param, Side, Transition};

    fn mix() -> Mix {
        let t = |id| MixTrack { track_id: id, mix_in: 0.0, mix_out: 60.0, key_shift: 0, gain_db: 0.0, stems: Vec::new() };
        Mix { tracks: vec![t(1), t(2)], transitions: vec![Transition::new(TransitionStyle::StemBlend, 32)], ..Default::default() }
    }

    #[test]
    fn undo_redo_groups_gestures() {
        let mut s = StudioView::default();
        s.set_mix(mix());
        // a drag: many changes in quick succession = one undo step
        for g in 1..=5 {
            s.mix.tracks[0].gain_db = g as f32;
            s.changed();
        }
        s.last_change = None; // gesture over
        s.mix.tracks[1].key_shift = 2;
        s.changed();
        assert_eq!(s.undo.len(), 2);
        s.undo();
        assert_eq!((s.mix.tracks[0].gain_db, s.mix.tracks[1].key_shift), (5.0, 0));
        s.undo();
        assert_eq!(s.mix.tracks[0].gain_db, 0.0);
        s.redo();
        s.redo();
        assert_eq!((s.mix.tracks[0].gain_db, s.mix.tracks[1].key_shift), (5.0, 2));
        // a new edit clears the redo stack
        s.undo();
        s.last_change = None;
        s.mix.keylock = !s.mix.keylock;
        s.changed();
        assert!(s.redo.is_empty());
    }

    #[test]
    fn transition_drags_keep_the_music_in_place() {
        let mut s = StudioView::default();
        let mut m = mix();
        m.tracks[1].mix_in = 10.0;
        s.set_mix(m);
        s.timeline = MixTimeline::build(&s.mix, &|_| None); // 120 BPM fallback grids: a beat = 0.5 s
        let before = s.mix.transitions[0].value(Side::In, Param::Stem(clubcraft_core::StemKind::Vocals), 28.0);
        // grow at the start by a bar: window 36 beats, both tracks keep their place in the mix
        s.resize_transition_start(0, -4);
        assert_eq!(s.mix.transitions[0].beats, 36);
        assert!((s.mix.tracks[0].mix_out - 58.0).abs() < 1e-9 && (s.mix.tracks[1].mix_in - 8.0).abs() < 1e-9);
        let after = s.mix.transitions[0].value(Side::In, Param::Stem(clubcraft_core::StemKind::Vocals), 32.0);
        assert!((before - after).abs() < 1e-6, "curves stay with the music");
        assert!(s.mix.transitions[0].lanes.iter().all(|l| l.points[0].0 == 0.0));
        // grow at the end: curves stretch over the new length
        s.resize_transition_end(0, 4);
        assert_eq!(s.mix.transitions[0].beats, 40);
        // move a bar earlier
        s.move_transition(0, -4.0);
        assert!((s.mix.tracks[0].mix_out - 56.0).abs() < 1e-9);
    }

    #[test]
    fn locked_samples_move_with_their_track() {
        let sh = Shared::default();
        let mut s = StudioView::default();
        let mut m = mix();
        // locked to track 2, 20 s in; another clip stays where it is
        let mut locked = clubcraft_core::SampleClip::new(0, "/tmp/a.wav".into(), 0.0);
        locked.lock = Some(clubcraft_core::SampleLock { track_id: 2, pos: 20.0 });
        m.samples = vec![locked, clubcraft_core::SampleClip::new(1, "/tmp/b.wav".into(), 10.0)];
        s.set_mix(m);
        s.refresh_timeline(&sh);
        let at = |s: &StudioView| s.timeline.beat_of_track_pos(1, 20.0);
        assert!((s.mix.samples[0].start_beat - at(&s)).abs() < 1e-9);
        // the second track comes in 8 s (16 beats) later: the locked clip goes along
        let before = s.mix.samples[0].start_beat;
        s.mix.tracks[0].mix_out += 8.0;
        s.changed();
        s.refresh_timeline(&sh);
        assert!((s.mix.samples[0].start_beat - before - 16.0).abs() < 1e-6, "{} vs {before}", s.mix.samples[0].start_beat);
        assert_eq!(s.mix.samples[1].start_beat, 10.0);
        // moved by hand it keeps the new place in its track
        s.mix.samples[0].start_beat += 4.0;
        s.relock_sample(0);
        assert!((s.mix.samples[0].lock.unwrap().pos - 22.0).abs() < 1e-6);
        // undoing the track move puts the clip back too
        s.undo();
        s.refresh_timeline(&sh);
        assert!((s.mix.samples[0].start_beat - before).abs() < 1e-6);
    }
}
