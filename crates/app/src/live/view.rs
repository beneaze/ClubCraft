//! Live performance screen (VirtualDJ-style): stacked scrolling waveforms, two decks, mixer,
//! and the browser.

use super::model::*;
use crate::library_view::DraggedTracks;
use crate::theme;
use crate::waveform::{self, WaveParams, WaveStyle};
use crate::widgets;
use clubcraft_core::{CueKind, StemKind, HOT_CUES};
use eframe::egui::{self, pos2, vec2, Align, Color32, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PadMode {
    HotCues,
    Stems,
    Loops,
    Roll,
    BeatJump,
}

impl PadMode {
    const ALL: [PadMode; 5] = [PadMode::HotCues, PadMode::Stems, PadMode::Loops, PadMode::Roll, PadMode::BeatJump];
    fn label(self) -> &'static str {
        match self {
            PadMode::HotCues => "Hot Cues",
            PadMode::Stems => "Stems",
            PadMode::Loops => "Loop",
            PadMode::Roll => "Roll",
            PadMode::BeatJump => "Jump",
        }
    }
}

pub struct LiveView {
    pub pad_mode: [PadMode; 4],
    /// Seconds visible in the scrolling waveforms.
    pub zoom: f64,
    pub wave_style: WaveStyle,
    held: Option<(usize, u8, PadMode)>,
    cue_held: Option<usize>,
    scrubbing: Option<usize>,
    pub browser_height: f32,
}

impl Default for LiveView {
    fn default() -> Self {
        LiveView {
            pad_mode: [PadMode::HotCues; 4],
            zoom: 8.0,
            wave_style: WaveStyle::Stems,
            held: None,
            cue_held: None,
            scrubbing: None,
            browser_height: 260.0,
        }
    }
}

const LOOP_SIZES: [f64; 8] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
const ROLL_SIZES: [f64; 8] = [1.0 / 32.0, 1.0 / 16.0, 0.125, 0.25, 0.5, 1.0, 2.0, 4.0];
const JUMPS: [f64; 8] = [-1.0, 1.0, -4.0, 4.0, -16.0, 16.0, -32.0, 32.0];

fn beats_label(b: f64) -> String {
    if b >= 1.0 {
        format!("{}", b as i64)
    } else {
        format!("1/{}", (1.0 / b).round() as i64)
    }
}

/// Detect press and release of a pad, also for touch-style holds.
fn press_release(ui: &Ui, resp: &egui::Response) -> (bool, bool) {
    let (pressed, released) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_released()));
    let over = resp.contains_pointer();
    (pressed && over, released)
}

impl LiveView {
    /// `decks` are drawn A, B (C, D in 4-deck mode, side by side pairs not yet supported).
    pub fn ui(&mut self, ui: &mut Ui, decks: &[DeckUi], mixer: &MixerUi, out: &mut Vec<LiveAction>, browser: impl FnOnce(&mut Ui)) {
        let full = ui.available_rect_before_wrap();
        let wave_h = 84.0;
        let waves = Rect::from_min_size(full.min, vec2(full.width(), wave_h * 2.0 + 6.0));
        self.waveforms(ui, waves, decks, out);
        let deck_top = waves.bottom() + 6.0;
        let deck_h = 404.0;
        let mixer_w = 300.0;
        let deck_w = (full.width() - mixer_w - 12.0) / 2.0;
        let a_rect = Rect::from_min_size(pos2(full.left(), deck_top), vec2(deck_w, deck_h));
        let m_rect = Rect::from_min_size(pos2(a_rect.right() + 6.0, deck_top), vec2(mixer_w, deck_h));
        let b_rect = Rect::from_min_size(pos2(m_rect.right() + 6.0, deck_top), vec2(deck_w, deck_h));
        if let Some(d) = decks.first() {
            self.deck(ui, a_rect, 0, d, out);
        }
        if let Some(d) = decks.get(1) {
            self.deck(ui, b_rect, 1, d, out);
        }
        self.mixer(ui, m_rect, decks, mixer, out);
        let br = Rect::from_min_max(pos2(full.left(), a_rect.bottom() + 6.0), full.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(br).layout(Layout::top_down(Align::Min)));
        egui::Frame::new().fill(theme::PANEL).corner_radius(4.0).inner_margin(6.0).show(&mut child, |ui| {
            ui.set_min_size(br.size() - vec2(12.0, 12.0));
            browser(ui);
        });
        ui.allocate_rect(full, Sense::hover());
    }

    fn waveforms(&mut self, ui: &mut Ui, rect: Rect, decks: &[DeckUi], out: &mut Vec<LiveAction>) {
        let h = (rect.height() - 6.0) / 2.0;
        for (i, d) in decks.iter().take(2).enumerate() {
            let r = Rect::from_min_size(pos2(rect.left(), rect.top() + i as f32 * (h + 6.0)), vec2(rect.width(), h));
            let resp = ui.allocate_rect(r, Sense::click_and_drag());
            let p = ui.painter_at(r);
            if let Some(wf) = &d.waveform {
                let wp = WaveParams {
                    wf,
                    grid: d.grid,
                    cues: &d.cues,
                    sections: &d.sections,
                    style: self.wave_style,
                    stem_gains: d.stem_gains,
                    loop_region: d.loop_region.filter(|_| d.loop_active),
                    accent: theme::deck_color(i),
                };
                waveform::scrolling(&p, r, &wp, d.position, self.zoom);
            } else {
                p.rect_filled(r, 4.0, theme::BG);
                let msg = if d.loaded { "Analysing…" } else { "Drop a track here" };
                p.text(r.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(13.0), theme::TEXT_DIM);
            }
            // deck letter
            p.text(r.left_top() + vec2(6.0, 4.0), egui::Align2::LEFT_TOP, ["A", "B", "C", "D"][i], egui::FontId::proportional(14.0), theme::deck_color(i));
            if d.has_stems && d.stems_ready < 0.999 {
                let w = 80.0;
                let br = Rect::from_min_size(r.right_top() + vec2(-w - 6.0, 6.0), vec2(w, 5.0));
                p.rect_filled(br, 2.0, theme::PANEL_HI);
                p.rect_filled(Rect::from_min_size(br.min, vec2(w * d.stems_ready, 5.0)), 2.0, theme::OK);
                p.text(br.left_bottom() + vec2(0.0, 2.0), egui::Align2::LEFT_TOP, "stems", egui::FontId::proportional(9.0), theme::TEXT_DIM);
            }
            if resp.hovered() {
                let s = ui.input(|i| i.smooth_scroll_delta.y);
                if s != 0.0 {
                    self.zoom = (self.zoom * (1.0 - s as f64 / 300.0)).clamp(1.5, 60.0);
                }
            }
            if resp.dragged() && d.loaded {
                let dx = resp.drag_delta().x as f64;
                out.push(LiveAction::Scrub(i, -dx / r.width() as f64 * self.zoom));
                self.scrubbing = Some(i);
            }
            if resp.drag_stopped() && self.scrubbing == Some(i) {
                out.push(LiveAction::ScrubEnd(i));
                self.scrubbing = None;
            }
            if let Some(DraggedTracks(ids)) = resp.dnd_release_payload::<DraggedTracks>().map(|p| (*p).clone()) {
                if let Some(id) = ids.first() {
                    out.push(LiveAction::Load(i, *id));
                }
            }
        }
    }

    fn deck(&mut self, ui: &mut Ui, rect: Rect, i: usize, d: &DeckUi, out: &mut Vec<LiveAction>) {
        let accent = theme::deck_color(i);
        let resp = ui.allocate_rect(rect, Sense::hover());
        let hovering_drop = resp.dnd_hover_payload::<DraggedTracks>().is_some();
        if let Some(p) = resp.dnd_release_payload::<DraggedTracks>() {
            if let Some(id) = p.0.first() {
                out.push(LiveAction::Load(i, *id));
            }
        }
        ui.painter().rect_filled(rect, 6.0, theme::PANEL);
        ui.painter().rect_stroke(rect, 6.0, Stroke::new(if hovering_drop { 2.0 } else { 1.0 }, if hovering_drop { accent } else { theme::LINE }), StrokeKind::Inside);
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(8.0)).layout(Layout::top_down(Align::Min)));
        let ui = &mut ui;
        // ---- header
        ui.horizontal(|ui| {
            let (badge, _) = ui.allocate_exact_size(vec2(22.0, 36.0), Sense::hover());
            ui.painter().rect_filled(badge, 4.0, accent);
            ui.painter().text(badge.center(), egui::Align2::CENTER_CENTER, ["A", "B", "C", "D"][i], egui::FontId::proportional(15.0), Color32::BLACK);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width() - 150.0);
                ui.add(egui::Label::new(RichText::new(if d.loaded { &d.title } else { "—" }).strong().size(15.0)).truncate());
                ui.add(egui::Label::new(RichText::new(&d.artist).color(theme::TEXT_DIM)).truncate());
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(k) = d.key {
                    let k2 = k.shifted(d.key_shift);
                    let txt = if d.key_shift != 0 { format!("{} ({:+})", k2.camelot(), d.key_shift) } else { k2.camelot() };
                    ui.label(RichText::new(txt).strong().size(14.0).color(theme::key_color(k2)));
                }
                ui.vertical(|ui| {
                    ui.label(RichText::new(d.bpm.map(|b| format!("{b:.2}")).unwrap_or("--.--".into())).size(22.0).strong().monospace());
                    ui.label(RichText::new(format!("{:+.2}%  ±{}", d.pitch_pct, d.pitch_range)).size(10.5).color(theme::TEXT_DIM));
                });
            });
        });
        // ---- overview + time
        if let Some(wf) = &d.waveform {
            let wp = WaveParams { wf, grid: d.grid, cues: &d.cues, sections: &d.sections, style: self.wave_style, stem_gains: d.stem_gains, loop_region: None, accent };
            if let Some(t) = waveform::overview(ui, vec2(ui.available_width(), 34.0), &wp, d.position, d.duration) {
                out.push(LiveAction::Seek(i, t));
            }
        } else {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
            ui.painter().rect_filled(r, 3.0, theme::BG);
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(theme::fmt_time(d.position)).monospace().size(13.0));
            ui.label(RichText::new(format!("-{}", theme::fmt_time(d.duration - d.position))).monospace().size(13.0).color(theme::TEXT_DIM));
            if let Some(ph) = d.phase_offset {
                let (r, _) = ui.allocate_exact_size(vec2(90.0, 12.0), Sense::hover());
                let p = ui.painter();
                p.rect_filled(r, 2.0, theme::BG);
                p.line_segment([r.center_top(), r.center_bottom()], Stroke::new(1.0, theme::LINE));
                let x = r.center().x + (ph as f32).clamp(-0.5, 0.5) * r.width();
                p.rect_filled(Rect::from_center_size(pos2(x, r.center().y), vec2(6.0, 10.0)), 1.0, if ph.abs() < 0.02 { theme::OK } else { theme::WARN });
            }
            if let Some(s) = d.sections.iter().find(|s| d.position >= s.start && d.position < s.end) {
                ui.label(RichText::new(s.kind.label()).size(11.0).color(theme::rgb(s.kind.color())));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if d.loaded && ui.small_button("⏏").on_hover_text("Eject").clicked() {
                    out.push(LiveAction::Eject(i));
                }
                if widgets::toggle(ui, d.quantize, "Q", theme::ACCENT, vec2(22.0, 18.0)).on_hover_text("Quantize").clicked() {
                    out.push(LiveAction::Quantize(i, !d.quantize));
                }
                if widgets::toggle(ui, d.slip, "SLIP", theme::ACCENT, vec2(36.0, 18.0)).clicked() {
                    out.push(LiveAction::Slip(i, !d.slip));
                }
            });
        });
        ui.add_space(2.0);
        // ---- body: [pitch] transport | pads | (pitch for deck B on the right)
        ui.horizontal_top(|ui| {
            if i % 2 == 0 {
                self.pitch_col(ui, i, d, out);
            }
            self.transport(ui, i, d, out);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width() - if i % 2 == 1 { 64.0 } else { 0.0 });
                self.pads(ui, i, d, out);
                ui.add_space(4.0);
                self.loop_row(ui, i, d, out);
                ui.add_space(4.0);
                self.stems_row(ui, i, d, out);
                ui.add_space(4.0);
                self.fx_row(ui, i, d, out);
            });
            if i % 2 == 1 {
                self.pitch_col(ui, i, d, out);
            }
        });
    }

    fn transport(&mut self, ui: &mut Ui, i: usize, d: &DeckUi, out: &mut Vec<LiveAction>) {
        let accent = theme::deck_color(i);
        ui.vertical(|ui| {
            ui.set_width(64.0);
            let sz = vec2(60.0, 44.0);
            let r = widgets::pad(ui, "CUE", "", theme::WARN, self.cue_held == Some(i) || (!d.playing && (d.position - d.cue_point).abs() < 0.01), sz);
            let (pr, rel) = press_release(ui, &r);
            if pr {
                self.cue_held = Some(i);
                out.push(LiveAction::CueDown(i));
            } else if rel && self.cue_held == Some(i) {
                self.cue_held = None;
                out.push(LiveAction::CueUp(i));
            }
            if widgets::pad(ui, if d.playing { "❚❚" } else { "▶" }, "", theme::OK, d.playing, sz).clicked() {
                out.push(LiveAction::PlayPause(i));
            }
            if widgets::pad(ui, "SYNC", "", accent, d.synced, vec2(60.0, 30.0)).clicked() {
                out.push(LiveAction::Sync(i));
            }
            if widgets::pad(ui, "MASTER", "", accent, d.master, vec2(60.0, 24.0)).clicked() {
                out.push(LiveAction::SetMaster(i));
            }
        });
    }

    fn pitch_col(&mut self, ui: &mut Ui, i: usize, d: &DeckUi, out: &mut Vec<LiveAction>) {
        let accent = theme::deck_color(i);
        ui.vertical(|ui| {
            ui.set_width(58.0);
            // pitch fader: up = slower (CDJ convention: top is -), we use top = +
            let mut v = 0.5 + d.pitch_pct / (2.0 * d.pitch_range.max(1.0));
            if widgets::vfader(ui, &mut v, 0.5, vec2(56.0, 150.0), accent).changed() {
                out.push(LiveAction::Pitch(i, (v - 0.5) * 2.0 * d.pitch_range));
            }
            let ranges = [6.0, 8.0, 10.0, 16.0, 25.0, 50.0, 100.0];
            if ui.add(egui::Button::new(RichText::new(format!("±{}%", d.pitch_range)).size(10.5)).min_size(vec2(56.0, 18.0))).clicked() {
                let next = ranges.iter().copied().find(|r| *r > d.pitch_range + 0.1).unwrap_or(6.0);
                out.push(LiveAction::PitchRange(i, next));
            }
            if widgets::toggle(ui, d.keylock, "KEYLOCK", accent, vec2(56.0, 20.0)).clicked() {
                out.push(LiveAction::Keylock(i, !d.keylock));
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if ui.add(egui::Button::new("-").min_size(vec2(17.0, 18.0))).on_hover_text("Key down").clicked() {
                    out.push(LiveAction::KeyShift(i, d.key_shift - 1));
                }
                if ui.add(egui::Button::new(RichText::new("K").size(10.0)).min_size(vec2(17.0, 18.0))).on_hover_text("Match key to master (click again to reset)").clicked() {
                    out.push(if d.key_shift != 0 { LiveAction::KeyShift(i, 0) } else { LiveAction::MatchKey(i) });
                }
                if ui.add(egui::Button::new("+").min_size(vec2(17.0, 18.0))).on_hover_text("Key up").clicked() {
                    out.push(LiveAction::KeyShift(i, d.key_shift + 1));
                }
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (lbl, s) in [("◀", -1.0f32), ("▶", 1.0)] {
                    let r = ui.add(egui::Button::new(lbl).min_size(vec2(27.0, 18.0)).sense(Sense::click_and_drag())).on_hover_text("Pitch bend");
                    let (p, rel) = press_release(ui, &r);
                    if p {
                        out.push(LiveAction::PitchBend(i, s * 4.0));
                    }
                    if rel && r.contains_pointer() || r.drag_stopped() {
                        out.push(LiveAction::PitchBend(i, 0.0));
                    }
                }
            });
        });
    }

    fn pads(&mut self, ui: &mut Ui, i: usize, d: &DeckUi, out: &mut Vec<LiveAction>) {
        let accent = theme::deck_color(i);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            for m in PadMode::ALL {
                if ui.add(egui::Button::new(RichText::new(m.label()).size(11.0)).selected(self.pad_mode[i] == m)).clicked() {
                    self.pad_mode[i] = m;
                }
            }
        });
        let w = ((ui.available_width() - 3.0 * 4.0) / 4.0).max(30.0);
        let size = vec2(w, 40.0);
        let mode = self.pad_mode[i];
        for row in 0..2 {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for col in 0..4 {
                    let n = (row * 4 + col) as u8;
                    let (label, sub, color, lit) = match mode {
                        PadMode::HotCues => {
                            let c = d.cues.iter().find(|c| c.slot == Some(n));
                            match c {
                                Some(c) => (format!("{}", n + 1), c.name.clone(), theme::rgb(c.color), true),
                                None => (format!("{}", n + 1), "set".into(), accent, false),
                            }
                        }
                        PadMode::Stems => {
                            if n < 4 {
                                let k = StemKind::ALL[3 - n as usize];
                                let g = d.stem_gains[k.index()];
                                (k.label().to_string(), if g > 0.01 { "on".into() } else { "muted".into() }, theme::rgb(k.color()), g > 0.01)
                            } else {
                                let (l, p) = [("Acapella", StemPreset::Acapella), ("Instru", StemPreset::Instrumental), ("Drums", StemPreset::DrumsOnly), ("All", StemPreset::All)][n as usize - 4];
                                let on = d.stem_gains == p.gains();
                                (l.to_string(), String::new(), theme::ACCENT, on)
                            }
                        }
                        PadMode::Loops => {
                            let b = LOOP_SIZES[n as usize];
                            (beats_label(b), "beats".into(), theme::WARN, d.loop_active && (d.auto_loop_beats - b).abs() < 1e-6)
                        }
                        PadMode::Roll => {
                            let b = ROLL_SIZES[n as usize];
                            (beats_label(b), "roll".into(), Color32::from_rgb(120, 200, 255), self.held == Some((i, n, PadMode::Roll)))
                        }
                        PadMode::BeatJump => {
                            let b = JUMPS[n as usize];
                            (format!("{:+}", b as i64), "beats".into(), Color32::from_rgb(180, 140, 255), false)
                        }
                    };
                    let enabled = d.loaded && (mode != PadMode::Stems || d.has_stems);
                    let r = ui.add_enabled_ui(enabled, |ui| widgets::pad(ui, &label, &sub, color, lit && enabled, size)).inner;
                    let (pressed, released) = press_release(ui, &r);
                    if !enabled {
                        continue;
                    }
                    if pressed {
                        let shift = ui.input(|i| i.modifiers.shift);
                        match mode {
                            PadMode::HotCues => {
                                if shift {
                                    out.push(LiveAction::DeleteHotCue(i, n));
                                } else {
                                    out.push(LiveAction::HotCue(i, n));
                                    self.held = Some((i, n, mode));
                                }
                            }
                            PadMode::Stems => {
                                if n < 4 {
                                    let k = StemKind::ALL[3 - n as usize];
                                    if shift {
                                        out.push(LiveAction::StemSolo(i, k.index()));
                                    } else {
                                        let g = d.stem_gains[k.index()];
                                        out.push(LiveAction::StemGain(i, k.index(), if g > 0.01 { 0.0 } else { 1.0 }));
                                    }
                                } else {
                                    let p = [StemPreset::Acapella, StemPreset::Instrumental, StemPreset::DrumsOnly, StemPreset::All][n as usize - 4];
                                    out.push(LiveAction::StemPreset(i, if d.stem_gains == p.gains() { StemPreset::All } else { p }));
                                }
                            }
                            PadMode::Loops => out.push(LiveAction::AutoLoop(i, LOOP_SIZES[n as usize])),
                            PadMode::Roll => {
                                out.push(LiveAction::LoopRoll(i, ROLL_SIZES[n as usize], true));
                                self.held = Some((i, n, mode));
                            }
                            PadMode::BeatJump => out.push(LiveAction::BeatJump(i, JUMPS[n as usize])),
                        }
                    } else if released && self.held == Some((i, n, mode)) {
                        self.held = None;
                        match mode {
                            PadMode::Roll => out.push(LiveAction::LoopRoll(i, ROLL_SIZES[n as usize], false)),
                            PadMode::HotCues => out.push(LiveAction::HotCueRelease(i, n)),
                            _ => {}
                        }
                    }
                    if mode == PadMode::HotCues {
                        r.on_hover_text("Click: jump / set  ·  Shift+click: delete");
                    }
                }
            });
        }
        let _ = HOT_CUES;
    }

    fn loop_row(&mut self, ui: &mut Ui, i: usize, d: &DeckUi, out: &mut Vec<LiveAction>) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let b = vec2(30.0, 20.0);
            if ui.add(egui::Button::new("IN").min_size(b)).clicked() {
                out.push(LiveAction::LoopIn(i));
            }
            if ui.add(egui::Button::new("OUT").min_size(b)).clicked() {
                out.push(LiveAction::LoopOut(i));
            }
            if ui.add(egui::Button::new("½").min_size(b)).clicked() {
                out.push(LiveAction::LoopHalve(i));
            }
            let lbl = format!("LOOP {}", beats_label(d.auto_loop_beats));
            if widgets::toggle(ui, d.loop_active, &lbl, theme::WARN, vec2(70.0, 20.0)).clicked() {
                out.push(if d.loop_active || d.loop_region.is_some() { LiveAction::LoopToggle(i) } else { LiveAction::AutoLoop(i, d.auto_loop_beats) });
            }
            if ui.add(egui::Button::new("×2").min_size(b)).clicked() {
                out.push(LiveAction::LoopDouble(i));
            }
            ui.separator();
            for (lbl, beats) in [("◀◀", -4.0), ("▶▶", 4.0)] {
                if ui.add(egui::Button::new(lbl).min_size(b)).on_hover_text("Jump 1 bar").clicked() {
                    out.push(LiveAction::BeatJump(i, beats));
                }
            }
            // quick jump to auto cues
            for k in [CueKind::Drop, CueKind::MixOut] {
                if let Some(c) = d.cues.iter().find(|c| c.kind == k) {
                    if ui.add(egui::Button::new(RichText::new(k.label()).size(10.5).color(theme::rgb(c.color))).min_size(vec2(44.0, 20.0))).clicked() {
                        out.push(LiveAction::Seek(i, c.pos));
                    }
                }
            }
        });
    }

    fn stems_row(&mut self, ui: &mut Ui, i: usize, d: &DeckUi, out: &mut Vec<LiveAction>) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.add_enabled_ui(d.has_stems, |ui| {
                for k in [StemKind::Vocals, StemKind::Other, StemKind::Bass, StemKind::Drums] {
                    let mut g = d.stem_gains[k.index()];
                    if widgets::knob(ui, &mut g, 0.0..=1.0, 1.0, k.label(), theme::rgb(k.color()), 34.0).changed() {
                        out.push(LiveAction::StemGain(i, k.index(), g));
                    }
                }
            });
            if !d.has_stems {
                ui.label(RichText::new("no stems").size(10.5).color(theme::TEXT_DIM));
            }
        });
    }

    fn fx_row(&mut self, ui: &mut Ui, i: usize, d: &DeckUi, out: &mut Vec<LiveAction>) {
        let accent = theme::deck_color(i);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for slot in 0..2 {
                let mut fx = d.fx[slot];
                let before = fx;
                egui::ComboBox::from_id_salt(("fx", i, slot)).width(76.0).selected_text(RichText::new(FX_NAMES[fx.kind]).size(11.0)).show_ui(ui, |ui| {
                    for (k, n) in FX_NAMES.iter().enumerate() {
                        ui.selectable_value(&mut fx.kind, k, *n);
                    }
                });
                if widgets::toggle(ui, fx.on, "ON", accent, vec2(28.0, 20.0)).clicked() {
                    fx.on = !fx.on;
                }
                widgets::knob(ui, &mut fx.amount, 0.0..=1.0, 0.5, "amt", accent, 26.0);
                if fx != before {
                    out.push(LiveAction::Fx(i, slot, fx));
                }
            }
        });
    }

    fn mixer(&mut self, ui: &mut Ui, rect: Rect, decks: &[DeckUi], m: &MixerUi, out: &mut Vec<LiveAction>) {
        ui.painter().rect_filled(rect, 6.0, theme::PANEL);
        ui.painter().rect_stroke(rect, 6.0, Stroke::new(1.0, theme::LINE), StrokeKind::Inside);
        let inner = rect.shrink(8.0);
        let ch_w = 72.0;
        let body_h = inner.height() - 50.0;
        let cols = [
            Rect::from_min_size(inner.min, vec2(ch_w, body_h)),
            Rect::from_min_size(pos2(inner.left() + ch_w, inner.top()), vec2(inner.width() - 2.0 * ch_w, body_h)),
            Rect::from_min_size(pos2(inner.right() - ch_w, inner.top()), vec2(ch_w, body_h)),
        ];
        for (i, d) in decks.iter().take(2).enumerate() {
            let r = if i == 0 { cols[0] } else { cols[2] };
            let mut cu = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(Layout::top_down(Align::Center)));
            self.channel(&mut cu, i, d, out);
        }
        let mut mu = ui.new_child(egui::UiBuilder::new().max_rect(cols[1]).layout(Layout::top_down(Align::Center)));
        self.master_col(&mut mu, m, out);
        let xr = Rect::from_min_max(pos2(inner.left() + 14.0, inner.bottom() - 48.0), pos2(inner.right() - 14.0, inner.bottom() - 20.0));
        let mut xu = ui.new_child(egui::UiBuilder::new().max_rect(xr).layout(Layout::left_to_right(Align::Center)));
        let mut x = m.crossfader;
        if widgets::hfader(&mut xu, &mut x, true, xr.size(), theme::TEXT).changed() {
            out.push(LiveAction::Crossfader(x));
        }
        let p = ui.painter();
        p.text(pos2(xr.left(), inner.bottom() - 8.0), egui::Align2::LEFT_CENTER, "A", egui::FontId::proportional(12.0), theme::deck_color(0));
        p.text(pos2(xr.right(), inner.bottom() - 8.0), egui::Align2::RIGHT_CENTER, "B", egui::FontId::proportional(12.0), theme::deck_color(1));
        if let Some(b) = m.master_bpm {
            p.text(pos2(xr.center().x, inner.bottom() - 8.0), egui::Align2::CENTER_CENTER, format!("{b:.1} BPM"), egui::FontId::proportional(11.0), theme::TEXT_DIM);
        }
    }

    fn channel(&mut self, ui: &mut Ui, i: usize, d: &DeckUi, out: &mut Vec<LiveAction>) {
        let accent = theme::deck_color(i);
        {
            ui.spacing_mut().item_spacing.y = 1.0;
            let mut g = d.gain;
            if widgets::knob(ui, &mut g, 0.0..=1.0, 0.5, "GAIN", theme::TEXT, 31.0).changed() {
                out.push(LiveAction::Gain(i, g));
            }
            for (b, name) in [(2usize, "HIGH"), (1, "MID"), (0, "LOW")] {
                let mut v = d.eq[b];
                let col = if v < 0.02 { Color32::from_rgb(255, 70, 70) } else { accent };
                if widgets::knob(ui, &mut v, 0.0..=1.0, 0.5, name, col, 31.0).changed() {
                    out.push(LiveAction::Eq(i, b, v));
                }
            }
            let mut f = d.filter;
            if widgets::knob(ui, &mut f, 0.0..=1.0, 0.5, "FILTER", Color32::from_rgb(120, 220, 255), 31.0).changed() {
                out.push(LiveAction::Filter(i, f));
            }
            if widgets::toggle(ui, d.pfl, "🎧", accent, vec2(40.0, 20.0)).on_hover_text("Headphone cue").clicked() {
                out.push(LiveAction::Pfl(i, !d.pfl));
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                widgets::vu(ui, d.vu[0], d.vu[1], vec2(12.0, 84.0));
                let mut v = d.fader;
                if widgets::vfader(ui, &mut v, 1.0, vec2(34.0, 88.0), accent).changed() {
                    out.push(LiveAction::Fader(i, v));
                }
            });
        }
    }

    fn master_col(&mut self, ui: &mut Ui, m: &MixerUi, out: &mut Vec<LiveAction>) {
        {
            ui.label(RichText::new("MASTER").size(10.5).color(theme::TEXT_DIM));
            widgets::vu(ui, m.master_vu[0], m.master_vu[1], vec2(22.0, 110.0));
            let mut v = m.master;
            if widgets::knob(ui, &mut v, 0.0..=1.0, 0.8, "MASTER", theme::ACCENT, 38.0).changed() {
                out.push(LiveAction::MasterVol(v));
            }
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 72.0).max(0.0) / 2.0);
                let mut hm = m.headphone_mix;
                if widgets::knob(ui, &mut hm, 0.0..=1.0, 0.0, "CUE·MIX", theme::TEXT, 32.0).changed() {
                    out.push(LiveAction::HeadphoneMix(hm));
                }
                let mut hv = m.headphone_vol;
                if widgets::knob(ui, &mut hv, 0.0..=1.0, 0.7, "PHONES", theme::TEXT, 32.0).changed() {
                    out.push(LiveAction::HeadphoneVol(hv));
                }
            });
            let rec = if m.recording { format!("● REC {}", theme::fmt_time_short(m.record_secs)) } else { "● REC".to_string() };
            if widgets::toggle(ui, m.recording, &rec, Color32::from_rgb(255, 60, 60), vec2(100.0, 22.0)).clicked() {
                out.push(LiveAction::Record(!m.recording));
            }
            if widgets::toggle(ui, m.automix, "AUTOMIX", theme::OK, vec2(100.0, 22.0)).on_hover_text("Mix the queue automatically using the analysed mix points").clicked() {
                out.push(LiveAction::Automix(!m.automix));
            }
            ui.label(RichText::new(&m.output_name).size(9.5).color(theme::TEXT_DIM));
        }
    }
}
