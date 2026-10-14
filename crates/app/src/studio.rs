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
        let planned = planner::plan_mix(&self.mix.name, &order, &|id| analysis_of(sh, id), &PlanOptions { has_stems: true, ..Default::default() });
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
        let mut planned = planner::plan_mix(&self.mix.name, order, &|id| analysis_of(sh, id), &PlanOptions { style: if keep_style { styles.first().copied() } else { None }, ..Default::default() });
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
        egui::Panel::left("studio-mixes").default_size(200.0).resizable(true).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Mixes");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("+ New").clicked() {
                        *new_mix = true;
                    }
                });
            });
            ui.separator();
            egui::ScrollArea::vertical().id_salt("mixlist").show(ui, |ui| {
                for (id, name, _) in mixes {
                    let sel = self.mix.id == *id;
                    if ui.selectable_label(sel, name).clicked() && !sel {
                        *load_mix = Some(*id);
                    }
                }
            });
            ui.separator();
            if ui.button("Import DJ.Studio projects").on_hover_text("Reads ~/Music/DJ.Studio (read-only)").clicked() {
                out.push(StudioAction::ImportDjStudio);
            }
            if ui.button("Export cues to VirtualDJ…").on_hover_text("Writes beat grids, keys, hot cues and automix points into a VirtualDJ database.xml").clicked() {
                out.push(StudioAction::ExportVirtualDj);
            }
        });
        egui::Panel::bottom("studio-inspector").default_size(250.0).resizable(true).show(ui, |ui| self.inspector(ui, sh));
        egui::CentralPanel::default().show(ui, |ui| {
            self.header(ui, sh, out);
            ui.add_space(4.0);
            self.order_strip(ui, sh);
            ui.add_space(6.0);
            self.timeline_ui(ui, sh, out);
        });
    }

    fn header(&mut self, ui: &mut Ui, sh: &Shared, out: &mut Vec<StudioAction>) {
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.mix.name).font(egui::TextStyle::Heading).desired_width(260.0));
            if r.changed() {
                self.dirty = true;
            }
            let play = if self.playing { "⏸ Pause" } else { "▶ Play" };
            if ui.add(egui::Button::new(RichText::new(play).strong()).min_size(vec2(80.0, 24.0))).clicked() {
                out.push(StudioAction::PlayPause);
            }
            ui.label(RichText::new(format!("{} / {}", theme::fmt_time(self.position), theme::fmt_time(self.timeline.total_secs))).monospace());
            ui.separator();
            let mut fixed = matches!(self.mix.tempo, TempoMode::Fixed(_));
            if ui.checkbox(&mut fixed, "Fixed BPM").changed() {
                self.mix.tempo = if fixed { TempoMode::Fixed(self.timeline.bpm_at_beat(0.0).round()) } else { TempoMode::Glide };
                self.changed();
            }
            if let TempoMode::Fixed(mut b) = self.mix.tempo {
                if ui.add(egui::DragValue::new(&mut b).range(60.0..=200.0).speed(0.1).suffix(" BPM")).changed() {
                    self.mix.tempo = TempoMode::Fixed(b);
                    self.changed();
                }
            }
            if ui.checkbox(&mut self.mix.keylock, "Keylock").changed() {
                self.dirty = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(p) = self.render_progress {
                    ui.add(egui::ProgressBar::new(p).desired_width(140.0).text("Rendering…"));
                } else if ui.add_enabled(!self.mix.tracks.is_empty(), egui::Button::new("Export mix…")).on_hover_text("Render the mix to a WAV file (with cue sheet)").clicked() {
                    out.push(StudioAction::Export);
                }
                if ui.add_enabled(!self.mix.tracks.is_empty(), egui::Button::new("Play live ▶")).on_hover_text("Queue this mix on the live decks with automix").clicked() {
                    out.push(StudioAction::SendToLive);
                }
                if ui.add_enabled(self.dirty, egui::Button::new("Save")).clicked() {
                    out.push(StudioAction::SaveMix);
                }
                let total_tracks = self.mix.tracks.len();
                ui.label(RichText::new(format!("{total_tracks} tracks")).color(theme::TEXT_DIM));
            });
        });
        ui.horizontal(|ui| {
            if ui.add_enabled(self.mix.tracks.len() > 2, egui::Button::new("✨ Harmonize order")).on_hover_text("Re-order tracks for harmonic mixing, smooth tempo and an energy arc").clicked() {
                self.harmonize(sh);
            }
            ui.label("BPM");
            ui.add(egui::Slider::new(&mut self.order_opts.key_weight, 0.0..=1.0).show_value(false));
            ui.label("Key");
            ui.add(egui::Slider::new(&mut self.order_opts.energy_weight, 0.0..=1.0).show_value(false).text("Energy arc"));
            ui.checkbox(&mut self.order_opts.lock_first, "Lock first");
            ui.checkbox(&mut self.order_opts.lock_last, "Lock last");
            if ui.add_enabled(self.mix.tracks.len() > 1, egui::Button::new("Auto mix points")).on_hover_text("Recompute mix-in/out points and transition lengths from the analysed structure").clicked() {
                let order: Vec<TrackId> = self.mix.tracks.iter().map(|t| t.track_id).collect();
                self.replan(sh, &order);
            }
            if !self.message.is_empty() {
                ui.label(RichText::new(&self.message).color(theme::TEXT_DIM).size(11.0));
            }
        });
    }

    fn order_strip(&mut self, ui: &mut Ui, sh: &Shared) {
        let h = 64.0;
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
                    let (r, resp) = ui.allocate_exact_size(vec2(150.0, 54.0), Sense::click_and_drag());
                    let p = ui.painter();
                    p.rect_filled(r, 5.0, if sel { theme::PANEL_HI.gamma_multiply(1.4) } else { theme::PANEL_HI });
                    p.rect_stroke(r, 5.0, Stroke::new(1.0, if sel { theme::ACCENT } else { theme::LINE }), StrokeKind::Inside);
                    let title = t.map(|t| t.title.clone()).unwrap_or("?".into());
                    let artist = t.map(|t| t.artist.clone()).unwrap_or_default();
                    p.text(r.left_top() + vec2(6.0, 5.0), egui::Align2::LEFT_TOP, truncate(&format!("{}. {}", i + 1, title), 22), egui::FontId::proportional(12.0), theme::TEXT);
                    p.text(r.left_top() + vec2(6.0, 21.0), egui::Align2::LEFT_TOP, truncate(&artist, 24), egui::FontId::proportional(10.5), theme::TEXT_DIM);
                    if let Some(a) = a {
                        if let Some(b) = a.bpm() {
                            p.text(r.left_bottom() + vec2(6.0, -5.0), egui::Align2::LEFT_BOTTOM, format!("{b:.1}"), egui::FontId::monospace(11.0), theme::TEXT);
                        }
                        if let Some(k) = a.key {
                            let k = k.shifted(self.mix.tracks[i].key_shift);
                            p.text(r.center_bottom() + vec2(0.0, -5.0), egui::Align2::CENTER_BOTTOM, k.camelot(), egui::FontId::proportional(12.0), theme::key_color(k));
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
                        let (br, bresp) = ui.allocate_exact_size(vec2(58.0, 54.0), Sense::click());
                        let selt = self.selection == Selection::Transition(i);
                        let col = match compat {
                            Some(c) if c >= 0.85 => theme::OK,
                            Some(c) if c >= 0.5 => theme::WARN,
                            Some(_) => Color32::from_rgb(255, 80, 80),
                            None => theme::TEXT_DIM,
                        };
                        let p = ui.painter();
                        p.rect_filled(br.shrink2(vec2(2.0, 8.0)), 10.0, if selt { theme::PANEL_HI } else { theme::BG });
                        p.rect_stroke(br.shrink2(vec2(2.0, 8.0)), 10.0, Stroke::new(1.0, col), StrokeKind::Inside);
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
                ui.label(RichText::new("Select a track or a transition to edit it. Drag tracks from the library onto the strip above; Harmonize orders them; the timeline shows the beat-matched result.").color(theme::TEXT_DIM));
            }
            Selection::Track(i) if i < self.mix.tracks.len() => self.track_inspector(ui, sh, i),
            Selection::Transition(k) if k < self.mix.transitions.len() && k + 1 < self.mix.tracks.len() => self.transition_editor(ui, sh, k),
            _ => self.selection = Selection::None,
        }
    }

    fn track_inspector(&mut self, ui: &mut Ui, sh: &Shared, i: usize) {
        let id = self.mix.tracks[i].track_id;
        let a = sh.analyses.get(&id).cloned();
        let name = sh.track(id).map(|t| t.display_name()).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.heading(format!("{}. {}", i + 1, name));
            if let Some(a) = &a {
                ui.label(RichText::new(format!("{:.2} BPM · {} · energy {}", a.bpm().unwrap_or(0.0), a.key.map(|k| k.camelot()).unwrap_or("-".into()), a.energy.unwrap_or(0))).color(theme::TEXT_DIM));
            }
        });
        let grid = a.as_ref().and_then(|a| a.grid);
        let bar = grid.map(|g| g.bar_len()).unwrap_or(2.0);
        let mut changed = false;
        let mt: &mut MixTrack = &mut self.mix.tracks[i];
        egui::Grid::new("trk").num_columns(2).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
            for (label, which) in [("Mix in", 0), ("Mix out", 1)] {
                ui.label(label);
                ui.horizontal(|ui| {
                    let v = if which == 0 { &mut mt.mix_in } else { &mut mt.mix_out };
                    ui.label(RichText::new(format!("{}  (bar {:.0})", theme::fmt_time(*v), grid.map(|g| g.bar_at(*v) + 1.0).unwrap_or(0.0))).monospace());
                    for (txt, d) in [("-8 bars", -8.0), ("-1", -1.0), ("+1", 1.0), ("+8 bars", 8.0)] {
                        if ui.small_button(txt).clicked() {
                            *v = (*v + d * bar).max(0.0);
                            changed = true;
                        }
                    }
                    if let Some(a) = &a {
                        egui::ComboBox::from_id_salt(("cuepick", which)).selected_text("from cue…").show_ui(ui, |ui| {
                            for c in &a.cues {
                                if ui.selectable_label(false, format!("{} @ {}", c.name, theme::fmt_time(c.pos))).clicked() {
                                    *v = c.pos;
                                    changed = true;
                                }
                            }
                        });
                    }
                });
                ui.end_row();
            }
            ui.label("Key shift");
            ui.horizontal(|ui| {
                changed |= ui.add(egui::Slider::new(&mut mt.key_shift, -3..=3).suffix(" st")).changed();
                if let Some(k) = a.as_ref().and_then(|a| a.key) {
                    ui.label(RichText::new(k.shifted(mt.key_shift).camelot()).color(theme::key_color(k.shifted(mt.key_shift))).strong());
                }
            });
            ui.end_row();
            ui.label("Gain");
            changed |= ui.add(egui::Slider::new(&mut mt.gain_db, -12.0..=6.0).suffix(" dB")).changed();
            ui.end_row();
        });
        if changed {
            self.changed();
        }
    }
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
