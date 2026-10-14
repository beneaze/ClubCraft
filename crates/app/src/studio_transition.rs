//! Transition editor: a header with the two tracks and the style, and below it an editor with
//! one clearly separated band per parameter for the outgoing and the incoming track, aligned on
//! a bar ruler.

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
        TransitionStyle::StemBlend => "Incoming drums first, outgoing vocals leave early, bass swapped halfway, melody crossfades.",
        TransitionStyle::BassSwap => "Classic DJ blend: new track comes in without bass, the low end swaps halfway.",
        TransitionStyle::Crossfade => "Smooth volume crossfade of both tracks.",
        TransitionStyle::VocalOver => "Outgoing vocals keep singing over the new track's instrumental.",
        TransitionStyle::FilterOut => "High-pass sweep out of the old track while the new one comes in.",
        TransitionStyle::EchoOut => "Old track echoes out, new track drops in on the downbeat.",
        TransitionStyle::Cut => "Hard cut on the downbeat.",
        TransitionStyle::Custom => "Your own curves.",
    }
}

const LABEL_W: f32 = 118.0;
const BAND_H: f32 = 21.0;
const WAVE_H: f32 = 22.0;

impl StudioView {
    pub(crate) fn transition_editor(&mut self, ui: &mut Ui, sh: &Shared, k: usize) {
        let (ia, ib) = (self.mix.tracks[k].track_id, self.mix.tracks[k + 1].track_id);
        let title = |id| sh.track(id).map(|t| t.title.clone()).unwrap_or_default();
        let key = |i: usize, id| sh.analyses.get(&id).and_then(|a| a.key).map(|kk| kk.shifted(self.mix.tracks[i].key_shift));
        let (ka, kb) = (key(k, ia), key(k + 1, ib));
        let mut changed = false;

        // ------------------------------------------------------------ header
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Transition {} ▶ {}", k + 1, k + 2)).size(15.0).strong());
            ui.add_space(8.0);
            for (n, (id, kk, col)) in [(ia, ka, theme::deck_color(k % 2)), (ib, kb, theme::deck_color((k + 1) % 2))].into_iter().enumerate() {
                if n == 1 {
                    ui.label(RichText::new("▶").color(theme::TEXT_DIM));
                }
                if let Some(kk) = kk {
                    key_badge(ui, kk);
                }
                ui.label(RichText::new(crate::studio::truncate(&title(id), 30)).color(col));
            }
            if let (Some(a), Some(b)) = (ka, kb) {
                let c = a.compatibility(b);
                let (txt, col) = if c >= 0.85 { ("harmonic", theme::OK) } else if c >= 0.5 { ("acceptable", theme::WARN) } else { ("key clash", Color32::from_rgb(255, 90, 90)) };
                ui.label(RichText::new(txt).color(col).size(12.0)).on_hover_text(if c < 0.5 { "Try a key shift on one of the tracks (select the track)" } else { "Camelot-compatible keys" });
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let tr = &mut self.mix.transitions[k];
            ui.label(RichText::new("Style").color(theme::TEXT_DIM));
            egui::ComboBox::from_id_salt(("tstyle", k)).width(130.0).selected_text(tr.style.label()).show_ui(ui, |ui| {
                for s in TransitionStyle::ALL {
                    if ui.selectable_label(tr.style == s, s.label()).on_hover_text(style_help(s)).clicked() && tr.style != s {
                        *tr = Transition::new(s, tr.beats);
                        changed = true;
                    }
                }
            });
            ui.add_space(10.0);
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
            ui.add_space(10.0);
            ui.label(RichText::new("Move").color(theme::TEXT_DIM));
            let bar = self.timeline.tracks.get(k).map(|t| t.grid.bar_len()).unwrap_or(2.0);
            for (lbl, d) in [("−8", -8.0), ("−1", -1.0), ("+1", 1.0), ("+8", 8.0)] {
                if ui.small_button(lbl).on_hover_text("Move the transition by bars (the incoming track moves with it)").clicked() {
                    let mt = &mut self.mix.tracks[k];
                    let nv = mt.mix_out + d * bar;
                    if nv > mt.mix_in + 2.0 * bar {
                        mt.mix_out = nv;
                        changed = true;
                    }
                }
            }
            let tr = &mut self.mix.transitions[k];
            if tr.style == TransitionStyle::Custom && ui.small_button("Reset curves").clicked() {
                *tr = Transition::new(TransitionStyle::StemBlend, tr.beats);
                changed = true;
            }
            ui.label(RichText::new(style_help(tr.style)).size(11.0).color(theme::TEXT_DIM));
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            for g in ParamGroup::ALL {
                if ui.add(egui::Button::new(RichText::new(g.label()).size(12.5)).selected(self.tgroup == g).min_size(vec2(96.0, 22.0))).clicked() {
                    self.tgroup = g;
                }
            }
            ui.add_space(8.0);
            ui.label(RichText::new("drag in a band to draw · right-click removes a point").size(11.0).color(theme::TEXT_DIM));
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical().id_salt(("tedit-scroll", k)).show(ui, |ui| {
            changed |= self.aligned_editor(ui, sh, k);
        });
        if changed {
            self.changed();
        }
    }

    /// Ruler + OUT block + IN block. Returns true if edited.
    fn aligned_editor(&mut self, ui: &mut Ui, sh: &Shared, k: usize) -> bool {
        let Some(x) = self.timeline.transitions.get(k).cloned() else { return false };
        let beats = self.mix.transitions[k].beats as f32;
        let params = self.tgroup.params();
        let block_h = WAVE_H + 4.0 + params.len() as f32 * (BAND_H + 3.0);
        let width = ui.available_width();
        let ruler_h = 18.0;
        let total_h = ruler_h + 6.0 + 2.0 * block_h + 12.0;
        let (rect, _) = ui.allocate_exact_size(vec2(width, total_h), Sense::hover());
        let plot_l = rect.left() + LABEL_W;
        let plot_w = rect.width() - LABEL_W - 6.0;
        let x_of_beat = |b: f32| plot_l + b / beats * plot_w;
        let p = ui.painter_at(rect);

        // ruler
        let ruler = Rect::from_min_size(pos2(plot_l, rect.top()), vec2(plot_w, ruler_h));
        p.rect_filled(ruler, 3.0, theme::PANEL_HI);
        for b in 0..=(beats as i32) {
            let xx = x_of_beat(b as f32);
            if b % 4 == 0 {
                p.line_segment([pos2(xx, ruler.bottom()), pos2(xx, rect.bottom())], Stroke::new(1.0, Color32::from_white_alpha(22)));
                if b < beats as i32 {
                    p.text(pos2(xx + 4.0, ruler.center().y), egui::Align2::LEFT_CENTER, format!("bar {}", b / 4 + 1), egui::FontId::proportional(10.5), Color32::from_rgb(220, 210, 120));
                }
            } else if plot_w / beats > 12.0 {
                p.line_segment([pos2(xx, ruler.bottom() - 4.0), pos2(xx, ruler.bottom())], Stroke::new(1.0, theme::LINE));
            }
        }
        let mut edited = false;
        let mut y = ruler.bottom() + 6.0;
        for (side, ti) in [(Side::Out, k), (Side::In, k + 1)] {
            let accent = theme::deck_color(ti % 2);
            let block = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), block_h));
            p.rect_filled(block, 6.0, Color32::from_rgb(22, 24, 29));
            p.rect_filled(Rect::from_min_size(block.min, vec2(4.0, block.height())), 2.0, accent);
            // label + waveform strip
            let tid = self.timeline.tracks[ti].track_id;
            let name = sh.track(tid).map(|t| t.title.clone()).unwrap_or_default();
            p.text(pos2(block.left() + 10.0, y + WAVE_H / 2.0), egui::Align2::LEFT_CENTER, if side == Side::Out { "OUT" } else { "IN" }, egui::FontId::proportional(12.0), accent);
            p.text(pos2(block.left() + 42.0, y + WAVE_H / 2.0), egui::Align2::LEFT_CENTER, crate::studio::truncate(&name, 11), egui::FontId::proportional(11.0), theme::TEXT_DIM);
            let wave = Rect::from_min_size(pos2(plot_l, y + 2.0), vec2(plot_w, WAVE_H - 4.0));
            if let (Some(wf), Some(a)) = (sh.waveforms.get(&tid), sh.analyses.get(&tid)) {
                let t0 = self.timeline.track_pos(ti, x.start_beat);
                let t1 = self.timeline.track_pos(ti, x.start_beat + x.beats);
                let wp = WaveParams { wf, grid: a.grid, cues: &[], sections: &[], style: WaveStyle::Stems, stem_gains: [0.8; 4], loop_region: None, accent };
                waveform::paint_wave(&p, wave, &wp, t0, t1, true);
            }
            let mut by = y + WAVE_H + 4.0;
            for (j, (param, label, col)) in params.iter().enumerate() {
                let band = Rect::from_min_size(pos2(plot_l, by), vec2(plot_w, BAND_H));
                let centered = matches!(param, Param::EqLow | Param::EqMid | Param::EqHigh | Param::Filter);
                p.rect_filled(band, 3.0, theme::BG);
                p.text(pos2(plot_l - 10.0, band.center().y), egui::Align2::RIGHT_CENTER, *label, egui::FontId::proportional(11.5), *col);
                let inner = band.shrink2(vec2(0.0, 3.0));
                let to_y = |v: f32| inner.bottom() - v.clamp(0.0, 1.0) * inner.height();
                let base_y = if centered { to_y(0.5) } else { inner.bottom() };
                if centered {
                    p.line_segment([pos2(inner.left(), base_y), pos2(inner.right(), base_y)], Stroke::new(1.0, Color32::from_white_alpha(35)));
                }
                let tr = &self.mix.transitions[k];
                let steps = (plot_w / 4.0).max(24.0) as usize;
                let pts: Vec<egui::Pos2> = (0..=steps)
                    .map(|s| {
                        let f = s as f32 / steps as f32;
                        pos2(inner.left() + f * inner.width(), to_y(tr.value(side, *param, f * beats)))
                    })
                    .collect();
                for w in pts.windows(2) {
                    p.add(egui::Shape::convex_polygon(vec![w[0], w[1], pos2(w[1].x, base_y), pos2(w[0].x, base_y)], col.linear_multiply(0.28), Stroke::NONE));
                }
                p.add(egui::Shape::line(pts, Stroke::new(1.8, *col)));
                let resp = ui.interact(band, Id::new(("tband", k, side == Side::In, j)), Sense::click_and_drag());
                if resp.hovered() || resp.dragged() {
                    p.rect_stroke(band, 3.0, Stroke::new(1.0, col.linear_multiply(0.6)), StrokeKind::Inside);
                    if let Some(lane) = tr.lanes.iter().find(|l| l.side == side && l.param == *param) {
                        for (bx, v) in &lane.points {
                            p.circle_filled(pos2(x_of_beat(*bx), to_y(*v)), 3.5, *col);
                        }
                    }
                    if let Some(pp) = resp.hover_pos() {
                        let b = ((pp.x - inner.left()) / inner.width() * beats).clamp(0.0, beats);
                        let v = tr.value(side, *param, b);
                        let txt = if centered { format!("{:+.0}", (v - 0.5) * 200.0) } else { format!("{:.0}%", v * 100.0) };
                        p.text(pos2(band.right() - 4.0, band.top() + 2.0), egui::Align2::RIGHT_TOP, format!("bar {:.2} · {txt}", b / 4.0 + 1.0), egui::FontId::proportional(10.0), Color32::WHITE);
                    }
                }
                if resp.dragged() || resp.clicked() {
                    if let Some(pp) = resp.interact_pointer_pos() {
                        let b = (((pp.x - inner.left()) / inner.width() * beats).clamp(0.0, beats) * 2.0).round() / 2.0;
                        let v = (1.0 - (pp.y - inner.top()) / inner.height()).clamp(0.0, 1.0);
                        let v = if centered && (v - 0.5).abs() < 0.04 { 0.5 } else { v };
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
                by += BAND_H + 3.0;
            }
            y += block_h + 6.0;
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

fn key_badge(ui: &mut Ui, k: clubcraft_core::Key) {
    let (r, _) = ui.allocate_exact_size(vec2(32.0, 18.0), Sense::hover());
    ui.painter().rect_filled(r, 4.0, theme::key_color(k));
    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, k.camelot(), egui::FontId::proportional(11.0), Color32::BLACK);
}
