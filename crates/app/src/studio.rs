//! Studio screen (DJ.Studio-style mix designer).

use crate::app::Shared;
use crate::library_view::DraggedTracks;
use crate::theme;
use crate::waveform::{self, WaveParams, WaveStyle};
use clubcraft_core::planner::{self, OrderItem, OrderOptions, PlanOptions};
use clubcraft_core::{Analysis, CueKind, Mix, MixTimeline, MixTrack, Param, Side, StemKind, TempoMode, TrackId, Transition, TransitionStyle};
use eframe::egui::{self, pos2, vec2, Align, Color32, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui};

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
    timeline_dirty: bool,
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
        egui::Panel::left("studio-mixes").default_size(200.0).resizable(true).show_inside(ui, |ui| {
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
        egui::Panel::bottom("studio-inspector").default_size(250.0).resizable(true).show_inside(ui, |ui| self.inspector(ui, sh));
        egui::CentralPanel::default().show_inside(ui, |ui| {
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
            let play = if self.playing { "❚❚ Pause" } else { "▶ Play" };
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
                if ui.add_enabled(!self.mix.tracks.is_empty(), egui::Button::new("Play live ▸")).on_hover_text("Queue this mix on the live decks with automix").clicked() {
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

    fn timeline_ui(&mut self, ui: &mut Ui, sh: &Shared, out: &mut Vec<StudioAction>) {
        let avail = ui.available_rect_before_wrap();
        let rect = Rect::from_min_size(avail.min, vec2(avail.width(), avail.height().max(160.0)));
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        if let Some(p) = resp.dnd_release_payload::<DraggedTracks>() {
            self.add_tracks(sh, &p.0);
        }
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 4.0, theme::BG);
        let total = self.timeline.total_secs.max(1.0) as f32;
        // zoom (ctrl/cmd + scroll or pinch) and horizontal scroll
        if resp.hovered() {
            let (scroll, zoom_d, cmd) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers.command));
            if cmd && scroll.y != 0.0 || zoom_d != 1.0 {
                let f = if zoom_d != 1.0 { zoom_d } else { 1.0 + scroll.y / 300.0 };
                let mouse_x = ui.input(|i| i.pointer.hover_pos()).map(|p| p.x - rect.left()).unwrap_or(rect.width() / 2.0);
                let t_at = (self.scroll + mouse_x) / self.zoom;
                self.zoom = (self.zoom * f).clamp(rect.width() / total.max(1.0) * 0.9, 120.0);
                self.scroll = t_at * self.zoom - mouse_x;
            } else {
                self.scroll -= scroll.x + scroll.y;
            }
        }
        if resp.dragged() && ui.input(|i| i.pointer.secondary_down() || i.modifiers.alt) {
            self.scroll -= resp.drag_delta().x;
        }
        if self.follow && self.playing {
            let x = self.position as f32 * self.zoom - self.scroll;
            if x > rect.width() * 0.8 || x < 0.0 {
                self.scroll = self.position as f32 * self.zoom - rect.width() * 0.2;
            }
        }
        self.scroll = self.scroll.clamp(0.0, (total * self.zoom - rect.width() * 0.5).max(0.0));
        let x_of = |t: f64| rect.left() + t as f32 * self.zoom - self.scroll;
        // ruler
        let ruler = Rect::from_min_size(rect.min, vec2(rect.width(), 18.0));
        p.rect_filled(ruler, 0.0, theme::PANEL);
        let step = [5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0].into_iter().find(|s| s * self.zoom >= 70.0).unwrap_or(600.0);
        let mut t = (self.scroll / self.zoom / step).floor() * step;
        while x_of(t as f64) < rect.right() {
            let x = x_of(t as f64);
            p.line_segment([pos2(x, ruler.bottom() - 6.0), pos2(x, ruler.bottom())], Stroke::new(1.0, theme::TEXT_DIM));
            p.text(pos2(x + 3.0, ruler.top() + 2.0), egui::Align2::LEFT_TOP, theme::fmt_time_short(t as f64), egui::FontId::proportional(10.0), theme::TEXT_DIM);
            t += step;
        }
        // lanes
        let lane_h = ((rect.height() - 18.0 - 46.0) / 2.0).clamp(50.0, 140.0);
        let lane_top = [ruler.bottom() + 6.0, ruler.bottom() + 12.0 + lane_h];
        for (k, x) in self.timeline.transitions.iter().enumerate() {
            let xr = Rect::from_min_max(pos2(x_of(x.start_sec), lane_top[0]), pos2(x_of(x.end_sec), lane_top[1] + lane_h));
            let sel = self.selection == Selection::Transition(k);
            p.rect_filled(xr, 3.0, if sel { Color32::from_rgba_unmultiplied(255, 92, 140, 40) } else { Color32::from_rgba_unmultiplied(255, 255, 255, 14) });
            if sel {
                p.rect_stroke(xr, 3.0, Stroke::new(1.0, theme::ACCENT), StrokeKind::Inside);
            }
        }
        let mut clicked_track: Option<usize> = None;
        for (i, tt) in self.timeline.tracks.iter().enumerate() {
            let lane = i % 2;
            let r = Rect::from_min_max(pos2(x_of(tt.start_sec), lane_top[lane]), pos2(x_of(tt.end_sec), lane_top[lane] + lane_h));
            if r.right() < rect.left() || r.left() > rect.right() {
                continue;
            }
            let accent = theme::deck_color(lane);
            let sel = self.selection == Selection::Track(i);
            p.rect_filled(r, 4.0, theme::PANEL);
            let a = sh.analyses.get(&tt.track_id);
            if let (Some(wf), Some(a)) = (sh.waveforms.get(&tt.track_id), a) {
                // draw the waveform segment [track_pos(start), track_pos(end)] stretched into r
                let t0 = self.timeline.track_pos(i, tt.start_beat);
                let t1 = self.timeline.track_pos(i, tt.end_beat);
                let wp = WaveParams { wf, grid: a.grid, cues: &[], sections: &a.sections, style: WaveStyle::Stems, stem_gains: [1.0; 4], loop_region: None, accent };
                waveform::paint_wave(&p, r.shrink2(vec2(0.0, 14.0)), &wp, t0, t1, true);
                // cue markers (mix-relevant ones)
                for c in a.cues.iter().filter(|c| matches!(c.kind, CueKind::Drop | CueKind::Breakdown | CueKind::VocalIn | CueKind::MixIn | CueKind::MixOut)) {
                    let mb = self.timeline.beat_of_track_pos(i, c.pos);
                    if mb < tt.start_beat || mb > tt.end_beat {
                        continue;
                    }
                    let x = x_of(self.timeline.sec_at_beat(mb));
                    p.line_segment([pos2(x, r.top() + 13.0), pos2(x, r.bottom())], Stroke::new(1.0, theme::rgb(c.color).linear_multiply(0.7)));
                    p.text(pos2(x + 2.0, r.bottom() - 2.0), egui::Align2::LEFT_BOTTOM, c.kind.label(), egui::FontId::proportional(9.0), theme::rgb(c.color));
                }
            }
            p.rect_stroke(r, 4.0, Stroke::new(if sel { 2.0 } else { 1.0 }, if sel { theme::ACCENT } else { accent.linear_multiply(0.6) }), StrokeKind::Inside);
            let name = sh.track(tt.track_id).map(|t| t.display_name()).unwrap_or_default();
            let label_x = r.left().max(rect.left()) + 4.0;
            p.text(pos2(label_x, r.top() + 2.0), egui::Align2::LEFT_TOP, truncate(&format!("{}. {}", i + 1, name), ((r.right() - label_x) / 6.5).max(4.0) as usize), egui::FontId::proportional(11.0), theme::TEXT);
            if resp.clicked() && resp.interact_pointer_pos().is_some_and(|pp| r.contains(pp)) {
                clicked_track = Some(i);
            }
        }
        // transition lanes overlay (volume of both sides) for the selected transition
        if let Selection::Transition(k) = self.selection {
            if let (Some(x), Some(tr)) = (self.timeline.transitions.get(k), self.mix.transitions.get(k)) {
                let x0 = x_of(x.start_sec);
                let x1 = x_of(x.end_sec);
                for (side, lane) in [(Side::Out, k % 2), (Side::In, (k + 1) % 2)] {
                    let top = lane_top[lane] + 14.0;
                    let h = lane_h - 28.0;
                    let mut pts = Vec::new();
                    for s in 0..=64 {
                        let f = s as f32 / 64.0;
                        let v = tr.value(side, Param::Volume, f * x.beats as f32);
                        pts.push(pos2(x0 + (x1 - x0) * f, top + h * (1.0 - v)));
                    }
                    p.add(egui::Shape::line(pts, Stroke::new(1.5, Color32::WHITE)));
                }
            }
        }
        // BPM line
        let bpm_r = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 40.0), rect.max);
        p.rect_filled(bpm_r, 0.0, theme::PANEL);
        if self.timeline.total_beats > 0.0 {
            let mut lo = f64::MAX;
            let mut hi = f64::MIN;
            for tt in &self.timeline.tracks {
                lo = lo.min(tt.grid.bpm);
                hi = hi.max(tt.grid.bpm);
            }
            if let TempoMode::Fixed(b) = self.mix.tempo {
                lo = lo.min(b);
                hi = hi.max(b);
            }
            let (lo, hi) = (lo - 2.0, hi + 2.0);
            let mut pts = Vec::new();
            let mut xpix = bpm_r.left();
            while xpix < bpm_r.right() {
                let t = ((xpix - rect.left() + self.scroll) / self.zoom) as f64;
                if t > self.timeline.total_secs {
                    break;
                }
                let b = self.timeline.bpm_at_beat(self.timeline.beat_at_sec(t));
                pts.push(pos2(xpix, bpm_r.bottom() - 4.0 - ((b - lo) / (hi - lo)) as f32 * (bpm_r.height() - 8.0)));
                xpix += 3.0;
            }
            p.add(egui::Shape::line(pts, Stroke::new(1.5, theme::WARN)));
            p.text(bpm_r.left_top() + vec2(4.0, 2.0), egui::Align2::LEFT_TOP, format!("BPM {:.1}", self.timeline.bpm_at_beat(self.timeline.beat_at_sec(self.position))), egui::FontId::proportional(10.0), theme::WARN);
        }
        // playhead
        let xp = x_of(self.position);
        p.line_segment([pos2(xp, rect.top()), pos2(xp, rect.bottom())], Stroke::new(1.5, Color32::WHITE));
        if let Some(i) = clicked_track {
            self.selection = Selection::Track(i);
        }
        if resp.clicked() || (resp.dragged() && !ui.input(|i| i.modifiers.alt || i.pointer.secondary_down())) {
            if let Some(pp) = resp.interact_pointer_pos() {
                if pp.y < lane_top[0] || pp.y > bpm_r.top() || resp.dragged() {
                    let t = ((pp.x - rect.left() + self.scroll) / self.zoom) as f64;
                    out.push(StudioAction::Seek(t.clamp(0.0, self.timeline.total_secs)));
                }
            }
        }
    }

    fn inspector(&mut self, ui: &mut Ui, sh: &Shared) {
        match self.selection {
            Selection::None => {
                ui.label(RichText::new("Select a track or a transition to edit it. Drag tracks from the library onto the strip above; Harmonize orders them; the timeline shows the beat-matched result.").color(theme::TEXT_DIM));
            }
            Selection::Track(i) if i < self.mix.tracks.len() => self.track_inspector(ui, sh, i),
            Selection::Transition(k) if k < self.mix.transitions.len() => self.transition_inspector(ui, sh, k),
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

    fn transition_inspector(&mut self, ui: &mut Ui, sh: &Shared, k: usize) {
        let a = sh.track(self.mix.tracks[k].track_id).map(|t| t.title.clone()).unwrap_or_default();
        let b = sh.track(self.mix.tracks[k + 1].track_id).map(|t| t.title.clone()).unwrap_or_default();
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.heading(format!("Transition: {}  ▸  {}", truncate(&a, 30), truncate(&b, 30)));
        });
        let tr: &mut Transition = &mut self.mix.transitions[k];
        ui.horizontal(|ui| {
            ui.label("Style");
            egui::ComboBox::from_id_salt("tstyle").selected_text(tr.style.label()).show_ui(ui, |ui| {
                for s in TransitionStyle::ALL {
                    if ui.selectable_label(tr.style == s, s.label()).clicked() && tr.style != s {
                        *tr = Transition::new(s, tr.beats);
                        changed = true;
                    }
                }
            });
            ui.label("Length");
            for bars in [2u32, 4, 8, 16, 32] {
                if ui.selectable_label(tr.beats == bars * 4, format!("{bars} bars")).clicked() && tr.beats != bars * 4 {
                    let style = tr.style;
                    let lanes = if style == TransitionStyle::Custom { scale_lanes(&tr.lanes, tr.beats as f32, (bars * 4) as f32) } else { style.lanes((bars * 4) as f32) };
                    tr.beats = bars * 4;
                    tr.lanes = lanes;
                    changed = true;
                }
            }
        });
        ui.add_space(4.0);
        // lane overview: one row per parameter with both sides
        let params: Vec<Param> = {
            let mut v = vec![Param::Volume, Param::EqLow, Param::EqMid, Param::EqHigh, Param::Filter, Param::Echo];
            v.extend(StemKind::ALL.iter().map(|s| Param::Stem(*s)));
            v.into_iter().filter(|p| tr.lanes.iter().any(|l| l.param == *p)).collect()
        };
        let row_h = 22.0;
        let w = ui.available_width();
        for param in params {
            ui.horizontal(|ui| {
                ui.add_sized(vec2(70.0, row_h), egui::Label::new(RichText::new(param.label()).size(11.0)));
                let (r, resp) = ui.allocate_exact_size(vec2(w - 90.0, row_h), Sense::click_and_drag());
                let p = ui.painter();
                p.rect_filled(r, 2.0, theme::BG);
                for bar in 0..=(tr.beats / 4) {
                    let x = r.left() + r.width() * (bar * 4) as f32 / tr.beats as f32;
                    p.line_segment([pos2(x, r.top()), pos2(x, r.bottom())], Stroke::new(1.0, theme::LINE));
                }
                for (side, col) in [(Side::Out, theme::deck_color(k % 2)), (Side::In, theme::deck_color((k + 1) % 2))] {
                    let lo = if matches!(param, Param::Filter) { 0.0 } else { 0.0 };
                    let mut pts = Vec::new();
                    for s in 0..=96 {
                        let f = s as f32 / 96.0;
                        let v = tr.value(side, param, f * tr.beats as f32);
                        pts.push(pos2(r.left() + r.width() * f, r.bottom() - 2.0 - (v - lo).clamp(0.0, 1.0) * (r.height() - 4.0)));
                    }
                    p.add(egui::Shape::line(pts, Stroke::new(1.5, col)));
                }
                // simple editing: drag sets the incoming (lower half) or outgoing (upper half) curve point at that beat
                if resp.dragged() || resp.clicked() {
                    if let Some(pp) = resp.interact_pointer_pos() {
                        let x = ((pp.x - r.left()) / r.width()).clamp(0.0, 1.0) * tr.beats as f32;
                        let x = (x * 2.0).round() / 2.0;
                        let v = (1.0 - (pp.y - r.top()) / r.height()).clamp(0.0, 1.0);
                        let side = if ui.input(|i| i.modifiers.shift) { Side::In } else { Side::Out };
                        set_point(tr, side, param, x, v);
                        tr.style = TransitionStyle::Custom;
                        changed = true;
                    }
                }
                resp.on_hover_text("Drag to draw the outgoing curve, Shift+drag for the incoming one");
            });
        }
        if changed {
            self.changed();
        }
    }
}

#[derive(Clone, Copy)]
struct OrderDrag(usize);

fn set_point(tr: &mut Transition, side: Side, param: Param, x: f32, v: f32) {
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

fn scale_lanes(l: &[clubcraft_core::Lane], from: f32, to: f32) -> Vec<clubcraft_core::Lane> {
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
