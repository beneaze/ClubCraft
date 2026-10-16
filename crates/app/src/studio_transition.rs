//! Transition editor: a header with the two tracks and the style, and below it an editor with
//! one clearly separated band per parameter for the outgoing and the incoming track, aligned on
//! a bar ruler.

use crate::app::Shared;
use crate::studio::StudioView;
use crate::studio_timeline::{EQ_COLORS, FILTER_COLOR, VOLUME_COLOR};
use crate::theme;
use crate::waveform::{self, WaveParams, WaveStyle};
use clubcraft_core::{Mode, Param, Side, StemKind, TrackId, Transition, TransitionStyle};
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
            ParamGroup::Stems => [StemKind::Vocals, StemKind::Other, StemKind::Bass, StemKind::Drums].iter().map(|k| (Param::Stem(*k), k.label(), theme::stem_color(*k))).collect(),
            ParamGroup::Volume => vec![(Param::Volume, "Volume", VOLUME_COLOR)],
            ParamGroup::Eq => vec![(Param::EqHigh, "High", EQ_COLORS[2]), (Param::EqMid, "Mid", EQ_COLORS[1]), (Param::EqLow, "Low", EQ_COLORS[0])],
            ParamGroup::FilterFx => vec![(Param::Filter, "Filter", FILTER_COLOR), (Param::Echo, "Echo", Color32::from_rgb(255, 160, 90))],
        }
    }
}

/// Categories of the transition editor (DJ.Studio-like): simple choices per stem / band, or the
/// curve editor.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TCategory {
    #[default]
    Stems,
    /// Volume and the three EQ bands side by side (as DJ.Studio's Equalizer category).
    Equalizer,
    Effects,
    Curves,
}

impl TCategory {
    pub const ALL: [TCategory; 4] = [TCategory::Stems, TCategory::Equalizer, TCategory::Effects, TCategory::Curves];
    fn label(self) -> &'static str {
        match self {
            TCategory::Stems => "Stems",
            TCategory::Equalizer => "Equalizer",
            TCategory::Effects => "Effects",
            TCategory::Curves => "Curves (draw)",
        }
    }
    fn params(self) -> Vec<Param> {
        self.columns().into_iter().map(|c| c.0).collect()
    }
    fn columns(self) -> Vec<(Param, &'static str, Color32)> {
        match self {
            TCategory::Stems => [StemKind::Drums, StemKind::Bass, StemKind::Other, StemKind::Vocals].into_iter().map(|k| (Param::Stem(k), k.label(), theme::stem_color(k))).collect(),
            TCategory::Equalizer => vec![(Param::Volume, "Volume", Color32::from_gray(220)), (Param::EqLow, "Low", EQ_COLORS[0]), (Param::EqMid, "Mid", EQ_COLORS[1]), (Param::EqHigh, "High", EQ_COLORS[2])],
            TCategory::Effects => vec![(Param::Filter, "Filter", FILTER_COLOR), (Param::Echo, "Echo", Color32::from_rgb(255, 160, 90))],
            TCategory::Curves => vec![],
        }
    }
}

fn mode_help(p: Param, m: Mode) -> &'static str {
    match (p, m) {
        (Param::Stem(_), Mode::Off) => "Silent on both tracks during the transition",
        (Param::Stem(_), Mode::Swap) => "The old track's stem plays until the middle, then the new one's",
        (Param::Stem(_), Mode::Crossfade) => "Handed over between 40 % and 60 % of the transition",
        (Param::Stem(_), Mode::Full) => "Both tracks keep it for the whole transition",
        (_, Mode::Off) => "No change",
        (_, Mode::Swap) => "Switch at the middle of the transition",
        (_, Mode::Crossfade) => "Gradual handover",
        (_, Mode::Full) => "Both tracks at full level throughout (stems do the mixing)",
        (_, Mode::Half) => "Old track fades over the second half, new one comes up over the first",
        (_, Mode::SweepOut) => "High-pass sweep out of the old track",
        (_, Mode::SweepIn) => "Low-pass sweep into the new track",
        (_, Mode::EchoOut) => "The old track echoes out over the last bar",
    }
}

/// Mode name in its column: for stems "Off" silences the stem, for every other parameter it
/// leaves the parameter alone, so there it is called "None".
fn mode_label(p: Param, m: Mode) -> &'static str {
    match (p, m) {
        (Param::Stem(_), _) => m.label(),
        (_, Mode::Off) => "None",
        _ => m.label(),
    }
}

/// Height of a mode entry: five modes and "Manual" fit the editor's default height.
const MODE_H: f32 = 22.0;

/// A titled column in the transition editor.
fn column(ui: &mut Ui, w: f32, h: f32, title: &str, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new().fill(theme::PANEL_HI).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
        ui.set_width(w - 16.0);
        ui.set_min_height(h - 16.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.label(RichText::new(title).strong());
            ui.add_space(4.0);
            add(ui);
        });
    });
}

/// A choice column with a coloured header (one stem / band).
fn mode_column(ui: &mut Ui, title: &str, col: Color32, h: f32, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new().fill(theme::blend(theme::PANEL_HI, col, 0.07)).corner_radius(6.0).show(ui, |ui| {
        ui.set_width(144.0);
        ui.set_min_height(h);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let (r, _) = ui.allocate_exact_size(vec2(144.0, 28.0), Sense::hover());
            // the stem's / band's own colour at full strength, so the columns read at a glance
            ui.painter().rect_filled(r, egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, col);
            ui.painter().text(r.left_center() + vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, title, egui::FontId::proportional(14.0), header_text(col));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.vertical(add);
            });
        });
    });
}

/// Dark text on light header colours, white on dark ones.
fn header_text(bg: Color32) -> Color32 {
    let l = 0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32;
    if l > 150.0 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

/// One mode entry: the chosen one is filled with the column's colour and outlined in it.
fn mode_button(ui: &mut Ui, selected: bool, label: &str, col: Color32) -> egui::Response {
    let b = if selected {
        egui::Button::new(RichText::new(label).color(Color32::WHITE).strong()).fill(theme::blend(theme::PANEL_HI, col, 0.45)).stroke(Stroke::new(1.5, col))
    } else {
        egui::Button::new(label).fill(Color32::TRANSPARENT).stroke(Stroke::NONE)
    };
    ui.add_sized([128.0, MODE_H], b)
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
        TransitionStyle::Mashup => "The old track's vocals sing over the new track's instrumental for the whole transition.",
        TransitionStyle::Custom => "Your own curves.",
    }
}

const LABEL_W: f32 = 118.0;
const BAND_H: f32 = 21.0;
const WAVE_H: f32 = 22.0;

impl StudioView {
    /// Level of the incoming track for 8 bars after transition `k` minus the outgoing track's level
    /// for 8 bars before it (dB, from the waveform summaries, loudness-normalised).
    fn level_jump_db(&self, sh: &Shared, k: usize) -> Option<f32> {
        let (a, b) = (self.mix.tracks.get(k)?, self.mix.tracks.get(k + 1)?);
        let x = self.timeline.transitions.get(k)?;
        let level = |id: TrackId, t0: f64, t1: f64| -> Option<f32> {
            let wf = sh.waveforms.get(&id)?;
            let (i0, i1) = ((t0.max(0.0) * wf.rate) as usize, ((t1 * wf.rate) as usize).min(wf.peak.len()));
            if i1 <= i0 + 4 {
                return None;
            }
            // summaries store 255·√peak: square back to amplitude, then the RMS of the peaks
            let ms = wf.peak[i0..i1].iter().map(|&v| (v as f32 / 255.0).powi(4)).sum::<f32>() / (i1 - i0) as f32;
            Some(10.0 * ms.max(1e-8).log10() + sh.analyses.get(&id).and_then(|an| an.gain_db).unwrap_or(0.0))
        };
        let (ga, gb) = (sh.analyses.get(&a.track_id)?.grid?, sh.analyses.get(&b.track_id)?.grid?);
        let out = level(a.track_id, a.mix_out - 8.0 * ga.bar_len(), a.mix_out)?;
        let inc_start = b.mix_in + x.beats * gb.beat_len();
        let inc = level(b.track_id, inc_start, inc_start + 8.0 * gb.bar_len())?;
        Some(inc + b.gain_db - (out + a.gain_db))
    }

    pub(crate) fn transition_editor(&mut self, ui: &mut Ui, sh: &Shared, k: usize) {
        let (ia, ib) = (self.mix.tracks[k].track_id, self.mix.tracks[k + 1].track_id);
        let title = |id| sh.track(id).map(|t| t.title.clone()).unwrap_or_default();
        let key = |i: usize, id| sh.analyses.get(&id).and_then(|a| a.key).map(|kk| kk.shifted(self.mix.tracks[i].key_shift));
        let (ka, kb) = (key(k, ia), key(k + 1, ib));
        let n_tr = self.mix.transitions.len();
        let locked = self.mix.transitions[k].locked;
        let mut changed = false;
        let mut manual: Option<Param> = None;

        // ------------------------------------------------------------ header
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if ui.add_enabled(k > 0, egui::Button::new("‹").min_size(vec2(24.0, 24.0))).on_hover_text("Previous transition ([)").clicked() {
                self.selection = crate::studio::Selection::Transition(k - 1);
            }
            ui.label(RichText::new(format!("Transition {}", k + 1)).size(15.0).strong());
            if ui.add_enabled(k + 1 < n_tr, egui::Button::new("›").min_size(vec2(24.0, 24.0))).on_hover_text("Next transition (])").clicked() {
                self.selection = crate::studio::Selection::Transition(k + 1);
            }
            let lock_txt = if locked { "🔒 Locked" } else { "🔓 Lock" };
            if crate::widgets::toggle(ui, locked, lock_txt, theme::accent(), vec2(78.0, 22.0)).on_hover_text("A locked transition keeps its mix points, length and curves when you harmonize or recompute mix points, and can't be edited or dragged").clicked() {
                self.mix.transitions[k].locked = !locked;
                changed = true;
            }
            ui.add_space(6.0);
            for (n, (id, kk, col)) in [(ia, ka, theme::deck_color(k % 2)), (ib, kb, theme::deck_color((k + 1) % 2))].into_iter().enumerate() {
                if n == 1 {
                    ui.label(RichText::new("▶").color(theme::TEXT_DIM));
                }
                if let Some(kk) = kk {
                    key_badge(ui, kk);
                }
                ui.label(RichText::new(crate::studio::truncate(&title(id), 26)).color(col));
            }
            if let (Some(a), Some(b)) = (ka, kb) {
                let c = a.compatibility(b);
                let (txt, col) = if c >= 0.85 { ("harmonic", theme::OK) } else if c >= 0.5 { ("acceptable", theme::WARN) } else { ("key clash", Color32::from_rgb(255, 90, 90)) };
                ui.label(RichText::new(txt).color(col).size(12.0));
            }
            ui.separator();
            ui.add_enabled_ui(!locked, |ui| {
                let tr = &mut self.mix.transitions[k];
                for bars in [4u32, 8, 16, 32] {
                    if ui.selectable_label(tr.beats == bars * 4, format!("{bars}")).on_hover_text("Length in bars").clicked() && tr.beats != bars * 4 {
                        tr.set_beats(bars * 4);
                        changed = true;
                    }
                }
                ui.label(RichText::new("bars").color(theme::TEXT_DIM));
                ui.add_space(6.0);
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
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("🎤 Find vocal-over point").on_hover_text("Find where the outgoing vocals sound best over the incoming instrumental").clicked() {
                    self.pending.push(crate::studio::StudioAction::FindVocalOver(k));
                }
            });
        });
        // what you hear differs from the curves: no stems yet
        if self.mix.transitions[k].uses_stems() && !self.stems_ok(sh, k) {
            let missing: Vec<TrackId> = [ia, ib].into_iter().filter(|id| !sh.analyses.get(id).is_some_and(|a| a.stems_model.is_some())).collect();
            ui.horizontal(|ui| {
                let names: Vec<String> = missing.iter().map(|id| crate::studio::truncate(&title(*id), 28)).collect();
                ui.label(RichText::new(format!("No stems yet for {}: its stem curves are imitated with volume and EQ, which only approximates the sound.", names.join(" and "))).color(theme::WARN).size(12.0));
                if ui.button("Separate stems now").on_hover_text("Queue stem separation for these tracks first").clicked() {
                    self.pending.push(crate::studio::StudioAction::SeparateStems(missing.clone()));
                }
            });
        }
        // the outgoing track must still be playing when the transition ends
        if let (Some(a), Some(x)) = (sh.analyses.get(&ia), self.timeline.transitions.get(k)) {
            let mt = &self.mix.tracks[k];
            let bl = a.grid.map(|g| g.beat_len()).unwrap_or(0.5);
            let needed = mt.mix_out + x.beats * bl;
            // where the audio really ends: the last moment above −36 dB (files often end in a
            // fade and silence), else the file length
            let end = sh
                .waveforms
                .get(&ia)
                .and_then(|wf| wf.peak.iter().rposition(|&v| v >= 32).map(|i| (i + 1) as f64 / wf.rate))
                .map(|e| e.min(a.duration))
                .unwrap_or(a.duration);
            if end > 0.0 && needed > end + 0.25 {
                let mut fit = false;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} fades out {:.1} s before this transition ends, so it goes quiet early.", crate::studio::truncate(&title(ia), 30), needed - end)).color(Color32::from_rgb(255, 110, 100)).size(12.0));
                    fit = ui.button("Fit").on_hover_text("Start the transition earlier (on a bar) so it ends before the track fades out").clicked();
                });
                if fit {
                    let start = end - x.beats * bl;
                    let start = a.grid.map(|g| g.time_of_bar(g.bar_at(start).floor())).unwrap_or(start).max(mt.mix_in + 1.0);
                    self.mix.tracks[k].mix_out = start;
                    changed = true;
                }
            }
        }
        // level jump: the incoming section is much quieter than the outgoing one (e.g. a soft intro)
        if let Some(d) = self.level_jump_db(sh, k) {
            if d <= -6.0 {
                ui.label(RichText::new(format!(
                    "{} is about {:.0} dB quieter after this transition than {} before it (its mix-in sits in a soft part), so the mix gets softer here. Move its mix-in to a fuller section if that's not what you want.",
                    crate::studio::truncate(&title(ib), 30),
                    -d,
                    crate::studio::truncate(&title(ia), 30)
                ))
                .color(theme::TEXT_DIM)
                .size(12.0));
            }
        }
        if locked {
            ui.label(RichText::new("Locked: harmonize and automatic mix points keep this transition as it is. Unlock it to change it.").color(theme::TEXT_DIM).size(12.0));
        }
        ui.add_space(4.0);

        // ------------------------------------------------------------ presets · categories · choices
        ui.add_enabled_ui(!locked, |ui| {
            ui.horizontal_top(|ui| {
                let h = (ui.available_height() - 4.0).max(150.0);
                column(ui, 190.0, h, "Presets", |ui| {
                    let tr = &mut self.mix.transitions[k];
                    for st in TransitionStyle::ALL.into_iter().filter(|s| *s != TransitionStyle::Custom) {
                        let sel = tr.style == st;
                        if ui.add_sized([170.0, 24.0], egui::Button::selectable(sel, st.label())).on_hover_text(style_help(st)).clicked() && !sel {
                            *tr = Transition::new(st, tr.beats);
                            changed = true;
                        }
                    }
                    if tr.style == TransitionStyle::Custom {
                        ui.label(RichText::new("Custom (edited)").size(11.0).color(theme::TEXT_DIM));
                    }
                });
                column(ui, 170.0, h, "Categories", |ui| {
                    let tr = &self.mix.transitions[k];
                    for c in TCategory::ALL {
                        let n = c.params().iter().filter(|p| tr.lanes.iter().any(|l| l.param == **p && l.points.iter().any(|q| (q.1 - p.neutral()).abs() > 1e-3))).count();
                        let label = if n > 0 && c != TCategory::Curves { format!("{}   {n}", c.label()) } else { c.label().to_string() };
                        if ui.add_sized([150.0, 26.0], egui::Button::selectable(self.tcat == c, label)).clicked() {
                            self.tcat = c;
                        }
                    }
                });
                ui.vertical(|ui| {
                    let cat = self.tcat;
                    if cat == TCategory::Curves {
                        ui.horizontal(|ui| {
                            for g in ParamGroup::ALL {
                                if ui.add(egui::Button::new(RichText::new(g.label()).size(12.5)).selected(self.tgroup == g).min_size(vec2(96.0, 22.0))).clicked() {
                                    self.tgroup = g;
                                }
                            }
                            ui.label(RichText::new("drag in a band to draw · right-click removes a point").size(11.0).color(theme::TEXT_DIM));
                        });
                        egui::ScrollArea::vertical().id_salt(("tedit-scroll", k)).show(ui, |ui| {
                            changed |= self.aligned_editor(ui, sh, k);
                        });
                    } else {
                        ui.horizontal_top(|ui| {
                            for (param, name, col) in cat.columns() {
                                let tr = &mut self.mix.transitions[k];
                                let cur = tr.mode(param);
                                mode_column(ui, name, col, h, |ui| {
                                    for &m in Mode::for_param(param) {
                                        if mode_button(ui, cur == Some(m), mode_label(param, m), col).on_hover_text(mode_help(param, m)).clicked() && cur != Some(m) {
                                            tr.set_mode(param, m);
                                            changed = true;
                                        }
                                    }
                                    // a curve none of the modes describe (a preset's own or a
                                    // hand-drawn one) is "Manual"; choosing it draws that curve
                                    if mode_button(ui, cur.is_none(), "Manual", col).on_hover_text("Draw this curve yourself in the timeline").clicked() {
                                        manual = Some(param);
                                    }
                                });
                            }
                        });
                    }
                });
            });
        });
        if let Some(p) = manual {
            // draw it in the timeline where it has a curve row there, else in the curve editor
            let target = match p {
                Param::Stem(_) => Some(crate::studio_timeline::CurveTarget::Stems),
                p => crate::studio_timeline::CurveTarget::ALL.into_iter().find(|t| t.param() == Some(p)),
            };
            match target {
                Some(t) => self.curve_target = t,
                None => {
                    self.tcat = TCategory::Curves;
                    self.tgroup = ParamGroup::FilterFx;
                }
            }
        }
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
                let wp = WaveParams { wf, grid: a.grid, cues: &[], sections: &[], style: WaveStyle::Stems, stem_gains: [0.8; 4], loop_region: None, accent, volume: 1.0 };
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
    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, crate::theme::key_label(k), egui::FontId::proportional(11.0), Color32::BLACK);
}
