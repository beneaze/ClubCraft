//! Studio timeline: lane headers (mixer per lane), clips with inline transition automation,
//! draggable transition markers and the tempo lane.

use crate::app::Shared;
use crate::studio::{Selection, StudioAction, StudioView};
use crate::theme;
use crate::waveform::{self, WaveParams, WaveStyle};
use crate::widgets;
use clubcraft_core::{CueKind, LaneSettings, Param, Side, StemKind, TempoMode, Transition, MIX_LANES};
use eframe::egui::{self, pos2, vec2, Align, Color32, CursorIcon, Id, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui};

/// Which automation the timeline draws on a lane's clips.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum ShowMode {
    #[default]
    All,
    Volume,
    Eq,
    Stems,
    None,
}

impl ShowMode {
    pub const ALL: [ShowMode; 5] = [ShowMode::All, ShowMode::Volume, ShowMode::Eq, ShowMode::Stems, ShowMode::None];
    pub fn label(self) -> &'static str {
        match self {
            ShowMode::All => "Volume & stems",
            ShowMode::Volume => "Volume",
            ShowMode::Eq => "EQ & filter",
            ShowMode::Stems => "Stems",
            ShowMode::None => "Waveform only",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Drag {
    TransitionStart(usize),
    TransitionEnd(usize),
    Clip(usize),
}

pub const HEADER_W: f32 = 214.0;
/// Zoom range of the studio timeline in pixels per second (3000 px/s ≈ one beat over 1500 px).
pub const MIN_ZOOM: f32 = 0.2;
/// Height of the beat ruler at the top of each clip.
const RULER_H: f32 = 14.0;
pub const MAX_ZOOM: f32 = 3000.0;
pub const EQ_COLORS: [Color32; 3] = [Color32::from_rgb(255, 90, 90), Color32::from_rgb(120, 230, 120), Color32::from_rgb(110, 200, 255)];
pub const FILTER_COLOR: Color32 = Color32::from_rgb(200, 140, 255);
pub const VOLUME_COLOR: Color32 = Color32::from_rgb(255, 214, 60);

/// Knob 0..1 ↔ dB with unity at 0.8.
pub fn fader_db(v: f32) -> f32 {
    if v <= 0.001 {
        -60.0
    } else if v <= 0.8 {
        40.0 * (v / 0.8).log10()
    } else {
        (v - 0.8) / 0.2 * 6.0
    }
}

pub fn db_fader(db: f32) -> f32 {
    if db <= -59.0 {
        0.0
    } else if db <= 0.0 {
        0.8 * 10f32.powf(db / 40.0)
    } else {
        0.8 + db / 6.0 * 0.2
    }
}

impl StudioView {
    pub(crate) fn timeline_ui(&mut self, ui: &mut Ui, sh: &Shared, out: &mut Vec<StudioAction>) {
        let avail = ui.available_rect_before_wrap();
        let full = Rect::from_min_size(avail.min, vec2(avail.width(), avail.height().max(220.0)));
        ui.allocate_rect(full, Sense::hover());
        let ruler_h = 24.0;
        let tempo_h = if self.tempo_open { 58.0 } else { 24.0 };
        // Collapsed lanes are a slim row (clip titles only); expanded lanes share the rest.
        const COLLAPSED_H: f32 = 30.0;
        let n_open = (0..MIX_LANES).filter(|l| !self.lane_collapsed[*l]).count().max(1) as f32;
        let n_closed = MIX_LANES as f32 - (0..MIX_LANES).filter(|l| !self.lane_collapsed[*l]).count() as f32;
        let open_h = ((full.height() - ruler_h - 21.0 - tempo_h - 8.0 - n_closed * (COLLAPSED_H + 2.0)) / n_open).clamp(80.0, 300.0);
        let lane_hs: Vec<f32> = (0..MIX_LANES).map(|l| if self.lane_collapsed[l] { COLLAPSED_H } else { open_h }).collect();
        let canvas = Rect::from_min_max(pos2(full.left() + HEADER_W + 4.0, full.top()), full.max);
        let ruler = Rect::from_min_size(canvas.min, vec2(canvas.width(), ruler_h));
        let strip_h = 20.0;
        let tstrip = Rect::from_min_size(pos2(canvas.left(), ruler.bottom() + 1.0), vec2(canvas.width(), strip_h));
        let mut lanes: Vec<Rect> = Vec::with_capacity(MIX_LANES);
        let mut ly = tstrip.bottom() + 2.0;
        for h in &lane_hs {
            lanes.push(Rect::from_min_size(pos2(canvas.left(), ly), vec2(canvas.width(), *h)));
            ly += h + 2.0;
        }
        let tempo = Rect::from_min_size(pos2(canvas.left(), lanes[MIX_LANES - 1].bottom() + 4.0), vec2(canvas.width(), tempo_h));

        // ----- lane headers
        let mut lane_changed = false;
        {
            let corner = Rect::from_min_size(full.min, vec2(HEADER_W, ruler_h));
            ui.painter().rect_filled(corner, 4.0, theme::PANEL);
            ui.painter().text(corner.left_center() + vec2(8.0, 0.0), egui::Align2::LEFT_CENTER, format!("{}  ·  {} tracks", theme::fmt_time_short(self.timeline.total_secs), self.mix.tracks.len()), egui::FontId::proportional(11.5), theme::TEXT_DIM);
        }
        for l in 0..MIX_LANES {
            let hr = Rect::from_min_size(pos2(full.left(), lanes[l].top()), vec2(HEADER_W, lane_hs[l]));
            lane_changed |= self.lane_header(ui, hr, l);
        }
        let th = Rect::from_min_size(pos2(full.left(), tempo.top()), vec2(HEADER_W, tempo_h));
        ui.painter().rect_filled(th, 4.0, theme::PANEL);
        let tr = ui.interact(Rect::from_min_size(th.min, vec2(HEADER_W, 22.0)), Id::new("tempo-hdr"), Sense::click());
        ui.painter().text(th.left_top() + vec2(8.0, 11.0), egui::Align2::LEFT_CENTER, format!("{}  Tempo", if self.tempo_open { "⏷" } else { "⏵" }), egui::FontId::proportional(13.0), theme::TEXT);
        if tr.clicked() {
            self.tempo_open = !self.tempo_open;
        }
        if self.tempo_open {
            let mode = match self.mix.tempo {
                TempoMode::Glide => "Glide between tracks".to_string(),
                TempoMode::Fixed(b) => format!("Fixed {b:.1} BPM"),
            };
            ui.painter().text(th.left_top() + vec2(10.0, 36.0), egui::Align2::LEFT_CENTER, mode, egui::FontId::proportional(11.0), theme::TEXT_DIM);
        }
        if lane_changed {
            self.changed();
        }

        // ----- canvas interaction: zoom / scroll / seek
        let resp = ui.interact(canvas, Id::new("timeline-canvas"), Sense::click_and_drag());
        if let Some(p) = resp.dnd_release_payload::<crate::library_view::DraggedTracks>() {
            self.add_tracks(sh, &p.0);
        }
        let total = self.timeline.total_secs.max(1.0) as f32;
        if self.fit_request && self.timeline.total_secs > 0.0 {
            self.zoom = (canvas.width() / total).clamp(MIN_ZOOM, MAX_ZOOM);
            self.scroll = 0.0;
            self.fit_request = false;
        }
        if ui.rect_contains_pointer(canvas) {
            let (scroll, zoom_d, zmod) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers.command || i.modifiers.ctrl || i.modifiers.alt));
            if (zmod && scroll.y != 0.0) || zoom_d != 1.0 {
                let f = if zoom_d != 1.0 { zoom_d } else { (1.0 + scroll.y / 200.0).clamp(0.5, 2.0) };
                let mouse_x = ui.input(|i| i.pointer.hover_pos()).map(|p| p.x - canvas.left()).unwrap_or(canvas.width() / 2.0);
                let t_at = (self.scroll + mouse_x) / self.zoom;
                self.zoom = (self.zoom * f).clamp(MIN_ZOOM, MAX_ZOOM);
                self.scroll = t_at * self.zoom - mouse_x;
            } else if scroll != egui::Vec2::ZERO {
                // Manual scrolling takes over from "follow playhead" (press F or Follow to resume).
                self.scroll -= scroll.x + scroll.y;
                if self.playing {
                    self.follow = false;
                }
            }
        }
        if self.follow && self.playing {
            // keep the playhead between 20 % and 80 % of the view; page smoothly when it leaves
            let x = self.position as f32 * self.zoom - self.scroll;
            if x > canvas.width() * 0.8 || x < canvas.width() * 0.05 {
                self.scroll = self.position as f32 * self.zoom - canvas.width() * 0.2;
            }
        }
        self.scroll = self.scroll.clamp(0.0, (total * self.zoom - canvas.width() * 0.5).max(0.0));
        let (zoom, scroll) = (self.zoom, self.scroll);
        let x_of = move |t: f64| canvas.left() + t as f32 * zoom - scroll;
        let p = ui.painter_at(canvas.expand(1.0));
        p.rect_filled(canvas, 4.0, theme::BG);

        // ----- ruler: mix bars + time (beats when zoomed in)
        p.rect_filled(ruler, 0.0, theme::PANEL);
        let tl = &self.timeline;
        if tl.total_beats > 0.0 {
            let px_per_beat = 60.0 / tl.bpm_at_beat(tl.beat_at_sec(((canvas.width() * 0.5 + scroll) / zoom) as f64)) as f32 * zoom;
            let px_per_bar = 4.0 * px_per_beat;
            let every = [1.0f32, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0].into_iter().find(|n| n * px_per_bar >= 46.0).unwrap_or(128.0) as f64;
            // only visit the visible range
            let b_lo = (tl.beat_at_sec((scroll / zoom) as f64) / 4.0).floor().max(0.0);
            let b_hi = (tl.beat_at_sec(((canvas.width() + scroll) / zoom) as f64) / 4.0).ceil() + 1.0;
            let mut bar = (b_lo / every).floor() * every;
            while bar <= b_hi && bar * 4.0 <= tl.total_beats {
                let x = x_of(tl.sec_at_beat(bar * 4.0));
                p.line_segment([pos2(x, ruler.top() + 12.0), pos2(x, ruler.bottom())], Stroke::new(1.0, Color32::from_rgb(200, 190, 90)));
                p.text(pos2(x + 3.0, ruler.top() + 1.0), egui::Align2::LEFT_TOP, format!("{}", bar as i64 + 1), egui::FontId::proportional(10.0), Color32::from_rgb(220, 210, 120));
                p.text(pos2(x + 3.0, ruler.top() + 12.0), egui::Align2::LEFT_TOP, theme::fmt_time_short(tl.sec_at_beat(bar * 4.0)), egui::FontId::proportional(9.0), theme::TEXT_DIM);
                bar += every;
            }
            // minor ticks: bars, and beats (with bar.beat labels) when there is room
            let step_beats = if px_per_beat >= 10.0 { 1.0 } else if px_per_bar >= 5.0 { 4.0 } else { 0.0 };
            if step_beats > 0.0 {
                let mut bt = (b_lo * 4.0 / step_beats).floor() * step_beats;
                while bt <= b_hi * 4.0 && bt <= tl.total_beats {
                    let x = x_of(tl.sec_at_beat(bt));
                    let in_bar = (bt as i64).rem_euclid(4);
                    p.line_segment([pos2(x, ruler.bottom() - if in_bar == 0 { 6.0 } else { 3.0 }), pos2(x, ruler.bottom())], Stroke::new(1.0, theme::LINE));
                    if in_bar != 0 && px_per_beat >= 70.0 {
                        p.text(pos2(x + 2.0, ruler.top() + 12.0), egui::Align2::LEFT_TOP, format!("{}.{}", (bt / 4.0).floor() as i64 + 1, in_bar + 1), egui::FontId::proportional(9.0), theme::TEXT_DIM);
                    }
                    bt += step_beats;
                }
            }
        }
        p.rect_filled(tstrip, 0.0, Color32::from_rgb(20, 22, 27));
        // lane backgrounds
        for (l, r) in lanes.iter().enumerate() {
            let audible = self.mix.lanes.get(l).map(|x| !x.mute).unwrap_or(true);
            p.rect_filled(*r, 3.0, if audible { Color32::from_rgb(20, 22, 26) } else { Color32::from_rgb(16, 16, 18) });
        }

        // ----- clips
        let mut clicked_track = None;
        for i in 0..self.timeline.tracks.len() {
            let (start_sec, end_sec, bar_len) = {
                let tt = &self.timeline.tracks[i];
                (tt.start_sec, tt.end_sec, tt.grid.bar_len())
            };
            let l = i % MIX_LANES;
            let r = Rect::from_min_max(pos2(x_of(start_sec), lanes[l].top() + 2.0), pos2(x_of(end_sec), lanes[l].bottom() - 2.0));
            if r.right() < canvas.left() || r.left() > canvas.right() {
                continue;
            }
            self.draw_clip(&p, r, i, sh, &x_of);
            let cid = Id::new(("clip", i));
            let cr = ui.interact(r.intersect(canvas), cid, Sense::click_and_drag());
            if cr.hovered() {
                ui.ctx().set_cursor_icon(if cr.dragged() { CursorIcon::Grabbing } else { CursorIcon::Grab });
            }
            if cr.clicked() {
                clicked_track = Some(i);
            }
            if cr.drag_started() {
                self.drag = Some(Drag::Clip(i));
                self.drag_acc = 0.0;
                self.selection = Selection::Track(i);
            }
            if cr.dragged() && self.drag == Some(Drag::Clip(i)) {
                self.drag_acc += cr.drag_delta().x;
                if let Some(bars) = self.take_bars(i) {
                    let mt = &mut self.mix.tracks[i];
                    let new_in = (mt.mix_in - bars as f64 * bar_len).max(0.0);
                    if new_in < mt.mix_out - bar_len * 2.0 {
                        mt.mix_in = new_in;
                        self.changed();
                    }
                }
            }
            if cr.drag_stopped() {
                self.drag = None;
            }
            cr.on_hover_text("Drag to slide the track by bars · click to edit");
        }

        // ----- transitions: shaded overlap, markers and handles
        let lane_top = lanes[0].top();
        let lane_bottom = lanes[MIX_LANES - 1].bottom();
        for k in 0..self.timeline.transitions.len() {
            let x = self.timeline.transitions[k].clone();
            let (xs, xe) = (x_of(x.start_sec), x_of(x.end_sec));
            if xe < canvas.left() || xs > canvas.right() {
                continue;
            }
            let sel = self.selection == Selection::Transition(k);
            let tr = &self.mix.transitions[k];
            let style_col = theme::style_color(tr.style);
            let col = if sel { theme::accent() } else { style_col };
            // a frame around both tracks, its header is the chip on the strip (like DJ.Studio)
            let frame = Rect::from_min_max(pos2(xs, tstrip.top() + 1.0), pos2(xe, lane_bottom));
            p.rect_filled(Rect::from_min_max(pos2(xs, lane_top), pos2(xe, lane_bottom)), 0.0, col.linear_multiply(if sel { 0.07 } else { 0.03 }));
            p.rect_stroke(frame, 6.0, Stroke::new(if sel { 2.0 } else { 1.0 }, if sel { Color32::WHITE } else { style_col.linear_multiply(0.55) }), StrokeKind::Inside);
            // label chip on the ruler, coloured by style: the longest label that fits
            let chip = Rect::from_min_max(pos2(xs + 7.0, tstrip.top() + 2.0), pos2((xe - 7.0).max(xs + 24.0), tstrip.bottom() - 2.0));
            if chip.width() > 22.0 {
                p.rect_filled(chip, 4.0, if sel { style_col } else { theme::blend(theme::PANEL, style_col, 0.55) });
                if sel {
                    p.rect_stroke(chip, 4.0, Stroke::new(1.5, theme::accent()), StrokeKind::Outside);
                }
                let bars = tr.beats / 4;
                let abbrev: String = tr.style.label().split_whitespace().filter_map(|w| w.chars().next()).collect::<String>().to_uppercase();
                let font = egui::FontId::proportional(10.5);
                let key = |j: usize| self.mix.tracks.get(j).and_then(|t| sh.analyses.get(&t.track_id).and_then(|a| a.key).map(|kk| theme::key_label(kk.shifted(t.key_shift))));
                let keys = key(k).zip(key(k + 1)).map(|(a, b)| format!(" · {a} ▶ {b}")).unwrap_or_default();
                let no_stems = tr.uses_stems() && !self.stems_ok(sh, k);
                let ns = if no_stems { " · no stems" } else { "" };
                let label = [format!("{} · {bars} bars{keys}{ns}", tr.style.label()), format!("{} · {bars} bars{ns}", tr.style.label()), format!("{} · {bars}", tr.style.label()), format!("{abbrev} {bars}"), format!("{bars}")]
                    .into_iter()
                    .find(|l| ui.fonts_mut(|f| f.layout_no_wrap(l.clone(), font.clone(), Color32::WHITE).size().x) < chip.width() - 8.0)
                    .unwrap_or_default();
                p.with_clip_rect(chip).text(chip.center(), egui::Align2::CENTER_CENTER, label, font, if sel { Color32::BLACK } else { Color32::WHITE });
            }
            let chip_r = ui.interact(chip, Id::new(("tchip", k)), Sense::click());
            if chip_r.clicked() {
                self.selection = Selection::Transition(k);
            }
            chip_r.on_hover_text(if tr.uses_stems() && !self.stems_ok(sh, k) {
                "Click to edit this transition. One of the tracks has no stems yet, so this plays as an EQ blend with the same timing (shown in the clips)."
            } else {
                "Click to edit this transition"
            });
            // handles
            for (end, xx) in [(false, xs), (true, xe)] {
                let hr = Rect::from_center_size(pos2(xx, tstrip.center().y), vec2(10.0, strip_h - 2.0));
                p.rect_filled(hr, 3.0, col);
                p.line_segment([pos2(xx, hr.top() + 5.0), pos2(xx, hr.bottom() - 5.0)], Stroke::new(1.5, Color32::WHITE));
                let id = Id::new(("thandle", k, end));
                let r = ui.interact(hr.expand(3.0), id, Sense::click_and_drag());
                if r.hovered() || r.dragged() {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
                let which = if end { Drag::TransitionEnd(k) } else { Drag::TransitionStart(k) };
                if r.drag_started() {
                    self.drag = Some(which);
                    self.drag_acc = 0.0;
                    self.selection = Selection::Transition(k);
                }
                if r.clicked() {
                    self.selection = Selection::Transition(k);
                }
                if r.dragged() && self.drag == Some(which) {
                    self.drag_acc += r.drag_delta().x;
                    if let Some(bars) = self.take_bars(k) {
                        if end {
                            let t = &mut self.mix.transitions[k];
                            let nb = (t.beats as i32 + bars * 4).clamp(4, 256) as u32;
                            if nb != t.beats {
                                let lanes = if t.style == clubcraft_core::TransitionStyle::Custom {
                                    crate::studio::scale_lanes(&t.lanes, t.beats as f32, nb as f32)
                                } else {
                                    t.style.lanes(nb as f32)
                                };
                                t.beats = nb;
                                t.lanes = lanes;
                                self.changed();
                            }
                        } else {
                            let bar_len = self.timeline.tracks[k].grid.bar_len();
                            let dur = self.timeline.tracks[k].duration;
                            let mt = &mut self.mix.tracks[k];
                            let nv = mt.mix_out + bars as f64 * bar_len;
                            if nv > mt.mix_in + bar_len * 2.0 && nv < dur - bar_len {
                                mt.mix_out = nv;
                                self.changed();
                            }
                        }
                    }
                }
                if r.drag_stopped() {
                    self.drag = None;
                }
                r.on_hover_text(if end { "Drag: transition length (bars)" } else { "Drag: move the transition (bars)" });
            }
        }

        // ----- tempo lane
        p.rect_filled(tempo, 3.0, theme::PANEL);
        if self.timeline.total_beats > 0.0 {
            let mut lo = f64::MAX;
            let mut hi = f64::MIN;
            for t in &self.timeline.tracks {
                lo = lo.min(t.grid.bpm);
                hi = hi.max(t.grid.bpm);
            }
            if let TempoMode::Fixed(b) = self.mix.tempo {
                lo = lo.min(b);
                hi = hi.max(b);
            }
            let (lo, hi) = (lo - 1.5, hi + 1.5);
            let plot = tempo.shrink2(vec2(0.0, 5.0));
            let mut pts = Vec::new();
            let mut xp = tempo.left();
            while xp < tempo.right() {
                let t = ((xp - canvas.left() + scroll) / zoom) as f64;
                if t > self.timeline.total_secs {
                    break;
                }
                let b = self.timeline.bpm_at_beat(self.timeline.beat_at_sec(t));
                pts.push(pos2(xp, plot.bottom() - ((b - lo) / (hi - lo)) as f32 * plot.height()));
                xp += 2.0;
            }
            p.add(egui::Shape::line(pts, Stroke::new(1.5, theme::WARN)));
            for t in &self.timeline.tracks {
                let x = x_of(t.start_sec);
                if x > tempo.left() - 30.0 && x < tempo.right() {
                    p.text(pos2(x + 3.0, tempo.bottom() - 2.0), egui::Align2::LEFT_BOTTOM, format!("{:.0}", t.grid.bpm), egui::FontId::proportional(10.0), theme::TEXT_DIM);
                }
            }
        }

        // ----- playhead
        let xp = x_of(self.position);
        if xp >= canvas.left() && xp <= canvas.right() {
            p.line_segment([pos2(xp, ruler.top()), pos2(xp, tempo.bottom())], Stroke::new(1.5, Color32::WHITE));
            p.add(egui::Shape::convex_polygon(vec![pos2(xp - 6.0, ruler.top()), pos2(xp + 6.0, ruler.top()), pos2(xp, ruler.top() + 8.0)], Color32::WHITE, Stroke::NONE));
        }
        if let Some(i) = clicked_track {
            self.selection = Selection::Track(i);
        }
        // seek: click on the ruler / tempo lane / empty canvas
        if resp.clicked() || (resp.dragged() && self.drag.is_none()) {
            if let Some(pp) = resp.interact_pointer_pos() {
                let t = ((pp.x - canvas.left() + scroll) / zoom) as f64;
                out.push(StudioAction::Seek(t.clamp(0.0, self.timeline.total_secs)));
            }
        }
    }

    /// Convert accumulated drag pixels into whole bars at the tempo of track `i`.
    fn take_bars(&mut self, i: usize) -> Option<i32> {
        let tt = self.timeline.tracks.get(i)?;
        let px_per_bar = (tt.grid.bar_len() as f32 * self.zoom).max(1.0);
        let bars = (self.drag_acc / px_per_bar).trunc() as i32;
        if bars != 0 {
            self.drag_acc -= bars as f32 * px_per_bar;
            Some(bars)
        } else {
            None
        }
    }

    /// Lane header with a small mixer. Returns true if the lane settings changed.
    fn lane_header(&mut self, ui: &mut Ui, r: Rect, l: usize) -> bool {
        let accent = theme::deck_color(l);
        ui.painter().rect_filled(r, 4.0, theme::PANEL);
        ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(3.0, r.height())), 2.0, accent);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(vec2(10.0, 6.0))).layout(Layout::top_down(Align::Min)));
        let ui = &mut child;
        let mut ls: LaneSettings = self.mix.lanes.get(l).copied().unwrap_or_default();
        let before = ls;
        ui.horizontal(|ui| {
            let chevron = if self.lane_collapsed[l] { "⏵" } else { "⏷" };
            if ui.add(egui::Label::new(RichText::new(format!("{chevron} Deck {}", l + 1)).strong().color(accent)).sense(Sense::click())).on_hover_text("Collapse / expand the lane").clicked() {
                self.lane_collapsed[l] = !self.lane_collapsed[l];
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::toggle(ui, ls.mute, "M", Color32::from_rgb(255, 90, 90), vec2(22.0, 18.0)).on_hover_text("Mute lane").clicked() {
                    ls.mute = !ls.mute;
                }
                if widgets::toggle(ui, ls.solo, "S", theme::WARN, vec2(22.0, 18.0)).on_hover_text("Solo lane").clicked() {
                    ls.solo = !ls.solo;
                }
            });
        });
        if r.height() >= 56.0 {
        ui.horizontal(|ui| {
                let mut v = db_fader(ls.gain_db);
                if widgets::hfader_with_default(ui, &mut v, false, 0.8, Some(0.8), vec2(130.0, 18.0), accent).on_hover_text("Lane volume · double-click: 0 dB").changed() {
                    ls.gain_db = fader_db(v);
                    if ls.gain_db.abs() < 0.3 {
                        ls.gain_db = 0.0;
                    }
                }
                ui.label(RichText::new(format!("{:+.1} dB", ls.gain_db)).size(10.5).color(theme::TEXT_DIM));
            });
        }
        if r.height() >= 92.0 {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for (b, name) in [(2usize, "High"), (1, "Mid"), (0, "Low")] {
                    let col = if ls.eq[b] < 0.02 { Color32::from_rgb(255, 70, 70) } else { EQ_COLORS[b] };
                    widgets::knob(ui, &mut ls.eq[b], 0.0..=1.0, 0.5, name, col, 26.0);
                }
                widgets::knob(ui, &mut ls.filter, 0.0..=1.0, 0.5, "Filter", FILTER_COLOR, 26.0);
            });
        }
        if r.height() >= 128.0 {
            let mut show = self.lane_show[l];
            egui::ComboBox::from_id_salt(("show", l)).width(150.0).selected_text(show.label()).show_ui(ui, |ui| {
                for m in ShowMode::ALL {
                    ui.selectable_value(&mut show, m, m.label());
                }
            });
            self.lane_show[l] = show;
        }
        if ls != before {
            if self.mix.lanes.len() <= l {
                self.mix.lanes.resize(MIX_LANES, LaneSettings::default());
            }
            self.mix.lanes[l] = ls;
            return true;
        }
        false
    }

    fn draw_clip(&self, p: &egui::Painter, r: Rect, i: usize, sh: &Shared, x_of: &dyn Fn(f64) -> f32) {
        let tt = &self.timeline.tracks[i];
        let l = i % MIX_LANES;
        let accent = theme::deck_color(l);
        let sel = self.selection == Selection::Track(i);
        let audible = self.mix.lane_audible(i);
        let a = sh.analyses.get(&tt.track_id);
        let header = Rect::from_min_size(r.min, vec2(r.width(), 20.0f32.min(r.height())));
        let body = Rect::from_min_max(pos2(r.left(), header.bottom()), r.max);
        p.rect_filled(r, 4.0, Color32::from_rgb(30, 32, 38));
        // title bar tinted with the deck colour of its lane
        p.rect_filled(header, egui::CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }, theme::blend(theme::PANEL, accent, if audible { 0.3 } else { 0.14 }));
        // key badge + title (kept visible when the clip is scrolled partly off screen)
        let mut tx = header.left().max(p.clip_rect().left()) + 4.0;
        if let Some(k) = a.and_then(|a| a.key) {
            let k = k.shifted(self.mix.tracks[i].key_shift);
            let badge = Rect::from_min_size(pos2(tx, header.top() + 2.0), vec2(30.0, 16.0));
            p.rect_filled(badge, 3.0, theme::key_color(k));
            p.text(badge.center(), egui::Align2::CENTER_CENTER, crate::theme::key_label(k), egui::FontId::proportional(10.5), Color32::BLACK);
            tx = badge.right() + 5.0;
        }
        let name = sh.track(tt.track_id).map(|t| t.title.clone()).unwrap_or_default();
        let max_chars = ((header.right() - tx) / 6.5).max(0.0) as usize;
        p.text(pos2(tx, header.center().y), egui::Align2::LEFT_CENTER, crate::studio::truncate(&format!("{}. {}", i + 1, name), max_chars), egui::FontId::proportional(11.5), theme::TEXT);
        if r.height() < 40.0 {
            // collapsed lane: title bar only
            if !audible {
                p.rect_filled(r, 4.0, Color32::from_black_alpha(110));
            }
            p.rect_stroke(r, 4.0, Stroke::new(if sel { 2.0 } else { 1.0 }, if sel { theme::accent() } else { accent.linear_multiply(0.5) }), StrokeKind::Inside);
            return;
        }
        // beat ruler (the track's own bar numbers) and alternating phrase shading
        let ruler = Rect::from_min_size(body.min, vec2(body.width(), RULER_H.min(body.height() * 0.3)));
        let body = Rect::from_min_max(pos2(body.left(), ruler.bottom()), body.max);
        self.draw_clip_ruler(p, ruler, body, i, x_of);
        // waveform
        // Waveform. Inside transitions it follows what plays: stems fade with their lanes and the
        // height follows the volume, so both tracks stay visible through the whole transition.
        if let (Some(wf), Some(a)) = (sh.waveforms.get(&tt.track_id), a) {
            let wrect = body.shrink2(vec2(0.0, 3.0));
            let clip = p.clip_rect();
            let paint = |b0: f64, b1: f64, gains: [f32; 4], volume: f32| {
                let (x0, x1) = (x_of(self.timeline.sec_at_beat(b0)), x_of(self.timeline.sec_at_beat(b1)));
                if x1 < clip.left() || x0 > clip.right() || x1 - x0 < 0.25 {
                    return;
                }
                let wp = WaveParams { wf, grid: a.grid, cues: &[], sections: &[], style: WaveStyle::Stems, stem_gains: if audible { gains } else { [0.0; 4] }, loop_region: None, accent, volume };
                let r = Rect::from_x_y_ranges(x0..=x1, wrect.y_range());
                waveform::paint_wave(p, r, &wp, self.timeline.track_pos(i, b0), self.timeline.track_pos(i, b1), true);
            };
            let mut regions: Vec<(f64, f64, Transition, Side)> = Vec::new();
            if i > 0 {
                if let (Some(x), Some(tr)) = (self.timeline.transitions.get(i - 1), self.effective_transition(sh, i - 1)) {
                    regions.push((x.start_beat, x.start_beat + x.beats, tr, Side::In));
                }
            }
            if let (Some(x), Some(tr)) = (self.timeline.transitions.get(i), self.effective_transition(sh, i)) {
                regions.push((x.start_beat, x.start_beat + x.beats, tr, Side::Out));
            }
            let mut b = tt.start_beat;
            for (start, end, tr, side) in &regions {
                let (start, r0, r1) = (*start, start.max(tt.start_beat), end.min(tt.end_beat));
                if r0 > b {
                    paint(b, r0, [1.0; 4], 1.0);
                }
                let (x0, x1) = (x_of(self.timeline.sec_at_beat(r0)), x_of(self.timeline.sec_at_beat(r1)));
                let n = ((x1 - x0) / 4.0).clamp(4.0, 400.0) as usize;
                for s in 0..n {
                    let (s0, s1) = (r0 + (r1 - r0) * s as f64 / n as f64, r0 + (r1 - r0) * (s + 1) as f64 / n as f64);
                    let xb = ((s0 + s1) * 0.5 - start) as f32;
                    let gains = [StemKind::Drums, StemKind::Bass, StemKind::Other, StemKind::Vocals].map(|k| tr.value(*side, Param::Stem(k), xb).clamp(0.0, 1.0));
                    paint(s0, s1, gains, tr.value(*side, Param::Volume, xb).clamp(0.0, 1.0));
                }
                b = b.max(r1);
            }
            if b < tt.end_beat {
                paint(b, tt.end_beat, [1.0; 4], 1.0);
            }
        }
        let bar_px = (tt.grid.bar_len() as f32 * self.zoom).max(0.1);
        if bar_px >= 60.0 {
            // beat lines when zoomed in
            let mut b = tt.start_beat.ceil();
            while b < tt.end_beat {
                let x = x_of(self.timeline.sec_at_beat(b));
                if x >= p.clip_rect().left() && x <= p.clip_rect().right() {
                    p.line_segment([pos2(x, body.top()), pos2(x, body.bottom())], Stroke::new(1.0, Color32::from_white_alpha(18)));
                }
                b += 1.0;
            }
        }
        // section markers from the analysis (drop / break / vocal)
        if let Some(a) = a {
            for c in a.cues.iter().filter(|c| matches!(c.kind, CueKind::Drop | CueKind::Breakdown | CueKind::VocalIn)) {
                let mb = self.timeline.beat_of_track_pos(i, c.pos);
                if mb < tt.start_beat || mb > tt.end_beat {
                    continue;
                }
                let x = x_of(self.timeline.sec_at_beat(mb));
                p.text(pos2(x + 2.0, body.bottom() - 2.0), egui::Align2::LEFT_BOTTOM, c.kind.label(), egui::FontId::proportional(9.5), theme::rgb(c.color));
                p.line_segment([pos2(x, body.bottom() - 14.0), pos2(x, body.bottom())], Stroke::new(1.0, theme::rgb(c.color)));
            }
        }
        // inline transition automation
        let show = self.lane_show[l];
        if show != ShowMode::None {
            if i > 0 {
                if let (Some(x), Some(tr)) = (self.timeline.transitions.get(i - 1), self.effective_transition(sh, i - 1)) {
                    self.draw_automation(p, body, &tr, Side::In, x.start_beat, x.beats, show, x_of);
                }
            }
            if let (Some(x), Some(tr)) = (self.timeline.transitions.get(i), self.effective_transition(sh, i)) {
                self.draw_automation(p, body, &tr, Side::Out, x.start_beat, x.beats, show, x_of);
            }
        }
        if !audible {
            p.rect_filled(r, 4.0, Color32::from_black_alpha(110));
        }
        p.rect_stroke(r, 4.0, Stroke::new(if sel { 2.0 } else { 1.0 }, if sel { theme::accent() } else { accent.linear_multiply(0.5) }), StrokeKind::Inside);
    }

    /// Both tracks of transition `k` have stems.
    pub(crate) fn stems_ok(&self, sh: &Shared, k: usize) -> bool {
        let has = |i: usize| self.mix.tracks.get(i).is_some_and(|t| sh.analyses.get(&t.track_id).is_some_and(|a| a.stems_model.is_some()));
        has(k) && has(k + 1)
    }

    /// Transition `k` as it plays (stem lanes fall back to an EQ blend without stems).
    pub(crate) fn effective_transition(&self, sh: &Shared, k: usize) -> Option<Transition> {
        let tr = self.mix.transitions.get(k)?;
        Some(if tr.uses_stems() && !self.stems_ok(sh, k) { tr.without_stems() } else { tr.clone() })
    }

    /// Ruler strip at the top of a clip: bar numbers of the track (bar 1 = first downbeat), bar and
    /// beat ticks by zoom level, plus alternating shading of 4-bar phrases behind the waveform.
    fn draw_clip_ruler(&self, p: &egui::Painter, ruler: Rect, body: Rect, i: usize, x_of: &dyn Fn(f64) -> f32) {
        let tt = &self.timeline.tracks[i];
        let bpb = tt.grid.beats_per_bar.max(1) as f64;
        let beat_px = (tt.grid.beat_len() as f32 * self.zoom).max(0.01);
        let bar_px = beat_px * bpb as f32;
        p.rect_filled(ruler, 0.0, Color32::from_rgb(24, 25, 30));
        let clip = p.clip_rect().intersect(ruler.union(body));
        let p = &p.with_clip_rect(clip); // labels near the clip end must not spill over
        // track beat (relative to the downbeat) <-> mix beat
        let to_mix = |tb: f64| tb - tt.track_beat0 + tt.start_beat;
        let tb0 = tt.track_beat0;
        let tb1 = tt.track_beat0 + (tt.end_beat - tt.start_beat);
        let x_tb = |tb: f64| x_of(self.timeline.sec_at_beat(to_mix(tb)));
        // labels every `label` bars, at least ~44 px apart
        let label = [1u32, 2, 4, 8, 16, 32, 64, 128].into_iter().find(|&n| bar_px * n as f32 >= 44.0).unwrap_or(256) as f64;
        // phrase shading: alternating bands of max(4, label) bars
        let band = label.max(4.0) * bpb;
        if band as f32 * beat_px >= 10.0 {
            let mut b = (tb0 / band).floor() * band;
            while b < tb1 {
                if ((b / band).round() as i64).rem_euclid(2) == 1 {
                    let (x0, x1) = (x_tb(b.max(tb0)).max(clip.left()), x_tb((b + band).min(tb1)).min(clip.right()));
                    if x1 > x0 {
                        p.rect_filled(Rect::from_x_y_ranges(x0..=x1, body.top()..=body.bottom()), 0.0, Color32::from_white_alpha(7));
                    }
                }
                b += band;
            }
        }
        // ticks: 16ths, beats or bars, whichever are far enough apart (else every `label / 4` bars)
        let step = if beat_px >= 48.0 { 0.25 } else if beat_px >= 6.0 { 1.0 } else if bar_px >= 6.0 { bpb } else { (label / 4.0).max(1.0) * bpb };
        let mut b = (tb0 / step).ceil() * step;
        while b < tb1 {
            let x = x_tb(b);
            if x >= clip.left() - 1.0 && x <= clip.right() + 1.0 {
                let bar = (b / bpb).round();
                let on_bar = (b / bpb - bar).abs() < 1e-6;
                let labelled = on_bar && (bar / label - (bar / label).round()).abs() < 1e-6;
                let on_beat = (b - b.round()).abs() < 1e-6;
                let (h, a) = if labelled {
                    (ruler.height(), 150)
                } else if on_bar {
                    (ruler.height() * 0.45, 90)
                } else if on_beat {
                    (ruler.height() * 0.25, 55)
                } else {
                    (ruler.height() * 0.12, 35)
                };
                p.line_segment([pos2(x, ruler.bottom() - h), pos2(x, ruler.bottom())], Stroke::new(1.0, Color32::from_white_alpha(a)));
                if labelled {
                    p.text(pos2(x + 2.0, ruler.center().y), egui::Align2::LEFT_CENTER, format!("{}", bar as i64 + 1), egui::FontId::proportional(9.5), Color32::from_white_alpha(170));
                }
            }
            b += step;
        }
        p.line_segment([ruler.left_bottom(), ruler.right_bottom()], Stroke::new(1.0, Color32::from_white_alpha(14)));
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_automation(&self, p: &egui::Painter, body: Rect, tr: &Transition, side: Side, start_beat: f64, beats: f64, show: ShowMode, x_of: &dyn Fn(f64) -> f32) {
        let steps = 48;
        let xs = |f: f32| x_of(self.timeline.sec_at_beat(start_beat + f as f64 * beats));
        let has = |param: Param| tr.lanes.iter().any(|l| l.side == side && l.param == param && l.points.iter().any(|p| (p.1 - param.neutral()).abs() > 1e-3));
        let curve = |param: Param, col: Color32, centered: bool| {
            let mut pts = Vec::with_capacity(steps + 1);
            for s in 0..=steps {
                let f = s as f32 / steps as f32;
                let v = tr.value(side, param, f * beats as f32).clamp(0.0, 1.0);
                let y = if centered { body.center().y - (v - 0.5) * (body.height() - 8.0) } else { body.bottom() - 3.0 - v * (body.height() - 8.0) };
                pts.push(pos2(xs(f), y));
            }
            p.add(egui::Shape::line(pts, Stroke::new(2.0, col)));
        };
        if matches!(show, ShowMode::All | ShowMode::Volume) && has(Param::Volume) {
            curve(Param::Volume, Color32::from_white_alpha(235), false);
        }
        if matches!(show, ShowMode::Eq) {
            for (b, param) in [Param::EqLow, Param::EqMid, Param::EqHigh].into_iter().enumerate() {
                if has(param) {
                    curve(param, EQ_COLORS[b], true);
                }
            }
            if has(Param::Filter) {
                curve(Param::Filter, FILTER_COLOR, true);
            }
        }
        if matches!(show, ShowMode::All | ShowMode::Stems) {
            let stems: Vec<StemKind> = [StemKind::Vocals, StemKind::Other, StemKind::Bass, StemKind::Drums].into_iter().filter(|k| tr.lanes.iter().any(|l| l.side == side && l.param == Param::Stem(*k))).collect();
            if !stems.is_empty() {
                let sh = 7.0;
                let top = body.bottom() - sh * stems.len() as f32 - 2.0;
                for (j, k) in stems.iter().enumerate() {
                    let y = top + j as f32 * sh;
                    let col = theme::stem_color(*k);
                    for s in 0..steps {
                        let f0 = s as f32 / steps as f32;
                        let f1 = (s + 1) as f32 / steps as f32;
                        let v = tr.value(side, Param::Stem(*k), (f0 + f1) * 0.5 * beats as f32).clamp(0.0, 1.0);
                        p.rect_filled(Rect::from_min_max(pos2(xs(f0), y), pos2(xs(f1) + 0.5, y + sh - 1.0)), 0.0, col.linear_multiply(0.15 + 0.85 * v));
                    }
                }
            }
        }
    }
}
