//! Transition editor: both tracks aligned on a bar ruler, with editable automation per
//! parameter group, and style presets explained in words.

use crate::app::Shared;
use crate::studio::StudioView;
use crate::studio_timeline::{EQ_COLORS, FILTER_COLOR, VOLUME_COLOR};
use crate::theme;
use crate::waveform::{self, WaveParams, WaveStyle};
use clubcraft_core::{Param, Side, StemKind, Transition, TransitionStyle};
use eframe::egui::{self, pos2, vec2, Color32, Id, Rect, RichText, Sense, Stroke, StrokeKind, Ui};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ParamGroup {
    #[default]
    Stems,
    Volume,
    Eq,
    FilterFx,
}

impl ParamGroup {
    const ALL: [ParamGroup; 4] = [ParamGroup::Stems, ParamGroup::Volume, ParamGroup::Eq, ParamGroup::FilterFx];
    fn label(self) -> &'static str {
        match self {
            ParamGroup::Stems => "Stems",
            ParamGroup::Volume => "Volume",
            ParamGroup::Eq => "EQ",
            ParamGroup::FilterFx => "Filter & Echo",
        }
    }
    fn params(self) -> Vec<(Param, &'static str, Color32)> {
        match self {
            ParamGroup::Stems => [StemKind::Vocals, StemKind::Other, StemKind::Bass, StemKind::Drums].iter().map(|k| (Param::Stem(*k), k.label(), theme::rgb(k.color()))).collect(),
            ParamGroup::Volume => vec![(Param::Volume, "Volume", VOLUME_COLOR)],
            ParamGroup::Eq => vec![(Param::EqHigh, "High", EQ_COLORS[2]), (Param::EqMid, "Mid", EQ_COLORS[1]), (Param::EqLow, "Low", EQ_COLORS[0])],
            ParamGroup::FilterFx => vec![(Param::Filter, "Filter", FILTER_COLOR), (Param::Echo, "Echo", Color32::from_rgb(255, 160, 90))],
        }
    }
}

fn style_help(s: TransitionStyle) -> &'static str {
    match s {
        TransitionStyle::StemBlend => "Incoming drums first, outgoing vocals leave early, bass swapped on the half, melody crossfades.",
        TransitionStyle::BassSwap => "Classic DJ blend: bring the new track in without bass, swap the low end in the middle.",
        TransitionStyle::Crossfade => "Smooth volume crossfade of both tracks.",
        TransitionStyle::VocalOver => "Outgoing vocals keep singing over the new track's instrumental (mashup).",
        TransitionStyle::FilterOut => "High-pass sweep out of the old track while the new one comes in.",
        TransitionStyle::EchoOut => "Old track echoes out, new track drops in on the downbeat.",
        TransitionStyle::Cut => "Hard cut on the downbeat.",
        TransitionStyle::Custom => "Your own curves (drawn below).",
    }
}

impl StudioView {
    pub(crate) fn transition_editor(&mut self, ui: &mut Ui, sh: &Shared, k: usize) {
        let (ia, ib) = (self.mix.tracks[k].track_id, self.mix.tracks[k + 1].track_id);
        let name = |id| sh.track(id).map(|t| t.display_name()).unwrap_or_default();
        let key = |i: usize, id| sh.analyses.get(&id).and_then(|a| a.key).map(|kk| kk.shifted(self.mix.tracks[i].key_shift));
        let (ka, kb) = (key(k, ia), key(k + 1, ib));
        let mut changed = false;
        ui.horizontal_top(|ui| {
            // ---------------------------------------------------- left: summary + presets
            ui.vertical(|ui| {
                ui.set_width(300.0);
                ui.label(RichText::new(format!("Transition {} ▶ {}", k + 1, k + 2)).size(16.0).strong());
                for (i, id, kk, col) in [(k, ia, ka, theme::deck_color(k % 2)), (k + 1, ib, kb, theme::deck_color((k + 1) % 2))] {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(if i == k { "OUT" } else { "IN " }).monospace().size(11.0).color(col));
                        if let Some(kk) = kk {
                            ui.label(RichText::new(kk.camelot()).strong().color(theme::key_color(kk)));
                        }
                        ui.add(egui::Label::new(crate::studio::truncate(&name(id), 34)).truncate());
                    });
                }
                if let (Some(a), Some(b)) = (ka, kb) {
                    let c = a.compatibility(b);
                    let (txt, col) = if c >= 0.85 { ("harmonic match", theme::OK) } else if c >= 0.5 { ("acceptable key change", theme::WARN) } else { ("key clash: try a key shift on the track", Color32::from_rgb(255, 90, 90)) };
                    ui.label(RichText::new(txt).color(col).size(11.5));
                }
                ui.add_space(6.0);
                let tr = &mut self.mix.transitions[k];
                ui.label(RichText::new("Style").color(theme::TEXT_DIM));
                egui::Grid::new("styles").num_columns(2).spacing(vec2(4.0, 4.0)).show(ui, |ui| {
                    for (n, s) in TransitionStyle::ALL.iter().enumerate() {
                        let sel = tr.style == *s;
                        let b = egui::Button::new(RichText::new(s.label()).size(12.0)).selected(sel).min_size(vec2(146.0, 24.0));
                        if ui.add(b).on_hover_text(style_help(*s)).clicked() && !sel {
                            *tr = Transition::new(*s, tr.beats);
                            changed = true;
                        }
                        if n % 2 == 1 {
                            ui.end_row();
                        }
                    }
                });
                ui.label(RichText::new(style_help(tr.style)).size(11.0).color(theme::TEXT_DIM));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Length").color(theme::TEXT_DIM));
                    for bars in [2u32, 4, 8, 16, 32] {
                        if ui.selectable_label(tr.beats == bars * 4, format!("{bars}")).clicked() && tr.beats != bars * 4 {
                            let lanes = if tr.style == TransitionStyle::Custom { crate::studio::scale_lanes(&tr.lanes, tr.beats as f32, (bars * 4) as f32) } else { tr.style.lanes((bars * 4) as f32) };
                            tr.beats = bars * 4;
                            tr.lanes = lanes;
                            changed = true;
                        }
                    }
                    ui.label(RichText::new("bars").color(theme::TEXT_DIM));
                });
                if tr.style == TransitionStyle::Custom && ui.small_button("Reset to Stem blend").clicked() {
                    *tr = Transition::new(TransitionStyle::StemBlend, tr.beats);
                    changed = true;
                }
            });
            ui.separator();
            // ---------------------------------------------------- right: aligned editor
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    for g in ParamGroup::ALL {
                        if ui.add(egui::Button::new(RichText::new(g.label()).size(12.5)).selected(self.tgroup == g).min_size(vec2(90.0, 22.0))).clicked() {
                            self.tgroup = g;
                        }
                    }
                    ui.label(RichText::new("Drag to draw · right-click a point to delete · values snap to ½ beat").size(11.0).color(theme::TEXT_DIM));
                });
                changed |= self.aligned_editor(ui, sh, k);
            });
        });
        if changed {
            self.changed();
        }
    }

    /// Ruler + one row per side (waveform with sub-lanes per parameter). Returns true if edited.
    fn aligned_editor(&mut self, ui: &mut Ui, sh: &Shared, k: usize) -> bool {
        let Some(x) = self.timeline.transitions.get(k).cloned() else { return false };
        let beats = self.mix.transitions[k].beats as f32;
        let avail = ui.available_rect_before_wrap();
        let label_w = 96.0;
        let ruler_h = 18.0;
        let row_h = ((avail.height() - ruler_h - 8.0) / 2.0).clamp(60.0, 160.0);
        let rect = Rect::from_min_size(avail.min, vec2(avail.width(), ruler_h + row_h * 2.0 + 6.0));
        ui.allocate_rect(rect, Sense::hover());
        let plot_l = rect.left() + label_w;
        let plot_w = rect.width() - label_w - 4.0;
        let x_of_beat = |b: f32| plot_l + b / beats * plot_w;
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 4.0, theme::BG);
        // ruler (bars of the transition)
        let ruler = Rect::from_min_size(pos2(plot_l, rect.top()), vec2(plot_w, ruler_h));
        p.rect_filled(ruler, 0.0, theme::PANEL);
        for b in 0..=(beats as i32) {
            let xx = x_of_beat(b as f32);
            if b % 4 == 0 {
                p.line_segment([pos2(xx, ruler.top() + 8.0), pos2(xx, rect.bottom())], Stroke::new(1.0, Color32::from_white_alpha(28)));
                if b < beats as i32 {
                    p.text(pos2(xx + 3.0, ruler.top() + 1.0), egui::Align2::LEFT_TOP, format!("bar {}", b / 4 + 1), egui::FontId::proportional(10.0), Color32::from_rgb(220, 210, 120));
                }
            } else if plot_w / beats > 10.0 {
                p.line_segment([pos2(xx, ruler.bottom() - 3.0), pos2(xx, ruler.bottom())], Stroke::new(1.0, theme::LINE));
            }
        }
        let params = self.tgroup.params();
        let mut edited = false;
        for (row, side, ti) in [(0usize, Side::Out, k), (1, Side::In, k + 1)] {
            let rr = Rect::from_min_size(pos2(rect.left(), ruler.bottom() + 3.0 + row as f32 * (row_h + 3.0)), vec2(rect.width(), row_h));
            let accent = theme::deck_color(ti % 2);
            // label column
            let lab = Rect::from_min_size(rr.min, vec2(label_w - 4.0, rr.height()));
            p.rect_filled(lab, 3.0, theme::PANEL);
            p.rect_filled(Rect::from_min_size(lab.min, vec2(4.0, lab.height())), 2.0, accent);
            p.text(lab.left_top() + vec2(9.0, 4.0), egui::Align2::LEFT_TOP, if side == Side::Out { "OUT" } else { "IN" }, egui::FontId::proportional(12.5), accent);
            // waveform of that track over the window
            let plot = Rect::from_min_max(pos2(plot_l, rr.top()), pos2(plot_l + plot_w, rr.bottom()));
            p.rect_filled(plot, 3.0, Color32::from_rgb(24, 26, 31));
            let tid = self.timeline.tracks[ti].track_id;
            if let (Some(wf), Some(a)) = (sh.waveforms.get(&tid), sh.analyses.get(&tid)) {
                let t0 = self.timeline.track_pos(ti, x.start_beat);
                let t1 = self.timeline.track_pos(ti, x.start_beat + x.beats);
                let wp = WaveParams { wf, grid: a.grid, cues: &[], sections: &[], style: WaveStyle::Stems, stem_gains: [0.35; 4], loop_region: None, accent };
                waveform::paint_wave(&p, plot.shrink2(vec2(0.0, 2.0)), &wp, t0, t1, true);
                p.rect_filled(plot, 3.0, Color32::from_black_alpha(150));
            }
            p.rect_stroke(plot, 3.0, Stroke::new(1.0, accent.linear_multiply(0.6)), StrokeKind::Inside);
            // sub-lanes
            let n = params.len().max(1);
            let sub_h = rr.height() / n as f32;
            for (j, (param, name, col)) in params.iter().enumerate() {
                let sr = Rect::from_min_size(pos2(plot_l, rr.top() + j as f32 * sub_h), vec2(plot_w, sub_h));
                if j % 2 == 1 {
                    p.rect_filled(sr, 0.0, Color32::from_white_alpha(8));
                }
                if j > 0 {
                    p.line_segment([sr.left_top(), sr.right_top()], Stroke::new(1.0, Color32::from_white_alpha(40)));
                }
                p.text(pos2(lab.right() - 6.0, sr.center().y + if n == 1 { 8.0 } else { 0.0 }), egui::Align2::RIGHT_CENTER, *name, egui::FontId::proportional(11.0), *col);
                let inner = sr.shrink2(vec2(0.0, 3.0));
                let centered = matches!(param, Param::EqLow | Param::EqMid | Param::EqHigh | Param::Filter);
                let tr = &self.mix.transitions[k];
                let to_y = |v: f32| inner.bottom() - v.clamp(0.0, 1.0) * inner.height();
                if centered {
                    p.line_segment([pos2(inner.left(), to_y(0.5)), pos2(inner.right(), to_y(0.5))], Stroke::new(1.0, Color32::from_white_alpha(30)));
                }
                // filled area under the curve (stems) or line (others)
                let steps = (plot_w / 3.0).max(16.0) as usize;
                let mut pts = Vec::with_capacity(steps + 1);
                for s in 0..=steps {
                    let f = s as f32 / steps as f32;
                    let v = tr.value(side, *param, f * beats);
                    pts.push(pos2(inner.left() + f * inner.width(), to_y(v)));
                }
                if matches!(param, Param::Stem(_)) {
                    for w in pts.windows(2) {
                        p.add(egui::Shape::convex_polygon(vec![w[0], w[1], pos2(w[1].x, inner.bottom()), pos2(w[0].x, inner.bottom())], col.linear_multiply(0.35), Stroke::NONE));
                    }
                }
                p.add(egui::Shape::line(pts, Stroke::new(2.0, *col)));
                // points
                if let Some(lane) = tr.lanes.iter().find(|l| l.side == side && l.param == *param) {
                    for (bx, v) in &lane.points {
                        p.circle_filled(pos2(x_of_beat(*bx), to_y(*v)), 3.5, *col);
                        p.circle_stroke(pos2(x_of_beat(*bx), to_y(*v)), 3.5, Stroke::new(1.0, Color32::BLACK));
                    }
                }
                // editing
                let resp = ui.interact(sr, Id::new(("tedit", k, row, j)), Sense::click_and_drag());
                if let Some(pp) = resp.hover_pos() {
                    let b = ((pp.x - inner.left()) / inner.width() * beats).clamp(0.0, beats);
                    let v = tr.value(side, *param, b);
                    p.text(pos2(pp.x + 8.0, inner.top()), egui::Align2::LEFT_TOP, format!("bar {:.2} · {:.0}%", b / 4.0 + 1.0, v * 100.0), egui::FontId::proportional(10.0), Color32::WHITE);
                }
                if resp.dragged() || resp.clicked() {
                    if let Some(pp) = resp.interact_pointer_pos() {
                        let b = (((pp.x - inner.left()) / inner.width() * beats).clamp(0.0, beats) * 2.0).round() / 2.0;
                        let v = (1.0 - (pp.y - inner.top()) / inner.height()).clamp(0.0, 1.0);
                        let v = if (v - 0.5).abs() < 0.03 && centered { 0.5 } else { v };
                        crate::studio::set_point(&mut self.mix.transitions[k], side, *param, b, v);
                        self.mix.transitions[k].style = TransitionStyle::Custom;
                        edited = true;
                    }
                }
                if resp.secondary_clicked() {
                    if let Some(pp) = resp.interact_pointer_pos() {
                        let b = ((pp.x - inner.left()) / inner.width() * beats).clamp(0.0, beats);
                        if let Some(lane) = self.mix.transitions[k].lanes.iter_mut().find(|l| l.side == side && l.param == *param) {
                            if let Some((idx, _)) = lane.points.iter().enumerate().min_by(|a, c| (a.1 .0 - b).abs().total_cmp(&(c.1 .0 - b).abs())) {
                                if lane.points.len() > 1 {
                                    lane.points.remove(idx);
                                    self.mix.transitions[k].style = TransitionStyle::Custom;
                                    edited = true;
                                }
                            }
                        }
                    }
                }
            }
        }
        // playhead within the window
        if self.position >= x.start_sec && self.position <= x.end_sec {
            let b = (self.timeline.beat_at_sec(self.position) - x.start_beat) as f32;
            let xx = x_of_beat(b);
            p.line_segment([pos2(xx, rect.top()), pos2(xx, rect.bottom())], Stroke::new(1.5, Color32::WHITE));
        }
        edited
    }
}
