//! Studio screen (DJ.Studio-style mix designer).

use crate::app::Shared;
use crate::library_view::DraggedTracks;
use crate::theme;

use clubcraft_core::planner::{self, OrderItem, OrderOptions, PlanOptions};
use clubcraft_core::{Analysis, Mix, MixTimeline, MixTrack, Param, Side, TempoMode, TrackId, Transition, TransitionStyle};
use eframe::egui::{self, vec2, Align, Color32, Layout, RichText, Sense, Stroke, StrokeKind, Ui};

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Selection {
    None,
    Track(usize),
    Transition(usize),
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
    last_selection: Selection,
    /// Open panel of the bottom bar.
    pub tab: Option<Tab>,
    /// Undo / redo history of the mix (one step per edit gesture).
    undo: Vec<Mix>,
    redo: Vec<Mix>,
    committed: Mix,
    last_change: Option<std::time::Instant>,
    /// Width of the centred transport group (measured the previous frame).
    transport_w: f32,
    pub selection: Selection,
    /// Pixels per second in the timeline.
    pub zoom: f32,
    pub scroll: f32,
    pub follow: bool,
    pub order_opts: OrderOptions,
    pub playing: bool,
    pub position: f64,
    pub render_progress: Option<f32>,
    pub message: String,
    pub timeline_dirty: bool,
    /// Set when the mix changed; the app pushes edits into a running preview.
    pub edited: bool,
    pub lane_show: [crate::studio_timeline::ShowMode; clubcraft_core::MIX_LANES],
    pub tempo_open: bool,
    pub drag: Option<crate::studio_timeline::Drag>,
    pub drag_acc: f32,
    pub tgroup: crate::studio_transition::ParamGroup,
    /// Zoom to fit the whole mix on the next frame.
    pub fit_request: bool,
    /// Defaults for new transitions (from the preferences).
    pub plan_opts: PlanOptions,
    /// Collapsed lanes in the timeline and the track strip.
    pub lane_collapsed: [bool; clubcraft_core::MIX_LANES],
}

impl Default for StudioView {
    fn default() -> Self {
        StudioView {
            mix: Mix::default(),
            timeline: MixTimeline::default(),
            dirty: false,
            pending: Vec::new(),
            mashup: Default::default(),
            mashup_actions: Vec::new(),
            preview_playing: false,
            last_selection: Selection::None,
            tab: Some(Tab::Playlist),
            undo: Vec::new(),
            redo: Vec::new(),
            committed: Mix::default(),
            last_change: None,
            transport_w: 420.0,
            selection: Selection::None,
            zoom: 6.0,
            scroll: 0.0,
            follow: true,
            order_opts: OrderOptions::default(),
            playing: false,
            position: 0.0,
            render_progress: None,
            message: String::new(),
            timeline_dirty: true,
            edited: false,
            lane_show: Default::default(),
            tempo_open: true,
            drag: None,
            drag_acc: 0.0,
            tgroup: Default::default(),
            fit_request: true,
            plan_opts: PlanOptions::default(),
            lane_collapsed: [false; clubcraft_core::MIX_LANES],
        }
    }
}

fn analysis_of(sh: &Shared, id: TrackId) -> Option<Analysis> {
    sh.analyses.get(&id).cloned()
}

impl StudioView {
    pub fn set_mix(&mut self, mix: Mix) {
        self.committed = mix.clone();
        self.undo.clear();
        self.redo.clear();
        self.last_change = None;
        self.mix = mix;
        self.selection = Selection::None;
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
    }

    fn restore(&mut self, m: Mix) {
        self.mix = m;
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
        let items: Vec<OrderItem> = self.mix.tracks.iter().map(|t| OrderItem::from_analysis(t.track_id, sh.analyses.get(&t.track_id))).collect();
        let order = planner::order_tracks(&items, &self.order_opts);
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
        self.mix = planned;
        self.changed();
    }

    pub fn ui(&mut self, ui: &mut Ui, sh: &Shared, mixes: &[(i64, String, i64)], out: &mut Vec<StudioAction>, load_mix: &mut Option<i64>, new_mix: &mut bool) {
        if self.timeline_dirty {
            self.refresh_timeline(sh);
        }
        self.keyboard(ui.ctx(), sh, out);
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
            .show(ui, |ui| self.transport(ui, mixes, out, load_mix, new_mix));
        if let Some(tab) = self.tab {
            let (default, min) = match tab {
                Tab::Zoom => (54.0, 50.0),
                Tab::Playlist => (100.0, 90.0),
                Tab::Master => (90.0, 80.0),
                Tab::Track | Tab::Transition => (380.0, 220.0),
                Tab::Mashups => (430.0, 240.0),
            };
            egui::Panel::bottom(format!("studio-tab-{tab:?}"))
                .default_size(default)
                .min_size(min)
                .max_size((ui.available_height() * 0.7).max(min + 20.0))
                .resizable(!matches!(tab, Tab::Zoom))
                .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(12, 8)))
                .show(ui, |ui| self.tab_panel(ui, sh, tab));
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::symmetric(8, 6))).show(ui, |ui| {
            self.timeline_ui(ui, sh, out);
        });
        // selecting a clip / transition opens its tab
        if self.selection != self.last_selection {
            match self.selection {
                Selection::Track(_) => self.tab = Some(Tab::Track),
                Selection::Transition(_) => self.tab = Some(Tab::Transition),
                Selection::None => {}
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
                self.selection = Selection::None;
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
        let _ = sh;
    }

    fn track_at_playhead(&self) -> Option<usize> {
        let n = self.timeline.tracks.len();
        (n > 0).then(|| self.timeline.tracks.iter().rposition(|t| t.start_sec <= self.position + 1e-6).unwrap_or(0).min(n - 1))
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
    fn transport(&mut self, ui: &mut Ui, mixes: &[(i64, String, i64)], out: &mut Vec<StudioAction>, load_mix: &mut Option<i64>, new_mix: &mut bool) {
        let full = ui.available_rect_before_wrap();
        let has = !self.mix.tracks.is_empty();
        // ---- left: mixes, name, edit menu, undo / redo
        let mut left = ui.new_child(egui::UiBuilder::new().max_rect(full).layout(Layout::left_to_right(Align::Center)));
        left.spacing_mut().item_spacing.x = 6.0;
        left.menu_button(RichText::new("☰ Mixes").size(13.0), |ui| {
            ui.set_min_width(260.0);
            if ui.button("＋ New mix").clicked() {
                *new_mix = true;
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
        if left.add(egui::TextEdit::singleline(&mut self.mix.name).font(egui::FontId::proportional(15.0)).frame(egui::Frame::NONE).desired_width(190.0)).changed() {
            self.dirty = true;
        }
        if self.dirty {
            if left.small_button("Save").on_hover_text("⌘S").clicked() {
                out.push(StudioAction::SaveMix);
            }
        } else {
            left.label(RichText::new("saved").size(11.0).color(theme::TEXT_DIM));
        }
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
        if left.add_enabled(!self.undo.is_empty(), egui::Button::new("↩").min_size(vec2(28.0, 26.0))).on_hover_text("Undo (⌘Z)").clicked() {
            self.undo();
        }
        if left.add_enabled(!self.redo.is_empty(), egui::Button::new("↪").min_size(vec2(28.0, 26.0))).on_hover_text("Redo (Shift+⌘Z)").clicked() {
            self.redo();
        }
        // ---- right: output
        let mut right = ui.new_child(egui::UiBuilder::new().max_rect(full).layout(Layout::right_to_left(Align::Center)));
        right.spacing_mut().item_spacing.x = 6.0;
        if let Some(p) = self.render_progress {
            right.add(egui::ProgressBar::new(p).desired_width(140.0).text("Rendering…"));
        } else if right.add_enabled(has, egui::Button::new("Export…")).on_hover_text("Render the mix to WAV with a cue sheet").clicked() {
            out.push(StudioAction::Export);
        }
        if right.add_enabled(has, egui::Button::new("Play live ▶")).on_hover_text("Play this mix on the live decks").clicked() {
            out.push(StudioAction::SendToLive);
        }
        if !self.message.is_empty() {
            right.label(RichText::new(&self.message).color(theme::TEXT_DIM).size(11.0));
        }
        // ---- centre: transport (centred on the measured width of the last frame)
        let cw = self.transport_w.max(100.0);
        let crect = egui::Rect::from_min_size(egui::pos2(full.center().x - cw / 2.0, full.top()), vec2(cw + 8.0, full.height()));
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(crect).layout(Layout::left_to_right(Align::Center)));
        c.spacing_mut().item_spacing.x = 8.0;
        let round = |t: &str| egui::Button::new(RichText::new(t).size(15.0)).corner_radius(17.0).min_size(vec2(34.0, 34.0));
        if c.add(round("⏮")).on_hover_text("To start (Home)").clicked() {
            out.push(StudioAction::Seek(0.0));
        }
        if c.add_enabled(self.timeline.transitions.len() > 0, round("⏪")).on_hover_text("Previous transition ([)").clicked() {
            let k = self.timeline.transitions.iter().rposition(|t| t.start_sec < self.position - 0.5);
            if let Some(k) = k {
                self.selection = Selection::Transition(k);
                out.push(StudioAction::Seek(self.timeline.transitions[k].start_sec));
            }
        }
        let play = if self.playing { "⏸" } else { "▶" };
        let pb = egui::Button::new(RichText::new(play).size(18.0).color(Color32::BLACK)).fill(theme::accent()).corner_radius(20.0).min_size(vec2(40.0, 40.0));
        if c.add_enabled(has, pb).on_hover_text("Play / pause (Space)").clicked() {
            out.push(StudioAction::PlayPause);
        }
        if c.add_enabled(self.timeline.transitions.len() > 0, round("⏩")).on_hover_text("Next transition (])").clicked() {
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
            Tab::Playlist => {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if ui.add_enabled(self.mix.tracks.len() > 2, egui::Button::new("✨ Harmonize")).on_hover_text("Re-order for harmonic mixing, smooth tempo and an energy arc").clicked() {
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
                        ui.checkbox(&mut self.order_opts.allow_key_shift, "Allow ±1 semitone key shifts");
                    });
                    if ui.add_enabled(self.mix.tracks.len() > 1, egui::Button::new("Auto mix points")).on_hover_text("Recompute mix-in/out points and transition lengths from the analysed structure").clicked() {
                        let order: Vec<TrackId> = self.mix.tracks.iter().map(|t| t.track_id).collect();
                        self.replan(sh, &order);
                    }
                    ui.label(RichText::new(format!("{} tracks · drag tracks from the Library onto the strip, drag cards to reorder", self.mix.tracks.len())).size(11.0).color(theme::TEXT_DIM));
                });
                ui.add_space(4.0);
                self.order_strip(ui, sh);
            }
            Tab::Transition => match self.selection {
                Selection::Transition(k) if k < self.mix.transitions.len() && k + 1 < self.mix.tracks.len() => self.transition_editor(ui, sh, k),
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

    fn order_strip(&mut self, ui: &mut Ui, sh: &Shared) {
        let h = 50.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
        if let Some(p) = resp.dnd_release_payload::<DraggedTracks>() {
            self.add_tracks(sh, &p.0);
        }
        let hovering = resp.dnd_hover_payload::<DraggedTracks>().is_some();
        ui.painter().rect_filled(rect, 4.0, theme::BG);
        if hovering {
            ui.painter().rect_stroke(rect, 4.0, Stroke::new(2.0, theme::accent()), StrokeKind::Inside);
        }
        if self.mix.tracks.is_empty() {
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Drag tracks from the library here to start a mix", egui::FontId::proportional(14.0), theme::TEXT_DIM);
            return;
        }
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(4.0)).layout(Layout::left_to_right(Align::Center)));
        let mut swap: Option<(usize, usize)> = None;
        let mut remove: Option<usize> = None;
        egui::ScrollArea::horizontal().id_salt("order").show(&mut child, |ui| {
            ui.horizontal(|ui| {
                for i in 0..self.mix.tracks.len() {
                    let id = self.mix.tracks[i].track_id;
                    let t = sh.track(id);
                    let a = sh.analyses.get(&id);
                    let sel = self.selection == Selection::Track(i);
                    let (r, resp) = ui.allocate_exact_size(vec2(150.0, 42.0), Sense::click_and_drag());
                    let p = ui.painter();
                    p.rect_filled(r, 5.0, if sel { theme::PANEL_HI.gamma_multiply(1.4) } else { theme::PANEL_HI });
                    p.rect_stroke(r, 5.0, Stroke::new(1.0, if sel { theme::accent() } else { theme::LINE }), StrokeKind::Inside);
                    let title = t.map(|t| t.title.clone()).unwrap_or("?".into());
                    let artist = t.map(|t| t.artist.clone()).unwrap_or_default();
                    p.text(r.left_top() + vec2(6.0, 5.0), egui::Align2::LEFT_TOP, truncate(&format!("{}. {}", i + 1, title), 22), egui::FontId::proportional(12.0), theme::TEXT);
                    let _ = &artist;
                    if let Some(a) = a {
                        if let Some(b) = a.bpm() {
                            p.text(r.left_bottom() + vec2(6.0, -5.0), egui::Align2::LEFT_BOTTOM, format!("{b:.1}"), egui::FontId::monospace(11.0), theme::TEXT);
                        }
                        if let Some(k) = a.key {
                            let k = k.shifted(self.mix.tracks[i].key_shift);
                            p.text(r.center_bottom() + vec2(0.0, -5.0), egui::Align2::CENTER_BOTTOM, crate::theme::key_label(k), egui::FontId::proportional(12.0), theme::key_color(k));
                        }
                        if let Some(e) = a.energy {
                            p.text(r.right_bottom() + vec2(-6.0, -5.0), egui::Align2::RIGHT_BOTTOM, format!("E{e}"), egui::FontId::proportional(10.5), theme::WARN);
                        }
                    }
                    if resp.clicked() {
                        self.selection = Selection::Track(i);
                        if let Some(t) = self.timeline.tracks.get(i) {
                            self.scroll = (t.start_sec as f32 * self.zoom - 100.0).max(0.0);
                        }
                    }
                    if resp.drag_started() {
                        resp.dnd_set_drag_payload(OrderDrag(i));
                    }
                    if let Some(from) = resp.dnd_release_payload::<OrderDrag>() {
                        if from.0 != i {
                            swap = Some((from.0, i));
                        }
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Remove from mix").clicked() {
                            remove = Some(i);
                            ui.close();
                        }
                    });
                    // transition badge
                    if i + 1 < self.mix.tracks.len() {
                        let tr = &self.mix.transitions[i];
                        let compat = match (a.and_then(|a| a.key), sh.analyses.get(&self.mix.tracks[i + 1].track_id).and_then(|a| a.key)) {
                            (Some(k1), Some(k2)) => Some(k1.shifted(self.mix.tracks[i].key_shift).compatibility(k2.shifted(self.mix.tracks[i + 1].key_shift))),
                            _ => None,
                        };
                        let (br, bresp) = ui.allocate_exact_size(vec2(54.0, 42.0), Sense::click());
                        let selt = self.selection == Selection::Transition(i);
                        let col = match compat {
                            Some(c) if c >= 0.85 => theme::OK,
                            Some(c) if c >= 0.5 => theme::WARN,
                            Some(_) => Color32::from_rgb(255, 80, 80),
                            None => theme::TEXT_DIM,
                        };
                        let p = ui.painter();
                        p.rect_filled(br.shrink2(vec2(2.0, 4.0)), 8.0, if selt { theme::PANEL_HI } else { theme::BG });
                        p.rect_stroke(br.shrink2(vec2(2.0, 4.0)), 8.0, Stroke::new(1.0, col), StrokeKind::Inside);
                        p.text(br.center() - vec2(0.0, 7.0), egui::Align2::CENTER_CENTER, short_style(tr.style), egui::FontId::proportional(10.0), theme::TEXT);
                        p.text(br.center() + vec2(0.0, 8.0), egui::Align2::CENTER_CENTER, format!("{} bars", tr.beats / 4), egui::FontId::proportional(9.5), theme::TEXT_DIM);
                        if bresp.clicked() {
                            self.selection = Selection::Transition(i);
                            if let Some(x) = self.timeline.transitions.get(i) {
                                self.scroll = (x.start_sec as f32 * self.zoom - 200.0).max(0.0);
                            }
                        }
                    }
                }
            });
        });
        if let Some((a, b)) = swap {
            let mut order: Vec<TrackId> = self.mix.tracks.iter().map(|t| t.track_id).collect();
            let t = order.remove(a);
            order.insert(b, t);
            self.replan(sh, &order);
        }
        if let Some(i) = remove {
            self.mix.tracks.remove(i);
            if !self.mix.transitions.is_empty() {
                self.mix.transitions.remove(i.min(self.mix.transitions.len() - 1));
            }
            self.selection = Selection::None;
            self.changed();
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

#[derive(Clone, Copy)]
struct OrderDrag(usize);

pub(crate) fn set_point(tr: &mut Transition, side: Side, param: Param, x: f32, v: f32) {
    let lane = match tr.lanes.iter_mut().find(|l| l.side == side && l.param == param) {
        Some(l) => l,
        None => {
            tr.lanes.push(clubcraft_core::Lane::new(side, param, &[(0.0, param.neutral()), (tr.beats as f32, param.neutral())]));
            tr.lanes.last_mut().unwrap()
        }
    };
    lane.points.retain(|p| (p.0 - x).abs() > 0.25);
    lane.points.push((x, v));
    lane.points.sort_by(|a, b| a.0.total_cmp(&b.0));
}

pub(crate) fn scale_lanes(l: &[clubcraft_core::Lane], from: f32, to: f32) -> Vec<clubcraft_core::Lane> {
    l.iter().map(|lane| clubcraft_core::Lane { side: lane.side, param: lane.param, points: lane.points.iter().map(|(x, y)| (x / from * to, *y)).collect() }).collect()
}

fn short_style(s: TransitionStyle) -> &'static str {
    match s {
        TransitionStyle::StemBlend => "Stems",
        TransitionStyle::BassSwap => "Bass swap",
        TransitionStyle::Crossfade => "Fade",
        TransitionStyle::VocalOver => "Vocal over",
        TransitionStyle::FilterOut => "Filter",
        TransitionStyle::EchoOut => "Echo",
        TransitionStyle::Cut => "Cut",
        TransitionStyle::Mashup => "Mashup",
        TransitionStyle::Custom => "Custom",
    }
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
    use clubcraft_core::MixTrack;

    fn mix() -> Mix {
        let t = |id| MixTrack { track_id: id, mix_in: 0.0, mix_out: 60.0, key_shift: 0, gain_db: 0.0 };
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
}
