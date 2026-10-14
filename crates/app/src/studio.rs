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
    pub strip_open: bool,
}

impl Default for StudioView {
    fn default() -> Self {
        StudioView {
            mix: Mix::default(),
            timeline: MixTimeline::default(),
            dirty: false,
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
            strip_open: true,
        }
    }
}

fn analysis_of(sh: &Shared, id: TrackId) -> Option<Analysis> {
    sh.analyses.get(&id).cloned()
}

impl StudioView {
    pub fn set_mix(&mut self, mix: Mix) {
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
        self.timeline_dirty = true;
        self.dirty = true;
        self.edited = true;
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
        egui::Panel::top("studio-toolbar").frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(10, 6))).show(ui, |ui| {
            self.toolbar(ui, sh, mixes, out, load_mix, new_mix);
        });
        let none_selected = self.selection == Selection::None;
        egui::Panel::bottom(if none_selected { "studio-inspector-empty" } else { "studio-inspector-v2" })
            .default_size(if none_selected { 80.0 } else { 380.0 })
            .min_size(if self.selection == Selection::None { 70.0 } else { (ui.available_height() * 0.40).clamp(220.0, 400.0) })
            .max_size((ui.available_height() * 0.65).max(260.0))
            .resizable(true)
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(12, 8)))
            .show(ui, |ui| self.inspector(ui, sh));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::symmetric(8, 6))).show(ui, |ui| {
            self.order_strip(ui, sh);
            ui.add_space(6.0);
            self.timeline_ui(ui, sh, out);
        });
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
                self.zoom = (self.zoom * 1.25).min(160.0);
            }
            if i.key_pressed(Key::Minus) {
                self.zoom = (self.zoom / 1.25).max(0.2);
            }
            if i.key_pressed(Key::Escape) {
                self.selection = Selection::None;
            }
            if cmd && i.key_pressed(Key::S) {
                out.push(StudioAction::SaveMix);
            }
        });
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

    fn toolbar(&mut self, ui: &mut Ui, sh: &Shared, mixes: &[(i64, String, i64)], out: &mut Vec<StudioAction>, load_mix: &mut Option<i64>, new_mix: &mut bool) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            // mix menu
            ui.menu_button(RichText::new("☰ Mixes").size(13.0), |ui| {
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
            let r = ui.add(egui::TextEdit::singleline(&mut self.mix.name).font(egui::TextStyle::Heading).frame(egui::Frame::NONE).desired_width(240.0));
            if r.changed() {
                self.dirty = true;
            }
            ui.separator();
            // transport
            if ui.add(egui::Button::new("⏮").min_size(vec2(28.0, 26.0))).on_hover_text("To start (Home)").clicked() {
                out.push(StudioAction::Seek(0.0));
            }
            let play = if self.playing { "⏸" } else { "▶" };
            let pb = egui::Button::new(RichText::new(play).size(15.0).color(Color32::BLACK)).fill(theme::ACCENT).min_size(vec2(44.0, 26.0));
            if ui.add_enabled(!self.mix.tracks.is_empty(), pb).on_hover_text("Play / pause (Space)").clicked() {
                out.push(StudioAction::PlayPause);
            }
            ui.label(RichText::new(theme::fmt_time(self.position)).monospace().size(16.0).strong());
            ui.label(RichText::new(format!("/ {}", theme::fmt_time(self.timeline.total_secs))).monospace().color(theme::TEXT_DIM));
            ui.label(RichText::new(format!("{:.1} BPM", self.timeline.bpm_at_beat(self.timeline.beat_at_sec(self.position)))).monospace().color(theme::WARN));
            if crate::widgets::toggle(ui, self.follow, "Follow", theme::ACCENT, vec2(56.0, 22.0)).on_hover_text("Keep the playhead in view (F)").clicked() {
                self.follow = !self.follow;
            }
            ui.separator();
            if ui.small_button("−").on_hover_text("Zoom out (−)").clicked() {
                self.zoom = (self.zoom / 1.25).max(0.2);
            }
            if ui.small_button("Fit").on_hover_text("Show the whole mix").clicked() {
                self.fit_request = true;
            }
            if ui.small_button("+").on_hover_text("Zoom in (+)").clicked() {
                self.zoom = (self.zoom * 1.25).min(160.0);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(p) = self.render_progress {
                    ui.add(egui::ProgressBar::new(p).desired_width(140.0).text("Rendering…"));
                } else if ui.add_enabled(!self.mix.tracks.is_empty(), egui::Button::new("Export…")).on_hover_text("Render the mix to WAV with a cue sheet").clicked() {
                    out.push(StudioAction::Export);
                }
                if ui.add_enabled(!self.mix.tracks.is_empty(), egui::Button::new("Play live ▶")).on_hover_text("Play this mix on the live decks").clicked() {
                    out.push(StudioAction::SendToLive);
                }
                if self.dirty {
                    if ui.button("Save").on_hover_text("⌘S").clicked() {
                        out.push(StudioAction::SaveMix);
                    }
                } else {
                    ui.label(RichText::new("saved").size(11.0).color(theme::TEXT_DIM));
                }
            });
        });
        ui.add_space(2.0);
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
            ui.separator();
            ui.label(RichText::new("Tempo").color(theme::TEXT_DIM));
            let fixed = matches!(self.mix.tempo, TempoMode::Fixed(_));
            if ui.selectable_label(!fixed, "Glide").on_hover_text("Each track plays at its own BPM; tempo glides during transitions").clicked() && fixed {
                self.mix.tempo = TempoMode::Glide;
                self.changed();
            }
            if ui.selectable_label(fixed, "Fixed").clicked() && !fixed {
                self.mix.tempo = TempoMode::Fixed(self.timeline.bpm_at_beat(0.0).round());
                self.changed();
            }
            if let TempoMode::Fixed(mut b) = self.mix.tempo {
                if ui.add(egui::DragValue::new(&mut b).range(60.0..=200.0).speed(0.1).suffix(" BPM")).changed() {
                    self.mix.tempo = TempoMode::Fixed(b);
                    self.changed();
                }
            }
            if crate::widgets::toggle(ui, self.mix.keylock, "Keylock", theme::ACCENT, vec2(62.0, 20.0)).on_hover_text("Keep the original key when the tempo changes").clicked() {
                self.mix.keylock = !self.mix.keylock;
                self.changed();
            }
            if !self.message.is_empty() {
                ui.label(RichText::new(&self.message).color(theme::TEXT_DIM).size(11.0));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new("Space play · Left/Right 1 bar (Shift: 8) · [ ] transitions · F follow · +/- zoom").size(10.5).color(theme::TEXT_DIM));
            });
        });
    }

    fn order_strip(&mut self, ui: &mut Ui, sh: &Shared) {
        let chevron = if self.strip_open { "⏷" } else { "⏵" };
        let n = self.mix.tracks.len();
        if ui.add(egui::Label::new(RichText::new(format!("{chevron} Tracks ({n})")).size(12.0).color(theme::TEXT_DIM)).sense(Sense::click())).on_hover_text("Show / hide the track order strip").clicked() {
            self.strip_open = !self.strip_open;
        }
        if !self.strip_open {
            return;
        }
        let h = 50.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
        if let Some(p) = resp.dnd_release_payload::<DraggedTracks>() {
            self.add_tracks(sh, &p.0);
        }
        let hovering = resp.dnd_hover_payload::<DraggedTracks>().is_some();
        ui.painter().rect_filled(rect, 4.0, theme::BG);
        if hovering {
            ui.painter().rect_stroke(rect, 4.0, Stroke::new(2.0, theme::ACCENT), StrokeKind::Inside);
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
                    p.rect_stroke(r, 5.0, Stroke::new(1.0, if sel { theme::ACCENT } else { theme::LINE }), StrokeKind::Inside);
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

    fn inspector(&mut self, ui: &mut Ui, sh: &Shared) {
        match self.selection {
            Selection::None => {
                ui.add_space(8.0);
                ui.label(RichText::new("Click a track or a transition (the coloured chip on the timeline ruler) to edit it.").color(theme::TEXT_DIM));
                ui.label(RichText::new("Add tracks by dragging them from the Library onto the strip above. ✨ Harmonize orders them by key, tempo and energy.").color(theme::TEXT_DIM));
            }
            Selection::Track(i) if i < self.mix.tracks.len() => self.track_inspector(ui, sh, i),
            Selection::Transition(k) if k < self.mix.transitions.len() && k + 1 < self.mix.tracks.len() => self.transition_editor(ui, sh, k),
            _ => self.selection = Selection::None,
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
