//! Transition editor: a header with the two tracks and the style, and below it presets and a
//! choice per stem / band / effect. The curves themselves are drawn in the timeline.

use crate::app::Shared;
use crate::studio::StudioView;
use crate::studio_timeline::{EQ_COLORS, FILTER_COLOR};
use crate::theme;
use clubcraft_core::{Mode, Param, StemKind, TrackId, Transition, TransitionStyle};
use eframe::egui::{self, vec2, Color32, RichText, Sense, Stroke, Ui};

/// Categories of the transition editor (DJ.Studio-like): simple choices per stem / band; the
/// curves themselves are drawn in the timeline.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TCategory {
    #[default]
    Stems,
    /// Volume and the three EQ bands side by side (as DJ.Studio's Equalizer category).
    Equalizer,
    Effects,
    /// Effects that work on the beat: loop, brake, side-chain pump.
    Rhythm,
}

impl TCategory {
    pub const ALL: [TCategory; 4] = [TCategory::Stems, TCategory::Equalizer, TCategory::Effects, TCategory::Rhythm];
    fn label(self) -> &'static str {
        match self {
            TCategory::Stems => "Stems",
            TCategory::Equalizer => "Equalizer",
            TCategory::Effects => "Effects",
            TCategory::Rhythm => "Rhythm FX",
        }
    }
    fn params(self) -> Vec<Param> {
        self.columns().into_iter().map(|c| c.0).collect()
    }
    fn columns(self) -> Vec<(Param, &'static str, Color32)> {
        match self {
            TCategory::Stems => [StemKind::Drums, StemKind::Bass, StemKind::Other, StemKind::Vocals].into_iter().map(|k| (Param::Stem(k), k.label(), theme::stem_color(k))).collect(),
            TCategory::Equalizer => vec![(Param::Volume, "Volume", Color32::from_gray(220)), (Param::EqLow, "Low", EQ_COLORS[0]), (Param::EqMid, "Mid", EQ_COLORS[1]), (Param::EqHigh, "High", EQ_COLORS[2])],
            TCategory::Effects => vec![(Param::Filter, "Filter", FILTER_COLOR), (Param::Echo, "Echo", theme::FX_ECHO), (Param::Reverb, "Reverb", theme::FX_REVERB), (Param::Noise, "Noise", theme::FX_NOISE)],
            TCategory::Rhythm => vec![(Param::Loop, "Loop", theme::FX_LOOP), (Param::Brake, "Vinyl brake", theme::FX_BRAKE), (Param::Pump, "Side-chain", theme::FX_PUMP)],
        }
    }
}

fn mode_help(p: Param, m: Mode) -> &'static str {
    match (p, m) {
        (Param::Stem(_), Mode::Off) => "Silent on both tracks during the transition",
        (Param::Stem(_), Mode::Swap) => "The old track's stem plays until the middle, then the new one's",
        (Param::Stem(_), Mode::Crossfade) => "Handed over between 40 % and 60 % of the transition",
        (Param::Stem(_), Mode::Full) => "Both tracks keep it for the whole transition",
        (Param::Pump, Mode::Crossfade) => "The old track pumps deeper and deeper on every beat while the new one stops pumping",
        (Param::Pump, Mode::Swap) => "The new track pumps until the middle, the old one from the middle on",
        (_, Mode::Off) => "No change",
        (_, Mode::Swap) => "Switch at the middle of the transition",
        (_, Mode::Crossfade) => "Gradual handover",
        (_, Mode::Full) => "Both tracks at full level throughout (stems do the mixing)",
        (_, Mode::Half) => "Old track fades over the second half, new one comes up over the first",
        (_, Mode::SweepOut) => "High-pass sweep out of the old track",
        (_, Mode::SweepIn) => "Low-pass sweep into the new track",
        (_, Mode::FilterDown) => "The old track closes down with a low-pass",
        (_, Mode::SweepBoth) => "High-pass out of the old track while the new one opens from a low-pass",
        (_, Mode::EchoOut) => "The old track echoes out over the last bar",
        (_, Mode::EchoThrow) => "The beat before the middle is thrown into the echo and repeats over the swap",
        (_, Mode::EchoBuild) => "The echo on the old track builds up over the second half",
        (_, Mode::ReverbOut) => "The old track dissolves into reverb over the last two bars (best when its volume fades)",
        (_, Mode::ReverbIn) => "The new track rises out of reverb over the first half",
        (_, Mode::Wash) => "The old track washes out in reverb while the new one rises out of it",
        (_, Mode::Riser) => "Noise sweeps up over the second half and cuts off at the end",
        (_, Mode::Downsweep) => "Noise falls away from the middle to the end",
        (_, Mode::Swoosh) => "A short swell of noise around the middle",
        (_, Mode::LoopOut) => "From the middle on, the old track repeats one bar",
        (_, Mode::RollOut) => "The old track rolls over the last two bars: one bar, then 2, 1 and ½ beat",
        (_, Mode::LoopIn) => "The new track repeats its first bar until the middle",
        (_, Mode::BrakeOut) => "The old track stops like a turntable over the last bar (best with volume Full or Swap)",
        (_, Mode::BrakeSwap) => "The old track brakes over the half bar before the middle and stays stopped",
        (_, Mode::PumpOut) => "The old track ducks on every beat, deeper and deeper, so the new kick comes through",
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
/// Painted by hand: egui's `Button` derives its margins from the hover state's stroke, so a
/// fixed outline made the chosen box shift by a pixel under the pointer.
fn mode_button(ui: &mut Ui, selected: bool, label: &str, col: Color32) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(128.0, MODE_H), Sense::click());
    let p = ui.painter();
    let radius = ui.visuals().widgets.inactive.corner_radius;
    let font = egui::TextStyle::Button.resolve(ui.style());
    if selected {
        p.rect_filled(r, radius, theme::blend(theme::PANEL_HI, col, 0.45));
        p.rect_stroke(r, radius, Stroke::new(1.5, col), egui::StrokeKind::Inside);
        p.text(r.center(), egui::Align2::CENTER_CENTER, label, font, Color32::WHITE);
    } else {
        if resp.hovered() {
            p.rect_filled(r, radius, ui.visuals().widgets.hovered.weak_bg_fill);
        }
        p.text(r.center(), egui::Align2::CENTER_CENTER, label, font, theme::TEXT);
    }
    resp
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
        TransitionStyle::MashupIn => "The new track's vocals sing over the old track's instrumental for the whole transition.",
        TransitionStyle::Custom => "Your own curves.",
    }
}

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
            // the symbol only shows while it's on, as on the timeline
            let lock_size = vec2(78.0, 22.0);
            let lock = if locked { crate::widgets::icon_toggle(ui, true, "Locked", theme::accent(), lock_size, |p, r, c| crate::widgets::lock_icon(p, r, true, c)) } else { crate::widgets::toggle(ui, false, "Lock", theme::accent(), lock_size) };
            if lock.on_hover_text("A locked transition keeps its mix points, length and curves when you harmonize or recompute mix points, and can't be edited or dragged").clicked() {
                self.mix.transitions[k].locked = !locked;
                changed = true;
            }
            let flagged = self.mix.transitions[k].flagged;
            let flag_size = vec2(70.0, 22.0);
            let flag = if flagged { crate::widgets::icon_toggle(ui, true, "To do", theme::FLAG, flag_size, crate::widgets::flag_icon) } else { crate::widgets::toggle(ui, false, "Flag", theme::FLAG, flag_size) };
            if flag.on_hover_text("Flag this transition as one you still need to work on (a reminder only, it changes nothing in the mix)").clicked() {
                self.mix.transitions[k].flagged = !flagged;
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
                            *tr = Transition { flagged: tr.flagged, ..Transition::new(st, tr.beats) };
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
                        let label = if n > 0 { format!("{}   {n}", c.label()) } else { c.label().to_string() };
                        if ui.add_sized([150.0, 26.0], egui::Button::selectable(self.tcat == c, label)).clicked() {
                            self.tcat = c;
                        }
                    }
                });
                ui.vertical(|ui| {
                    let cat = self.tcat;
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
                });
            });
        });
        if let Some(p) = manual {
            // draw it in the timeline: its stem row, its effect row (shown even while unused) or
            // its curve
            self.curve_target = crate::studio_timeline::CurveTarget::of(p);
            if self.curve_target == crate::studio_timeline::CurveTarget::Effects {
                self.fx_shown.insert((k, p));
            }
        }
        if changed {
            self.changed();
        }
    }
}

fn key_badge(ui: &mut Ui, k: clubcraft_core::Key) {
    let (r, _) = ui.allocate_exact_size(vec2(32.0, 18.0), Sense::hover());
    ui.painter().rect_filled(r, 4.0, theme::key_color(k));
    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, crate::theme::key_label(k), egui::FontId::proportional(11.0), Color32::BLACK);
}
