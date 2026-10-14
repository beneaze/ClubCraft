//! Studio timeline: lane headers (mixer per lane), clips with inline transition automation,
//! draggable transition markers and the tempo lane.

use crate::app::Shared;
use crate::studio::{Selection, StudioAction, StudioView};
use crate::theme;
use crate::waveform::{self, WaveParams, WaveStyle};
use crate::widgets;
use clubcraft_core::{CueKind, LaneSettings, Param, SampleClip, Side, StemKind, TempoMode, Transition, MIX_LANES, SAMPLE_LANES};
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

/// What the timeline draws and edits inside the selected transition (like DJ.Studio's
/// automation choice): one row per stem, or one curve for the volume, an EQ band or the filter.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CurveTarget {
    #[default]
    Stems,
    Volume,
    EqLow,
    EqMid,
    EqHigh,
    Filter,
}

impl CurveTarget {
    pub const ALL: [CurveTarget; 6] = [CurveTarget::Stems, CurveTarget::Volume, CurveTarget::EqLow, CurveTarget::EqMid, CurveTarget::EqHigh, CurveTarget::Filter];
    pub fn label(self) -> &'static str {
        match self {
            CurveTarget::Stems => "Stems",
            CurveTarget::Volume => "Volume",
            CurveTarget::EqLow => "Low",
            CurveTarget::EqMid => "Mid",
            CurveTarget::EqHigh => "High",
            CurveTarget::Filter => "Filter",
        }
    }
    fn param(self) -> Option<Param> {
        match self {
            CurveTarget::Stems => None,
            CurveTarget::Volume => Some(Param::Volume),
            CurveTarget::EqLow => Some(Param::EqLow),
            CurveTarget::EqMid => Some(Param::EqMid),
            CurveTarget::EqHigh => Some(Param::EqHigh),
            CurveTarget::Filter => Some(Param::Filter),
        }
    }
    fn color(self) -> Color32 {
        match self {
            CurveTarget::Stems | CurveTarget::Volume => VOLUME_COLOR,
            CurveTarget::EqLow => EQ_COLORS[0],
            CurveTarget::EqMid => EQ_COLORS[1],
            CurveTarget::EqHigh => EQ_COLORS[2],
            CurveTarget::Filter => FILTER_COLOR,
        }
    }
}

/// Stem rows from top to bottom (as in DJ.Studio).
pub const STEM_ROWS: [StemKind; 4] = [StemKind::Drums, StemKind::Bass, StemKind::Other, StemKind::Vocals];

/// The waveform area of a clip drawn in `r` (below its title bar and beat ruler).
pub fn clip_wave_rect(r: Rect) -> Rect {
    let body = Rect::from_min_max(pos2(r.left(), r.top() + 20.0f32.min(r.height())), r.max);
    let ruler_h = RULER_H.min(body.height() * 0.3);
    Rect::from_min_max(pos2(body.left(), body.top() + ruler_h), body.max).shrink2(vec2(0.0, 3.0))
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Drag {
    /// Moving transition `k` by its bar.
    TransitionMove(usize),
    TransitionStart(usize),
    TransitionEnd(usize),
    Clip(usize),
    /// Selecting a time range on track `i`'s waveform.
    Range(usize),
}

/// A selected time range of one track (mix beats), for extracting stems / samples.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RangeSel {
    pub track: usize,
    pub b0: f64,
    pub b1: f64,
}

pub const HEADER_W: f32 = 214.0;
/// Zoom range of the studio timeline in pixels per second (3000 px/s ≈ one beat over 1500 px).
pub const MIN_ZOOM: f32 = 0.2;
/// Height of the beat ruler at the top of each clip.
const RULER_H: f32 = 14.0;
pub const MAX_ZOOM: f32 = 3000.0;
pub const EQ_COLORS: [Color32; 3] = [Color32::from_rgb(255, 90, 90), Color32::from_rgb(120, 230, 120), Color32::from_rgb(110, 200, 255)];
pub const FILTER_COLOR: Color32 = Color32::from_rgb(200, 140, 255);
/// Colour of the sample lanes.
pub const SAMPLE_COLOR: Color32 = Color32::from_rgb(120, 200, 190);
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
        // Collapsed lanes are a slim row (clip titles only); expanded lanes share the rest.
        const COLLAPSED_H: f32 = 30.0;
        const SAMPLE_H: f32 = 58.0;
        let n_open = (0..MIX_LANES).filter(|l| !self.lane_collapsed[*l]).count().max(1) as f32;
        let n_closed = MIX_LANES as f32 - (0..MIX_LANES).filter(|l| !self.lane_collapsed[*l]).count() as f32;
        // editing a transition's stems needs room for four rows per track: the tempo and sample
        // lanes give way first
        let stem_edit = matches!(self.selection, Selection::Transition(_)) && self.curve_target == CurveTarget::Stems;
        let want = if stem_edit { 150.0 } else { 70.0 };
        let layout = |squeeze: bool| {
            let tempo_h = if self.tempo_open && !squeeze { 58.0 } else { 24.0 };
            // empty sample lanes stay slim (they still take dropped files)
            let sample_hs: [f32; SAMPLE_LANES] = std::array::from_fn(|l| if self.sample_open[l] && !squeeze && self.mix.samples.iter().any(|c| c.lane as usize == l) { SAMPLE_H } else { COLLAPSED_H });
            let samples_h: f32 = sample_hs.iter().map(|h| h + 2.0).sum();
            let open = (full.height() - ruler_h - 21.0 - tempo_h - 8.0 - samples_h - n_closed * (COLLAPSED_H + 2.0)) / n_open;
            (tempo_h, sample_hs, open)
        };
        let (mut tempo_h, mut sample_hs, mut open) = layout(false);
        if open < want {
            (tempo_h, sample_hs, open) = layout(true);
        }
        let open_h = open.clamp(70.0, if stem_edit { 420.0 } else { 300.0 });
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
        let mut slanes: Vec<Rect> = Vec::with_capacity(SAMPLE_LANES);
        for h in sample_hs {
            slanes.push(Rect::from_min_size(pos2(canvas.left(), ly), vec2(canvas.width(), h)));
            ly += h + 2.0;
        }
        let tempo = Rect::from_min_size(pos2(canvas.left(), slanes[SAMPLE_LANES - 1].bottom() + 4.0), vec2(canvas.width(), tempo_h));

        // ----- lane headers
        let mut lane_changed = false;
        {
            // like DJ.Studio: playhead time over the mix length, the track playing, its key
            let corner = Rect::from_min_size(full.min, vec2(HEADER_W, ruler_h + 1.0 + strip_h));
            let p = ui.painter();
            p.rect_filled(corner, 4.0, theme::PANEL);
            let x = corner.left() + 10.0;
            p.text(pos2(x, corner.top() + 3.0), egui::Align2::LEFT_TOP, theme::fmt_time_short(self.position), egui::FontId::monospace(15.0), theme::TEXT);
            p.text(pos2(x, corner.bottom() - 3.0), egui::Align2::LEFT_BOTTOM, theme::fmt_time_short(self.timeline.total_secs), egui::FontId::monospace(10.5), theme::TEXT_DIM);
            let cur = self.track_at_playhead();
            let x2 = x + 64.0;
            p.line_segment([pos2(x2 - 8.0, corner.top() + 6.0), pos2(x2 - 8.0, corner.bottom() - 6.0)], Stroke::new(1.0, theme::LINE));
            p.text(pos2(x2, corner.top() + 3.0), egui::Align2::LEFT_TOP, format!("{}/{}", cur.map(|i| i + 1).unwrap_or(0), self.mix.tracks.len()), egui::FontId::monospace(15.0), theme::TEXT);
            p.text(pos2(x2, corner.bottom() - 3.0), egui::Align2::LEFT_BOTTOM, "TRACKS", egui::FontId::proportional(10.0), theme::TEXT_DIM);
            if let Some(k) = cur.and_then(|i| self.mix.tracks.get(i)).and_then(|t| sh.analyses.get(&t.track_id).and_then(|a| a.key).map(|k| k.shifted(t.key_shift))) {
                let badge = Rect::from_min_size(pos2(corner.right() - 62.0, corner.top() + 5.0), vec2(54.0, corner.height() - 10.0));
                p.rect_stroke(badge, 5.0, Stroke::new(1.5, theme::key_color(k)), StrokeKind::Inside);
                p.text(pos2(badge.center().x, badge.top() + 2.0), egui::Align2::CENTER_TOP, theme::key_label(k), egui::FontId::proportional(13.0), theme::key_color(k));
                p.text(pos2(badge.center().x, badge.bottom() - 1.0), egui::Align2::CENTER_BOTTOM, k.name(), egui::FontId::proportional(10.0), theme::key_color(k));
            }
        }
        for l in 0..MIX_LANES {
            let hr = Rect::from_min_size(pos2(full.left(), lanes[l].top()), vec2(HEADER_W, lane_hs[l]));
            lane_changed |= self.lane_header(ui, hr, l);
        }
        for l in 0..SAMPLE_LANES {
            let hr = Rect::from_min_size(pos2(full.left(), slanes[l].top()), vec2(HEADER_W, slanes[l].height()));
            lane_changed |= self.sample_lane_header(ui, hr, l);
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

        // ----- ruler: time only (like DJ.Studio); bars are on each clip's own ruler
        p.rect_filled(ruler, 0.0, theme::PANEL);
        if self.timeline.total_secs > 0.0 {
            let steps = [0.1f64, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0];
            let major = steps.into_iter().find(|s| *s as f32 * zoom >= 70.0).unwrap_or(1200.0);
            let minor = match major {
                m if m >= 60.0 && (m / 60.0).fract() == 0.0 => major / 6.0,
                15.0 => 5.0,
                m => m / 5.0,
            };
            let (t_lo, t_hi) = ((scroll / zoom) as f64, ((canvas.width() + scroll) / zoom) as f64);
            let label = |t: f64| if major < 1.0 { theme::fmt_time(t) } else { theme::fmt_time_short(t) };
            if minor as f32 * zoom >= 5.0 {
                let mut t = (t_lo / minor).floor() * minor;
                while t <= t_hi.min(self.timeline.total_secs + minor) {
                    let x = x_of(t);
                    p.line_segment([pos2(x, ruler.bottom() - 5.0), pos2(x, ruler.bottom())], Stroke::new(1.0, theme::LINE));
                    t += minor;
                }
            }
            let mut t = (t_lo / major).floor() * major;
            while t <= t_hi.min(self.timeline.total_secs + major) {
                let x = x_of(t);
                p.line_segment([pos2(x, ruler.bottom() - 11.0), pos2(x, ruler.bottom())], Stroke::new(1.0, Color32::from_white_alpha(150)));
                p.text(pos2(x, ruler.top() + 1.0), egui::Align2::CENTER_TOP, label(t.max(0.0)), egui::FontId::proportional(12.0), theme::TEXT);
                t += major;
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
            // selected range (yellow, like DJ.Studio)
            if let Some(rs) = self.range.filter(|rs| rs.track == i) {
                let (x0, x1) = (x_of(self.timeline.sec_at_beat(rs.b0)), x_of(self.timeline.sec_at_beat(rs.b1)));
                let rr = Rect::from_x_y_ranges(x0..=x1.max(x0 + 1.0), (r.top() + 20.0)..=r.bottom());
                p.rect_filled(rr, 0.0, Color32::from_rgba_unmultiplied(210, 200, 60, 70));
                p.rect_stroke(rr, 0.0, Stroke::new(1.0, Color32::from_rgb(220, 210, 80)), StrokeKind::Inside);
            }
            let cid = Id::new(("clip", i));
            let cr = ui.interact(r.intersect(canvas), cid, Sense::click_and_drag());
            // the title bar moves the track, the waveform selects a range
            let press_y = ui.input(|inp| inp.pointer.press_origin()).map(|q| q.y);
            let on_header = press_y.is_some_and(|y| y < r.top() + 20.0);
            if cr.hovered() {
                let over_header = ui.input(|inp| inp.pointer.hover_pos()).is_some_and(|q| q.y < r.top() + 20.0);
                ui.ctx().set_cursor_icon(if self.drag == Some(Drag::Clip(i)) { CursorIcon::Grabbing } else if over_header { CursorIcon::Grab } else { CursorIcon::Text });
            }
            // inside the selected transition the clicks belong to it: the transition stays selected
            let in_sel_transition = match self.selection {
                Selection::Transition(k) if k == i || k + 1 == i => self.timeline.transitions.get(k).is_some_and(|x| {
                    let px = ui.input(|inp| inp.pointer.press_origin()).map(|q| q.x);
                    px.is_some_and(|px| px >= x_of(x.start_sec) && px <= x_of(x.end_sec))
                }),
                _ => false,
            };
            let keeps_transition = matches!(self.selection, Selection::Transition(k) if k == i || k + 1 == i);
            if cr.clicked() {
                if !in_sel_transition {
                    clicked_track = Some(i);
                }
                self.range = None;
            }
            // sliding a track moves its incoming transition: not while that one is locked
            let in_locked = i > 0 && self.mix.transitions.get(i - 1).is_some_and(|t| t.locked);
            let zoom = self.zoom;
            let origin = x_of(0.0);
            let snap_beat = |tl: &clubcraft_core::MixTimeline, x: f32, free: bool| {
                let b = tl.beat_at_sec((((x - origin) / zoom) as f64).max(0.0));
                if free { b } else { b.round() }
            };
            if cr.drag_started() && !on_header && !in_sel_transition {
                let free = ui.input(|inp| inp.modifiers.alt);
                let x = ui.input(|inp| inp.pointer.press_origin()).map(|q| q.x).unwrap_or(r.left());
                let (lo, hi) = (self.timeline.tracks[i].start_beat, self.timeline.tracks[i].end_beat);
                let a = snap_beat(&self.timeline, x, free).clamp(lo, hi);
                self.range_anchor = a;
                self.range = Some(RangeSel { track: i, b0: a, b1: a });
                self.drag = Some(Drag::Range(i));
            }
            if cr.dragged() && self.drag == Some(Drag::Range(i)) {
                if let Some(q) = ui.input(|inp| inp.pointer.latest_pos()) {
                    let free = ui.input(|inp| inp.modifiers.alt);
                    let (lo, hi) = (self.timeline.tracks[i].start_beat, self.timeline.tracks[i].end_beat);
                    let b = snap_beat(&self.timeline, q.x, free).clamp(lo, hi);
                    let a = self.range_anchor;
                    self.range = Some(RangeSel { track: i, b0: a.min(b), b1: a.max(b) });
                }
            }
            // right-click: extract the selection
            let has_range = self.range.is_some_and(|rs| rs.track == i && rs.b1 > rs.b0);
            let has_stems = sh.analyses.get(&self.mix.tracks[i].track_id).is_some_and(|a| a.stems_model.is_some());
            cr.context_menu(|ui| {
                ui.set_min_width(260.0);
                if !has_range {
                    ui.label(RichText::new("Drag across the waveform to select a range, then right-click to switch stems off in it, copy it to a sample lane or extract a stem.").size(11.5).color(theme::TEXT_DIM));
                    return;
                }
                use crate::extract::Part;
                if ui.add(egui::Button::new("Copy to sample lane").shortcut_text("⌘L")).clicked() {
                    self.extract_requests.push((Part::Full, false));
                    ui.close();
                }
                ui.separator();
                for (n, part) in [Part::Drums, Part::Bass, Part::Melody, Part::Vocals, Part::Instrumental, Part::DrumsBass].into_iter().enumerate() {
                    let b = egui::Button::new(format!("Copy {}", part.label().to_lowercase())).shortcut_text(format!("⇧⌘{}", n + 1));
                    if ui.add_enabled(has_stems, b).on_disabled_hover_text("Needs stems: separate them first").clicked() {
                        self.extract_requests.push((part, false));
                        ui.close();
                    }
                }
                if !has_stems && ui.button("Separate stems for this track").clicked() {
                    self.pending.push(StudioAction::SeparateStems(vec![self.mix.tracks[i].track_id]));
                    ui.close();
                }
                ui.separator();
                ui.menu_button("Save sample…", |ui| {
                    for part in Part::ALL {
                        if ui.add_enabled(part == Part::Full || has_stems, egui::Button::new(part.label())).clicked() {
                            self.extract_requests.push((part, true));
                            ui.close();
                        }
                    }
                });
                // switch stems off (or back on) for this part of the song
                if let Some(rs) = self.range.filter(|rs| rs.track == i) {
                    let (t0, t1) = (self.timeline.track_pos(i, rs.b0), self.timeline.track_pos(i, rs.b1));
                    let mid = (t0 + t1) / 2.0;
                    ui.menu_button("Stems in selection", |ui| {
                        ui.set_min_width(200.0);
                        let mut set: Option<[Option<f32>; 4]> = None;
                        for (n, stem) in STEM_ROWS.into_iter().enumerate() {
                            let on = self.mix.tracks[i].stem_at(stem, mid).unwrap_or(1.0) > 0.5;
                            if ui.button(format!("{} {}", if on { "Mute" } else { "Unmute" }, stem.label().to_lowercase())).clicked() {
                                let mut v = [None; 4];
                                v[n] = Some(if on { 0.0 } else { 1.0 });
                                set = Some(v);
                            }
                        }
                        ui.separator();
                        if ui.button("Acapella (vocals only)").clicked() {
                            set = Some([Some(0.0), Some(0.0), Some(0.0), Some(1.0)]);
                        }
                        if ui.button("Instrumental (no vocals)").clicked() {
                            set = Some([Some(1.0), Some(1.0), Some(1.0), Some(0.0)]);
                        }
                        if ui.button("All stems on").clicked() {
                            set = Some([Some(1.0); 4]);
                        }
                        if let Some(v) = set {
                            for (n, g) in v.into_iter().enumerate() {
                                if let Some(g) = g {
                                    self.mix.tracks[i].set_stem_range(STEM_ROWS[n], t0, t1, g);
                                }
                            }
                            self.changed();
                            ui.close();
                        }
                        if !has_stems {
                            ui.label(RichText::new("No stems yet: imitated with volume and EQ until you separate them.").size(11.0).color(theme::TEXT_DIM));
                        }
                    });
                }
                if ui.add(egui::Button::new("Clear selection").shortcut_text("Esc")).clicked() {
                    self.range = None;
                    ui.close();
                }
            });
            if cr.drag_started() && on_header && !in_locked {
                self.drag = Some(Drag::Clip(i));
                self.drag_acc = 0.0;
                // sliding a track of the selected transition keeps that transition selected
                if !keeps_transition {
                    self.selection = Selection::Track(i);
                }
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
            cr.on_hover_text(if in_locked {
                "Click to edit · drag on the waveform to select a range (right-click to extract) · the transition into this track is locked"
            } else {
                "Drag the title bar to slide the track · drag on the waveform to select a range, then right-click to switch stems off in it or extract them · click to edit"
            });
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
                    .find(|l| ui.fonts_mut(|f| f.layout_no_wrap(l.clone(), font.clone(), Color32::WHITE).size().x) < chip.width() - 30.0)
                    .unwrap_or_default();
                p.with_clip_rect(chip).text(chip.center() - vec2(9.0, 0.0), egui::Align2::CENTER_CENTER, label, font, if sel { Color32::BLACK } else { Color32::WHITE });
            }
            // the bar (chip) moves the transition, its edges change the length: like DJ.Studio
            let locked = self.mix.transitions[k].locked;
            let imitated = self.mix.transitions[k].uses_stems() && !self.stems_ok(sh, k);
            let bar_r = Rect::from_min_max(pos2(xs + 6.0, tstrip.top()), pos2((xe - 6.0).max(xs + 7.0), tstrip.bottom()));
            let chip_r = ui.interact(bar_r, Id::new(("tchip", k)), Sense::click_and_drag());
            if chip_r.hovered() || chip_r.dragged() {
                ui.ctx().set_cursor_icon(if locked { CursorIcon::NotAllowed } else if chip_r.dragged() { CursorIcon::Grabbing } else { CursorIcon::Grab });
            }
            if chip_r.clicked() {
                self.selection = Selection::Transition(k);
            }
            if chip_r.drag_started() {
                self.selection = Selection::Transition(k);
                if !locked {
                    self.drag = Some(Drag::TransitionMove(k));
                    self.drag_acc = 0.0;
                }
            }
            if chip_r.dragged() && self.drag == Some(Drag::TransitionMove(k)) {
                self.drag_acc += chip_r.drag_delta().x;
                // a beat (a quarter bar) per step; Shift: quarter beats
                let fine = ui.input(|i| i.modifiers.shift);
                if let Some(n) = self.take_beats(k, if fine { 0.25 } else { 1.0 }) {
                    self.move_transition(k, n);
                }
            }
            if chip_r.drag_stopped() {
                self.drag = None;
            }
            // lock / unlock on the bar (shown when locked, or when the bar is hovered or selected)
            let lock_r = Rect::from_center_size(pos2(bar_r.right() - 10.0, tstrip.center().y), vec2(16.0, 16.0));
            let show_lock = locked || sel || chip_r.hovered() || ui.rect_contains_pointer(lock_r);
            if show_lock && bar_r.width() > 40.0 {
                let lr = ui.interact(lock_r, Id::new(("tlock", k)), Sense::click());
                if lr.hovered() {
                    p.rect_filled(lock_r, 4.0, Color32::from_black_alpha(90));
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                widgets::lock_icon(&p, lock_r.shrink(3.0), locked, if sel { Color32::BLACK } else { Color32::WHITE });
                if lr.on_hover_text(if locked { "Locked: click to unlock" } else { "Lock this transition (harmonize and automatic mix points leave it alone, and it can't be dragged)" }).clicked() {
                    self.mix.transitions[k].locked = !locked;
                    self.selection = Selection::Transition(k);
                    self.changed();
                }
            }
            chip_r.on_hover_text(if locked {
                "Locked: unlock it in the transition editor to move it"
            } else if imitated {
                "Drag to move by beats (Shift: quarter beats) · drag an edge to change the length · click to edit. One of the tracks has no stems yet: its stem curves are imitated with volume and EQ."
            } else {
                "Drag to move by beats (Shift: quarter beats) · drag an edge to change the length · click to edit"
            });
            // edges
            for (end, xx) in [(false, xs), (true, xe)] {
                let hr = Rect::from_center_size(pos2(xx, tstrip.center().y), vec2(10.0, strip_h - 2.0));
                p.rect_filled(hr, 3.0, col);
                p.line_segment([pos2(xx, hr.top() + 5.0), pos2(xx, hr.bottom() - 5.0)], Stroke::new(1.5, Color32::WHITE));
                let r = ui.interact(hr.expand(3.0), Id::new(("thandle", k, end)), Sense::click_and_drag());
                if r.hovered() || r.dragged() {
                    ui.ctx().set_cursor_icon(if locked { CursorIcon::NotAllowed } else { CursorIcon::ResizeHorizontal });
                }
                let which = if end { Drag::TransitionEnd(k) } else { Drag::TransitionStart(k) };
                if r.drag_started() {
                    self.selection = Selection::Transition(k);
                    if !locked {
                        self.drag = Some(which);
                        self.drag_acc = 0.0;
                    }
                }
                if r.clicked() {
                    self.selection = Selection::Transition(k);
                }
                if r.dragged() && self.drag == Some(which) {
                    self.drag_acc += r.drag_delta().x;
                    let fine = ui.input(|i| i.modifiers.shift);
                    if let Some(n) = self.take_beats(k, if fine { 1.0 } else { 4.0 }) {
                        if end {
                            self.resize_transition_end(k, n as i32);
                        } else {
                            self.resize_transition_start(k, n as i32);
                        }
                    }
                }
                if r.drag_stopped() {
                    self.drag = None;
                }
                r.on_hover_text(if end { "Drag: change the length at the end (Shift: by beats)" } else { "Drag: change the length at the start, the music stays in place (Shift: by beats)" });
            }
        }

        if let Selection::Transition(k) = self.selection {
            self.edit_transition_curves(ui, sh, k, &lanes, canvas, &x_of);
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

        // ----- sample lanes
        self.sample_lanes_ui(ui, &p, &slanes, canvas, &x_of);

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

    /// The selected transition's curves, drawn and edited over both clips (like DJ.Studio): one
    /// row per stem, or one curve for the volume / an EQ band / the filter, chosen with the small
    /// selector over the frame.
    fn edit_transition_curves(&mut self, ui: &mut Ui, sh: &Shared, k: usize, lanes: &[Rect], canvas: Rect, x_of: &dyn Fn(f64) -> f32) {
        let Some(x) = self.timeline.transitions.get(k).cloned() else { return };
        if k + 1 >= self.timeline.tracks.len() {
            return;
        }
        let (xs, xe) = (x_of(x.start_sec), x_of(x.end_sec));
        if xe < canvas.left() || xs > canvas.right() || xe - xs < 8.0 {
            return;
        }
        let beats = self.mix.transitions[k].beats as f32;
        let editable = !self.mix.transitions[k].locked;
        let (zoom, origin) = (self.zoom, x_of(0.0));
        let tl = self.timeline.clone();
        let to_x = |b: f32| x_of(tl.sec_at_beat(x.start_beat + b as f64));
        let from_x = |px: f32| (tl.beat_at_sec((((px - origin) / zoom) as f64).max(0.0)) - x.start_beat) as f32;
        let beat_px = (tl.tracks[k].grid.beat_len() as f32 * zoom).max(0.1);
        let snap = move |b: f32, free: bool| {
            if free {
                return b;
            }
            let q = if beat_px > 48.0 { 0.25 } else if beat_px > 12.0 { 1.0 } else { 4.0 };
            (b / q).round() * q
        };
        let fmt_x = |b: f32| {
            let bar = (b / 4.0).floor() as i32 + 1;
            let beat = b.rem_euclid(4.0);
            if (beat - beat.round()).abs() < 1e-3 {
                format!("bar {bar}.{}", beat.round() as i32 + 1)
            } else {
                format!("bar {bar}.{:.2}", beat + 1.0)
            }
        };
        let pct = |v: f32| format!("{:.0} %", v * 100.0);
        let eq_fmt = |v: f32| {
            if v < 0.02 {
                "kill".to_string()
            } else {
                let g = if v <= 0.5 { (2.0 * v).powi(2) } else { 10f32.powf(12.0 * (v - 0.5) / 20.0) };
                format!("{:+.1} dB", 20.0 * g.log10())
            }
        };
        let filter_fmt = |v: f32| {
            if (v - 0.5).abs() < 0.01 {
                "off".to_string()
            } else if v < 0.5 {
                format!("low-pass {:.0} %", (0.5 - v) * 200.0)
            } else {
                format!("high-pass {:.0} %", (v - 0.5) * 200.0)
            }
        };
        let target = self.curve_target;
        let mut changed = false;
        for (side, ti) in [(Side::Out, k), (Side::In, k + 1)] {
            let l = ti % MIX_LANES;
            if self.lane_collapsed[l] {
                continue;
            }
            let (t_start, t_end, tid) = {
                let tt = &self.timeline.tracks[ti];
                (tt.start_sec, tt.end_sec, tt.track_id)
            };
            let r = Rect::from_min_max(pos2(x_of(t_start), lanes[l].top() + 2.0), pos2(x_of(t_end), lanes[l].bottom() - 2.0));
            let wave = clip_wave_rect(r);
            let area = Rect::from_x_y_ranges(xs.max(r.left())..=xe.min(r.right()), wave.y_range());
            if area.width() < 4.0 || area.height() < 16.0 {
                continue;
            }
            let painter = ui.painter_at(canvas);
            match target.param() {
                None => {
                    let has_stems = sh.waveforms.get(&tid).is_some_and(|w| w.stems.is_some());
                    let rh = area.height() / STEM_ROWS.len() as f32;
                    for (row, stem) in STEM_ROWS.into_iter().enumerate() {
                        let rr = Rect::from_min_size(pos2(area.left(), area.top() + row as f32 * rh), vec2(area.width(), rh - 1.0));
                        let col = theme::stem_color(stem);
                        if !has_stems {
                            painter.rect_filled(rr, 0.0, theme::blend(theme::PANEL, col, 0.14));
                        }
                        let plot = rr.shrink2(vec2(0.0, 3.0));
                        let mut pts = self.mix.transitions[k].lanes.iter().find(|ln| ln.side == side && ln.param == Param::Stem(stem)).map(|ln| ln.points.clone()).unwrap_or_default();
                        let off = !pts.is_empty() && pts.iter().all(|p| p.1 < 1e-3);
                        let c = crate::curve_edit::Curve {
                            rect: plot,
                            x0: 0.0,
                            x1: beats,
                            to_x: &to_x,
                            from_x: &from_x,
                            snap: &snap,
                            neutral: 1.0,
                            centered: false,
                            color: if off { col.gamma_multiply(0.4) } else { Color32::WHITE },
                            editable: editable && !off,
                            fmt: &pct,
                            fmt_x: &fmt_x,
                        };
                        if crate::curve_edit::curve_ui(ui, Id::new(("tcurve", k, side == Side::In, row)), &c, &mut pts) {
                            set_lane(&mut self.mix.transitions[k], side, Param::Stem(stem), pts);
                            changed = true;
                        }
                        // on / off per stem and side (DJ.Studio's stem toggles)
                        let b = Rect::from_min_size(pos2(area.left() + 4.0, rr.center().y - 8.0), vec2(20.0, 16.0));
                        if b.right() < area.right() && rr.height() >= 12.0 {
                            let br = ui.interact(b, Id::new(("ttoggle", k, side == Side::In, row)), Sense::click());
                            painter.rect_filled(b, 4.0, if off { theme::PANEL } else { col });
                            painter.rect_stroke(b, 4.0, Stroke::new(1.0, col), StrokeKind::Inside);
                            widgets::stem_icon(&painter, b.shrink2(vec2(3.0, 2.0)), stem, if off { col } else { Color32::BLACK });
                            if br.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }
                            let tip = format!(
                                "{} of the {} track: {} in this transition (click to switch it {})",
                                stem.label(),
                                if side == Side::Out { "outgoing" } else { "incoming" },
                                if off { "off" } else { "on" },
                                if off { "on" } else { "off" }
                            );
                            if br.on_hover_text(tip).clicked() && editable {
                                let key = (k, side == Side::In, row);
                                let cur = self.mix.transitions[k].lanes.iter().find(|ln| ln.side == side && ln.param == Param::Stem(stem)).map(|ln| ln.points.clone()).unwrap_or_default();
                                let next = if off {
                                    self.stem_stash.remove(&key).unwrap_or_else(|| vec![(0.0, 1.0), (beats, 1.0)])
                                } else {
                                    self.stem_stash.insert(key, cur);
                                    vec![(0.0, 0.0), (beats, 0.0)]
                                };
                                set_lane(&mut self.mix.transitions[k], side, Param::Stem(stem), next);
                                changed = true;
                            }
                        }
                    }
                    if !has_stems && area.width() > 200.0 {
                        painter.text(pos2(area.right() - 6.0, area.top() + 3.0), egui::Align2::RIGHT_TOP, "no stems yet: imitated with volume and EQ", egui::FontId::proportional(10.0), theme::TEXT_DIM);
                    }
                }
                Some(param) => {
                    let centered = matches!(param, Param::EqLow | Param::EqMid | Param::EqHigh | Param::Filter);
                    let mut pts = self.mix.transitions[k].lanes.iter().find(|ln| ln.side == side && ln.param == param).map(|ln| ln.points.clone()).unwrap_or_default();
                    let fmt: &dyn Fn(f32) -> String = match param {
                        Param::Filter => &filter_fmt,
                        Param::EqLow | Param::EqMid | Param::EqHigh => &eq_fmt,
                        _ => &pct,
                    };
                    let c = crate::curve_edit::Curve {
                        rect: area.shrink2(vec2(0.0, 4.0)),
                        x0: 0.0,
                        x1: beats,
                        to_x: &to_x,
                        from_x: &from_x,
                        snap: &snap,
                        neutral: param.neutral(),
                        centered,
                        color: target.color(),
                        editable,
                        fmt,
                        fmt_x: &fmt_x,
                    };
                    if crate::curve_edit::curve_ui(ui, Id::new(("tcurve", k, side == Side::In, 9)), &c, &mut pts) {
                        set_lane(&mut self.mix.transitions[k], side, param, pts);
                        changed = true;
                    }
                }
            }
        }
        // the selector, centred over the frame just below the transition bar
        let seg = 50.0;
        let w = seg * CurveTarget::ALL.len() as f32;
        if canvas.width() > w + 16.0 {
            let top = lanes[0].top() + 1.0;
            let bar = Rect::from_center_size(pos2(((xs + xe) / 2.0).clamp(canvas.left() + w / 2.0, canvas.right() - w / 2.0), top + 10.0), vec2(w, 19.0));
            let painter = ui.painter_at(canvas);
            painter.rect_filled(bar, 6.0, Color32::from_black_alpha(215));
            for (j, t) in CurveTarget::ALL.into_iter().enumerate() {
                let sr = Rect::from_min_size(pos2(bar.left() + j as f32 * seg, bar.top()), vec2(seg, bar.height())).shrink(1.5);
                let resp = ui.interact(sr, Id::new(("ctarget", j)), Sense::click());
                let on = self.curve_target == t;
                if on {
                    painter.rect_filled(sr, 5.0, theme::SEG_ON);
                } else if resp.hovered() {
                    painter.rect_filled(sr, 5.0, theme::SEG_HOVER);
                }
                painter.text(sr.center(), egui::Align2::CENTER_CENTER, t.label(), egui::FontId::proportional(11.0), if on { t.color() } else { theme::TEXT_DIM });
                if resp.on_hover_text(format!("Draw the {} curves in the timeline", t.label().to_lowercase())).clicked() {
                    self.curve_target = t;
                }
            }
        }
        if changed {
            self.mix.transitions[k].style = clubcraft_core::TransitionStyle::Custom;
            self.changed();
        }
    }

    /// Convert accumulated drag pixels into steps of `per` beats at the tempo of track `i`;
    /// returns the number of beats.
    fn take_beats(&mut self, i: usize, per: f32) -> Option<f32> {
        let tt = self.timeline.tracks.get(i)?;
        let px = (tt.grid.beat_len() as f32 * per * self.zoom).max(1.0);
        let steps = (self.drag_acc / px).trunc();
        if steps != 0.0 {
            self.drag_acc -= steps * px;
            Some(steps * per)
        } else {
            None
        }
    }

    /// Move transition `k` by `beats` (the outgoing track's mix-out; the incoming track follows).
    pub(crate) fn move_transition(&mut self, k: usize, beats: f32) {
        let (bl, dur) = (self.timeline.tracks[k].grid.beat_len(), self.timeline.tracks[k].duration);
        let mt = &mut self.mix.tracks[k];
        let nv = mt.mix_out + beats as f64 * bl;
        if nv > mt.mix_in + bl * 8.0 && (dur <= 0.0 || nv < dur - bl * 4.0) {
            mt.mix_out = nv;
            self.changed();
        }
    }

    /// Lengthen (or shorten) transition `k` at its end by `beats`.
    pub(crate) fn resize_transition_end(&mut self, k: usize, beats: i32) {
        let t = &mut self.mix.transitions[k];
        let nb = (t.beats as i32 + beats).clamp(1, 512) as u32;
        if nb != t.beats {
            t.set_beats(nb);
            self.changed();
        }
    }

    /// Lengthen (or shorten) transition `k` at its start by `beats`: both tracks keep their place
    /// in the mix, only the window grows to the left.
    pub(crate) fn resize_transition_start(&mut self, k: usize, beats: i32) {
        let (bl_out, bl_in) = (self.timeline.tracks[k].grid.beat_len(), self.timeline.tracks[k + 1].grid.beat_len());
        let nb = self.mix.transitions[k].beats as i32 - beats;
        let (out_in, out_out, in_in) = (self.mix.tracks[k].mix_in, self.mix.tracks[k].mix_out, self.mix.tracks[k + 1].mix_in);
        let (new_out, new_in) = (out_out + beats as f64 * bl_out, in_in + beats as f64 * bl_in);
        if !(1..=512).contains(&nb) || new_in < 0.0 || new_out < out_in + bl_out * 8.0 {
            return;
        }
        self.mix.tracks[k].mix_out = new_out;
        self.mix.tracks[k + 1].mix_in = new_in;
        // the curves keep their place in the music: shift them, then fit the new length
        let t = &mut self.mix.transitions[k];
        for l in &mut t.lanes {
            for p in &mut l.points {
                p.0 -= beats as f32;
            }
            l.points.retain(|p| p.0 >= -1e-3);
            if l.points.first().is_none_or(|p| p.0 > 1e-3) {
                let v = l.value_at(0.0);
                l.points.insert(0, (0.0, v));
            }
        }
        t.beats = nb as u32;
        self.changed();
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
            let mut regions: Vec<(f64, f64, Transition, Side, usize)> = Vec::new();
            if i > 0 {
                if let (Some(x), Some(tr)) = (self.timeline.transitions.get(i - 1), self.effective_transition(sh, i - 1)) {
                    regions.push((x.start_beat, x.start_beat + x.beats, tr, Side::In, i - 1));
                }
            }
            if let (Some(x), Some(tr)) = (self.timeline.transitions.get(i), self.effective_transition(sh, i)) {
                regions.push((x.start_beat, x.start_beat + x.beats, tr, Side::Out, i));
            }
            // the track's own stem lanes (outside transitions, and inside where the transition
            // has no lane for that stem)
            let own = self.mix.tracks.get(i).filter(|m| !m.stems.is_empty());
            let own_gain = |b: f64, k: StemKind| own.and_then(|m| m.stem_at(k, self.timeline.track_pos(i, b))).unwrap_or(1.0).clamp(0.0, 1.0);
            let paint_own = |b0: f64, b1: f64| {
                if own.is_none() {
                    return paint(b0, b1, [1.0; 4], 1.0);
                }
                let (x0, x1) = (x_of(self.timeline.sec_at_beat(b0)), x_of(self.timeline.sec_at_beat(b1)));
                let (x0, x1) = (x0.max(clip.left() - 4.0), x1.min(clip.right() + 4.0));
                if x1 <= x0 {
                    return;
                }
                let (c0, c1) = (self.timeline.beat_at_sec(((x0 - x_of(0.0)) / self.zoom) as f64).max(b0), self.timeline.beat_at_sec(((x1 - x_of(0.0)) / self.zoom) as f64).min(b1));
                let n = ((x1 - x0) / 4.0).clamp(1.0, 600.0) as usize;
                for s in 0..n {
                    let (s0, s1) = (c0 + (c1 - c0) * s as f64 / n as f64, c0 + (c1 - c0) * (s + 1) as f64 / n as f64);
                    paint(s0, s1, StemKind::ALL.map(|k| own_gain((s0 + s1) * 0.5, k)), 1.0);
                }
            };
            let mut b = tt.start_beat;
            for (start, end, tr, side, k) in &regions {
                let (start, r0, r1) = (*start, start.max(tt.start_beat), end.min(tt.end_beat));
                if r0 > b {
                    paint_own(b, r0);
                }
                // the selected transition shows one row per stem (like DJ.Studio)
                if self.selection == Selection::Transition(*k) && self.curve_target == CurveTarget::Stems && wf.stems.is_some() && audible {
                    let (x0, x1) = (x_of(self.timeline.sec_at_beat(r0)), x_of(self.timeline.sec_at_beat(r1)));
                    self.paint_stem_rows(p, Rect::from_x_y_ranges(x0..=x1, wrect.y_range()), wf, i, tr, *side, start, x_of);
                    b = b.max(r1);
                    continue;
                }
                let (x0, x1) = (x_of(self.timeline.sec_at_beat(r0)), x_of(self.timeline.sec_at_beat(r1)));
                let n = ((x1 - x0) / 4.0).clamp(4.0, 400.0) as usize;
                for s in 0..n {
                    let (s0, s1) = (r0 + (r1 - r0) * s as f64 / n as f64, r0 + (r1 - r0) * (s + 1) as f64 / n as f64);
                    let xb = ((s0 + s1) * 0.5 - start) as f32;
                    let orig = &self.mix.transitions[*k];
                    let has = |t: &Transition, st: StemKind| t.lanes.iter().any(|l| l.side == *side && l.param == Param::Stem(st));
                    let gains = StemKind::ALL.map(|st| {
                        if has(tr, st) {
                            tr.value(*side, Param::Stem(st), xb).clamp(0.0, 1.0)
                        } else if has(orig, st) {
                            1.0 // emulated by the volume and EQ
                        } else {
                            own_gain((s0 + s1) * 0.5, st)
                        }
                    });
                    paint(s0, s1, gains, tr.value(*side, Param::Volume, xb).clamp(0.0, 1.0));
                }
                b = b.max(r1);
            }
            if b < tt.end_beat {
                paint_own(b, tt.end_beat);
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
        // the track's own stem lanes: a thin row per muted stem at the bottom of the clip
        let own = &self.mix.tracks[i].stems;
        if !own.is_empty() {
            let clip = p.clip_rect().intersect(body);
            let (x0, x1) = (clip.left().floor(), clip.right());
            let origin = x_of(0.0);
            let rows: Vec<StemKind> = STEM_ROWS.into_iter().filter(|st| own.iter().any(|l| l.stem == *st)).collect();
            for (n, st) in rows.iter().enumerate() {
                let y = body.bottom() - 3.0 - (rows.len() - n) as f32 * 4.0;
                let col = theme::stem_color(*st);
                let mut x = x0;
                while x < x1 {
                    let b = self.timeline.beat_at_sec(((x + 1.0 - origin) / self.zoom) as f64);
                    let g = self.mix.tracks[i].stem_at(*st, self.timeline.track_pos(i, b)).unwrap_or(1.0);
                    if g < 0.99 {
                        p.rect_filled(Rect::from_min_max(pos2(x, y), pos2(x + 2.0, y + 3.0)), 0.0, col.gamma_multiply(1.0 - g * 0.8));
                    }
                    x += 2.0;
                }
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
        // the selected transition's curves are drawn (and edited) by `edit_transition_curves`
        let show_for = |k: usize| -> Option<ShowMode> { (self.selection != Selection::Transition(k)).then_some(show) };
        if show != ShowMode::None {
            if i > 0 {
                if let (Some(x), Some(tr), Some(sm)) = (self.timeline.transitions.get(i - 1), self.effective_transition(sh, i - 1), show_for(i - 1)) {
                    self.draw_automation(p, body, &tr, Side::In, x.start_beat, x.beats, sm, x_of);
                }
            }
            if let (Some(x), Some(tr), Some(sm)) = (self.timeline.transitions.get(i), self.effective_transition(sh, i), show_for(i)) {
                self.draw_automation(p, body, &tr, Side::Out, x.start_beat, x.beats, sm, x_of);
            }
        }
        if !audible {
            p.rect_filled(r, 4.0, Color32::from_black_alpha(110));
        }
        p.rect_stroke(r, 4.0, Stroke::new(if sel { 2.0 } else { 1.0 }, if sel { theme::accent() } else { accent.linear_multiply(0.5) }), StrokeKind::Inside);
    }

    /// Stem rows (drums, bass, melody, vocals) of track `i` inside transition `tr`: each row shows
    /// that stem's waveform at the level it plays.
    #[allow(clippy::too_many_arguments)]
    fn paint_stem_rows(&self, p: &egui::Painter, rect: Rect, wf: &clubcraft_core::Waveform, i: usize, tr: &Transition, side: Side, start: f64, x_of: &dyn Fn(f64) -> f32) {
        let Some(stems) = &wf.stems else { return };
        let clip = p.clip_rect().intersect(rect);
        if clip.width() <= 0.0 {
            return;
        }
        let origin = x_of(0.0);
        let beat_at_x = |x: f32| self.timeline.beat_at_sec(((x - origin) / self.zoom) as f64);
        let order = [StemKind::Drums, StemKind::Bass, StemKind::Other, StemKind::Vocals];
        let rh = rect.height() / 4.0;
        let mut mesh = egui::Mesh::default();
        for (row, k) in order.into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(rect.left(), rect.top() + row as f32 * rh), vec2(rect.width(), rh - 1.0));
            let col = theme::stem_color(k);
            p.rect_filled(r.intersect(clip), 0.0, theme::blend(theme::PANEL, col, 0.2));
            let data = &stems[k.index()];
            let mut x = clip.left().floor();
            while x < clip.right() {
                let b = beat_at_x(x + 0.5);
                let g = (tr.value(side, Param::Stem(k), (b - start) as f32) * tr.value(side, Param::Volume, (b - start) as f32)).clamp(0.0, 1.0);
                let pos = self.timeline.track_pos(i, b);
                let bin = ((pos * wf.rate) as usize).min(data.len().saturating_sub(1));
                let v = data.get(bin).map(|&v| v as f32 / 255.0).unwrap_or(0.0) * g;
                let hh = v * (rh * 0.5 - 2.0);
                mesh.add_colored_rect(Rect::from_min_max(pos2(x, r.center().y - hh), pos2(x + 1.0, r.center().y + hh)), col);
                x += 1.0;
            }
        }
        // (each row's level line is drawn, and edited, by `edit_transition_curves`)
        p.add(egui::Shape::mesh(mesh));
    }

    /// Header of sample lane `l`: name, fader, solo / mute, add a file.
    fn sample_lane_header(&mut self, ui: &mut Ui, r: Rect, l: usize) -> bool {
        let accent = SAMPLE_COLOR;
        ui.painter().rect_filled(r, 4.0, theme::PANEL);
        ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(3.0, r.height())), 2.0, accent);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(vec2(10.0, 4.0))).layout(Layout::top_down(Align::Min)));
        let ui = &mut child;
        self.mix.normalize();
        let mut ls = self.mix.sample_lanes[l];
        let before = ls;
        ui.horizontal(|ui| {
            let chevron = if self.sample_open[l] { "⏷" } else { "⏵" };
            if ui.add(egui::Label::new(RichText::new(format!("{chevron} Sample {}", l + 1)).strong().color(accent)).sense(Sense::click())).on_hover_text("Collapse / expand the lane").clicked() {
                self.sample_open[l] = !self.sample_open[l];
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::toggle(ui, ls.mute, "M", Color32::from_rgb(255, 90, 90), vec2(22.0, 18.0)).on_hover_text("Mute lane").clicked() {
                    ls.mute = !ls.mute;
                }
                if widgets::toggle(ui, ls.solo, "S", theme::WARN, vec2(22.0, 18.0)).on_hover_text("Solo lane").clicked() {
                    ls.solo = !ls.solo;
                }
                if ui.small_button("+").on_hover_text("Add an audio file at the playhead (or drop files from Finder onto the lane)").clicked() {
                    if let Some(files) = rfd::FileDialog::new().set_title("Add samples").add_filter("Audio", crate::importer::AUDIO_EXTS).pick_files() {
                        let beat = self.timeline.beat_at_sec(self.position).round();
                        for (n, f) in files.into_iter().enumerate() {
                            self.mix.samples.push(SampleClip { lane: l as u8, path: f, start_beat: beat + 4.0 * n as f64, gain_db: 0.0 });
                        }
                        self.changed();
                    }
                }
            });
        });
        if self.sample_open[l] {
            let mut v = db_fader(ls.gain_db);
            if widgets::hfader_with_default(ui, &mut v, false, 0.8, Some(0.8), vec2(150.0, 16.0), accent).on_hover_text("Lane volume · double-click: 0 dB").changed() {
                ls.gain_db = fader_db(v);
            }
        }
        if ls != before {
            self.mix.sample_lanes[l] = ls;
            return true;
        }
        false
    }

    /// Duration (s) and a peak overview (100 per second) of a sample file, cached.
    fn sample_info(&mut self, path: &std::path::Path) -> Option<(f64, std::sync::Arc<Vec<f32>>)> {
        if let Some(i) = self.sample_cache.get(path) {
            return i.clone();
        }
        let info = clubcraft_engine::samples::load(path).map(|a| {
            let step = (a.sample_rate as usize / 100).max(1);
            let peaks: Vec<f32> = a.left.chunks(step).zip(a.right.chunks(step)).map(|(l, r)| l.iter().chain(r).fold(0.0f32, |m, v| m.max(v.abs()))).collect();
            (a.frames() as f64 / a.sample_rate as f64, std::sync::Arc::new(peaks))
        });
        self.sample_cache.insert(path.to_path_buf(), info.clone());
        info
    }

    /// Sample clips: draw, drag (snaps to beats), context menu; files dropped from Finder land on
    /// the lane under the pointer.
    fn sample_lanes_ui(&mut self, ui: &mut Ui, p: &egui::Painter, slanes: &[Rect], canvas: Rect, x_of: &dyn Fn(f64) -> f32) {
        let origin = x_of(0.0);
        let zoom = self.zoom;
        let beat_at_x = move |tl: &clubcraft_core::MixTimeline, x: f32| tl.beat_at_sec((((x - origin) / zoom) as f64).max(0.0));
        let hovering_files = ui.input(|i| !i.raw.hovered_files.is_empty());
        let pointer = ui.input(|i| i.pointer.latest_pos());
        for (l, lane) in slanes.iter().enumerate() {
            p.rect_filled(*lane, 4.0, Color32::from_rgb(22, 24, 29));
            if hovering_files && pointer.is_some_and(|q| lane.contains(q)) {
                p.rect_stroke(*lane, 4.0, Stroke::new(2.0, SAMPLE_COLOR), StrokeKind::Inside);
            }
            if self.mix.samples.iter().all(|c| c.lane as usize != l) && self.sample_open[l] {
                p.text(pos2(canvas.left() + 12.0, lane.center().y), egui::Align2::LEFT_CENTER, "Drop audio files here, or + on the lane header", egui::FontId::proportional(11.0), theme::TEXT_DIM);
            }
        }
        // drops from Finder
        let dropped: Vec<std::path::PathBuf> = ui.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| crate::importer::is_audio(p)).collect());
        if let (false, Some(q)) = (dropped.is_empty(), pointer) {
            if let Some(l) = slanes.iter().position(|r| r.contains(q)) {
                let beat = beat_at_x(&self.timeline, q.x).round();
                for (n, f) in dropped.into_iter().enumerate() {
                    self.mix.samples.push(SampleClip { lane: l as u8, path: f, start_beat: beat + 4.0 * n as f64, gain_db: 0.0 });
                }
                self.consumed_drop = true;
                self.changed();
            }
        }
        let mut remove: Option<usize> = None;
        let mut dup: Option<usize> = None;
        let mut edited = false;
        for j in 0..self.mix.samples.len() {
            let c = self.mix.samples[j].clone();
            let Some(lane) = slanes.get(c.lane as usize).copied() else { continue };
            let (dur, peaks) = self.sample_info(&c.path).unwrap_or((2.0, Default::default()));
            let t0 = self.timeline.sec_at_beat(c.start_beat);
            let r = Rect::from_min_max(pos2(x_of(t0), lane.top() + 2.0), pos2(x_of(t0 + dur).max(x_of(t0) + 6.0), lane.bottom() - 2.0));
            if r.right() < canvas.left() || r.left() > canvas.right() {
                continue;
            }
            let pc = p.with_clip_rect(r.intersect(canvas));
            pc.rect_filled(r, 4.0, theme::blend(theme::PANEL, SAMPLE_COLOR, 0.35));
            let name = c.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            pc.text(r.left_top() + vec2(5.0, 2.0), egui::Align2::LEFT_TOP, &name, egui::FontId::proportional(11.0), Color32::WHITE);
            if r.height() > 24.0 && !peaks.is_empty() {
                let cy = r.center().y + 6.0;
                let hmax = (r.height() - 18.0) * 0.5;
                let mut mesh = egui::Mesh::default();
                let x0 = r.left().max(canvas.left());
                let mut x = x0;
                while x < r.right().min(canvas.right()) {
                    let t = (x - r.left()) / self.zoom;
                    let v = peaks.get((t * 100.0) as usize).copied().unwrap_or(0.0).min(1.0);
                    mesh.add_colored_rect(Rect::from_min_max(pos2(x, cy - v * hmax), pos2(x + 1.0, cy + v * hmax)), SAMPLE_COLOR);
                    x += 1.0;
                }
                pc.add(egui::Shape::mesh(mesh));
            }
            pc.rect_stroke(r, 4.0, Stroke::new(1.0, SAMPLE_COLOR), StrokeKind::Inside);
            let resp = ui.interact(r.intersect(canvas), Id::new(("sample-clip", j)), Sense::click_and_drag());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(if resp.dragged() { CursorIcon::Grabbing } else { CursorIcon::Grab });
            }
            if resp.dragged() {
                if let Some(q) = ui.input(|i| i.pointer.latest_pos()) {
                    let nb = beat_at_x(&self.timeline, x_of(t0) + resp.drag_delta().x).max(0.0);
                    let snapped = if ui.input(|i| i.modifiers.shift) { nb } else { nb.round() };
                    if (snapped - c.start_beat).abs() > 1e-6 {
                        self.mix.samples[j].start_beat = snapped;
                        edited = true;
                    }
                    // move between lanes
                    if let Some(nl) = slanes.iter().position(|lr| lr.contains(q)) {
                        if nl != c.lane as usize {
                            self.mix.samples[j].lane = nl as u8;
                            edited = true;
                        }
                    }
                }
            }
            resp.context_menu(|ui| {
                ui.label(RichText::new(&name).strong());
                let mut g = self.mix.samples[j].gain_db;
                if ui.add(egui::Slider::new(&mut g, -24.0..=6.0).suffix(" dB").text("Gain")).changed() {
                    self.mix.samples[j].gain_db = g;
                    edited = true;
                }
                if ui.button("Duplicate (1 bar later)").clicked() {
                    dup = Some(j);
                    ui.close();
                }
                if ui.button("Delete").clicked() {
                    remove = Some(j);
                    ui.close();
                }
            })
            .map(|_| ());
            resp.on_hover_text(format!("{name} · {:.1} s · drag to move (snaps to beats, Shift: free) · right-click for more", dur));
        }
        if let Some(j) = dup {
            let mut c = self.mix.samples[j].clone();
            c.start_beat += 4.0;
            self.mix.samples.push(c);
            edited = true;
        }
        if let Some(j) = remove {
            self.mix.samples.remove(j);
            edited = true;
        }
        if edited {
            self.changed();
        }
    }

    /// Both tracks of transition `k` have stems.
    pub(crate) fn stems_ok(&self, sh: &Shared, k: usize) -> bool {
        let has = |i: usize| self.mix.tracks.get(i).is_some_and(|t| sh.analyses.get(&t.track_id).is_some_and(|a| a.stems_model.is_some()));
        has(k) && has(k + 1)
    }

    /// Transition `k` as it plays: on a track without stems, the stem lanes are emulated with its
    /// volume and EQ.
    pub(crate) fn effective_transition(&self, sh: &Shared, k: usize) -> Option<Transition> {
        let tr = self.mix.transitions.get(k)?;
        let has = |i: usize| self.mix.tracks.get(i).is_some_and(|t| sh.analyses.get(&t.track_id).is_some_and(|a| a.stems_model.is_some()));
        Some(if tr.uses_stems() && !self.stems_ok(sh, k) { tr.without_stems_on(!has(k), !has(k + 1)) } else { tr.clone() })
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

/// Replace (or add) the lane of `param` on `side`.
fn set_lane(t: &mut Transition, side: Side, param: Param, points: Vec<(f32, f32)>) {
    match t.lanes.iter_mut().find(|l| l.side == side && l.param == param) {
        Some(l) => l.points = points,
        None => t.lanes.push(clubcraft_core::Lane { side, param, points }),
    }
}
