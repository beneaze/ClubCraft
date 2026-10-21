//! The studio's "Add tracks" dialog: search, sort and filter the library, listen to a track from
//! its drop before adding it, and add one track (double-click) or a selection to the mix.

use crate::app::Shared;
use crate::theme;
use clubcraft_core::{Analysis, SectionKind, Track, TrackId};
use eframe::egui::{self, Color32, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};

/// Tempo difference (%) a track may have from the mix's last track and still "fit".
const FIT_TEMPO_PCT: f64 = 6.0;
/// Key compatibility (see [`clubcraft_core::Key::compatibility`]) a track needs to "fit".
const FIT_KEY: f32 = 0.8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sort {
    Title,
    Artist,
    Bpm,
    Key,
    Energy,
    Time,
    Fit,
}

pub enum PickerAction {
    /// Play this track from `from` seconds.
    Preview(TrackId, f64),
    StopPreview,
    Add(Vec<TrackId>),
}

pub struct TrackPicker {
    pub open: bool,
    query: String,
    fits_only: bool,
    sort: Sort,
    ascending: bool,
    /// In click order, which is the order they join the mix.
    selected: Vec<TrackId>,
    previewing: Option<TrackId>,
    focus_search: bool,
}

impl Default for TrackPicker {
    fn default() -> Self {
        TrackPicker { open: false, query: String::new(), fits_only: false, sort: Sort::Fit, ascending: false, selected: Vec::new(), previewing: None, focus_search: false }
    }
}

/// Where a preview starts: the first drop, else a third into the track.
pub fn preview_start(a: Option<&Analysis>, duration: f64) -> f64 {
    a.and_then(|a| a.sections.iter().find(|s| s.kind == SectionKind::Drop).map(|s| s.start)).unwrap_or(duration / 3.0).max(0.0)
}

fn duration_of(t: &Track, a: Option<&Analysis>) -> f64 {
    a.map(|a| a.duration).filter(|d| *d > 0.0).unwrap_or(t.duration)
}

/// How well `a` follows `last` in a mix: key compatibility (0..1) and tempo difference (%).
fn fit(last: Option<&Analysis>, a: Option<&Analysis>) -> Option<(f32, f64)> {
    let (l, a) = (last?, a?);
    let key = l.key.zip(a.key).map(|(x, y)| x.compatibility(y)).unwrap_or(0.0);
    let (lb, ab) = (l.bpm()?, a.bpm()?);
    Some((key, (ab / lb - 1.0) * 100.0))
}

fn fits(f: Option<(f32, f64)>) -> bool {
    f.is_some_and(|(k, t)| k >= FIT_KEY && t.abs() <= FIT_TEMPO_PCT)
}

/// One score for sorting by fit: key first, tempo as the tie-breaker.
fn fit_score(f: Option<(f32, f64)>) -> f64 {
    f.map(|(k, t)| k as f64 * 10.0 - t.abs().min(20.0) * 0.1).unwrap_or(-100.0)
}

impl TrackPicker {
    pub fn show(&mut self) {
        self.open = true;
        self.focus_search = true;
        self.selected.clear();
    }

    /// Draw the dialog. `in_mix`: tracks already in the mix (left out); `last`: the mix's last
    /// track, which "fits" is measured against; `preview_playing` / `preview_pos`: the running
    /// preview, if any.
    pub fn ui(&mut self, ctx: &egui::Context, sh: &Shared, in_mix: &[TrackId], last: Option<TrackId>, preview_playing: bool, preview_pos: f64, out: &mut Vec<PickerAction>) {
        if !self.open {
            return;
        }
        if !preview_playing {
            self.previewing = None;
        }
        let last_a = last.and_then(|id| sh.analyses.get(&id));
        let q = self.query.to_lowercase();
        let mut rows: Vec<(&Track, Option<&Analysis>, Option<(f32, f64)>)> = sh
            .tracks
            .iter()
            .filter(|t| !in_mix.contains(&t.id) && (q.is_empty() || crate::library_view::matches(t, &q)))
            .map(|t| {
                let a = sh.analyses.get(&t.id);
                (t, a, fit(last_a, a))
            })
            .filter(|(_, _, f)| !self.fits_only || fits(*f))
            .collect();
        let bpm = |a: Option<&Analysis>| a.and_then(|a| a.bpm()).unwrap_or(0.0);
        match self.sort {
            Sort::Title => rows.sort_by_key(|(t, _, _)| t.display_name().to_lowercase()),
            Sort::Artist => rows.sort_by_key(|(t, _, _)| t.artist.to_lowercase()),
            Sort::Bpm => rows.sort_by(|x, y| bpm(x.1).total_cmp(&bpm(y.1))),
            Sort::Key => rows.sort_by_key(|(_, a, _)| a.and_then(|a| a.key).map(|k| (k.camelot_number(), !k.minor)).unwrap_or((99, false))),
            Sort::Energy => rows.sort_by_key(|(_, a, _)| a.and_then(|a| a.energy).unwrap_or(0)),
            Sort::Time => rows.sort_by(|x, y| duration_of(x.0, x.1).total_cmp(&duration_of(y.0, y.1))),
            Sort::Fit => rows.sort_by(|x, y| fit_score(x.2).total_cmp(&fit_score(y.2))),
        }
        if !self.ascending {
            rows.reverse();
        }

        let frame = egui::Frame::new()
            .fill(theme::PANEL)
            .stroke(egui::Stroke::new(1.0, theme::LINE))
            .corner_radius(12.0)
            .inner_margin(egui::Margin::same(18))
            .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(120) });
        let mut close = false;
        let mut add: Option<Vec<TrackId>> = None;
        let resp = egui::Modal::new(egui::Id::new("track-picker")).frame(frame).backdrop_color(Color32::from_black_alpha(120)).show(ctx, |ui| {
            ui.set_width(1000.0);
            ui.set_height(640.0);
            // header
            ui.horizontal(|ui| {
                ui.label(RichText::new("Add tracks").size(20.0).strong());
                if let Some(t) = last.and_then(|id| sh.track(id)) {
                    ui.label(RichText::new(format!("after {}", t.display_name())).size(12.0).color(theme::TEXT_DIM));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = crate::settings::close_button(ui).on_hover_text("Close (Esc)").clicked();
                });
            });
            ui.add_space(10.0);
            // search and filter
            ui.horizontal(|ui| {
                let search = ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("🔍  Search title, artist, genre…").desired_width(420.0).margin(egui::vec2(8.0, 6.0)));
                if std::mem::take(&mut self.focus_search) {
                    search.request_focus();
                }
                ui.add_space(8.0);
                if last_a.is_some() {
                    let label = format!("Fits the mix ({FIT_TEMPO_PCT:.0} % tempo, harmonic key)");
                    let chip = egui::Button::new(RichText::new(label).size(12.5).color(if self.fits_only { Color32::from_gray(16) } else { theme::TEXT }))
                        .fill(if self.fits_only { theme::OK } else { theme::SEG_TRACK })
                        .stroke(egui::Stroke::new(1.0, if self.fits_only { theme::OK } else { theme::LINE }))
                        .corner_radius(14.0)
                        .min_size(egui::vec2(0.0, 28.0));
                    if ui.add(chip).on_hover_text("Only tracks whose key and tempo mix well with the last track").clicked() {
                        self.fits_only = !self.fits_only;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("{} tracks", rows.len())).size(12.0).color(theme::TEXT_DIM));
                });
            });
            ui.add_space(10.0);
            self.preview_strip(ui, sh, preview_pos, out);
            ui.add_space(10.0);
            // the track table
            let footer_h = 44.0;
            let mut sort_click: Option<Sort> = None;
            let header = |ui: &mut Ui, label: &str, s: Sort, this: &Self, click: &mut Option<Sort>| {
                let arrow = if this.sort == s { if this.ascending { " ⏶" } else { " ⏷" } } else { "" };
                let col = if this.sort == s { theme::TEXT } else { theme::TEXT_DIM };
                if ui.add(egui::Label::new(RichText::new(format!("{label}{arrow}")).size(11.5).strong().color(col)).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    *click = Some(s);
                }
            };
            let avail = ui.available_height() - footer_h;
            TableBuilder::new(ui)
                .striped(true)
                .sense(Sense::click())
                .min_scrolled_height(avail)
                .max_scroll_height(avail)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(34.0))
                .column(Column::exact(26.0))
                .column(Column::remainder().at_least(220.0).clip(true))
                .column(Column::exact(190.0).clip(true))
                .column(Column::exact(64.0))
                .column(Column::exact(54.0))
                .column(Column::exact(74.0))
                .column(Column::exact(44.0))
                .column(Column::exact(56.0))
                .header(26.0, |mut h| {
                    h.col(|_| {});
                    h.col(|_| {});
                    h.col(|ui| header(ui, "TITLE", Sort::Title, self, &mut sort_click));
                    h.col(|ui| header(ui, "ARTIST", Sort::Artist, self, &mut sort_click));
                    h.col(|ui| header(ui, "BPM", Sort::Bpm, self, &mut sort_click));
                    h.col(|ui| header(ui, "KEY", Sort::Key, self, &mut sort_click));
                    h.col(|ui| header(ui, "FIT", Sort::Fit, self, &mut sort_click));
                    h.col(|ui| header(ui, "NRG", Sort::Energy, self, &mut sort_click));
                    h.col(|ui| header(ui, "TIME", Sort::Time, self, &mut sort_click));
                })
                .body(|body| {
                    body.rows(34.0, rows.len(), |mut row| {
                        let Some(&(t, a, f)) = rows.get(row.index()) else { return };
                        let sel = self.selected.contains(&t.id);
                        let playing = self.previewing == Some(t.id);
                        row.set_selected(sel || playing);
                        let mut preview_click = false;
                        row.col(|ui| {
                            preview_click = crate::widgets::play_button(ui, playing, a.is_some(), 24.0).on_hover_text(if playing { "Stop" } else { "Listen from the drop" }).clicked();
                        });
                        row.col(|ui| {
                            if tick_box(ui, sel).clicked() {
                                self.toggle(t.id);
                            }
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(t.display_name()).size(13.5).color(theme::TEXT));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&t.artist).size(12.5).color(theme::TEXT_DIM));
                        });
                        row.col(|ui| {
                            ui.label(a.and_then(|a| a.bpm()).map(|b| format!("{b:.1}")).unwrap_or_else(|| "–".into()));
                        });
                        row.col(|ui| match a.and_then(|a| a.key) {
                            Some(k) => {
                                ui.label(RichText::new(theme::key_label(k)).color(theme::key_color(k)).strong());
                            }
                            None => {
                                ui.label(RichText::new("–").color(theme::TEXT_DIM));
                            }
                        });
                        row.col(|ui| fit_badge(ui, f));
                        row.col(|ui| {
                            ui.label(a.and_then(|a| a.energy).map(|e| e.to_string()).unwrap_or_default());
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(theme::fmt_time_short(duration_of(t, a))).color(theme::TEXT_DIM));
                        });
                        let r = row.response();
                        if preview_click {
                            if playing {
                                self.previewing = None;
                                out.push(PickerAction::StopPreview);
                            } else {
                                self.previewing = Some(t.id);
                                out.push(PickerAction::Preview(t.id, preview_start(a, duration_of(t, a))));
                            }
                        } else if r.double_clicked() {
                            add = Some(vec![t.id]);
                        } else if r.clicked() {
                            self.toggle(t.id);
                        }
                    });
                });
            if let Some(s) = sort_click {
                if self.sort == s {
                    self.ascending = !self.ascending;
                } else {
                    self.sort = s;
                    // text sorts start at A, numbers and fit start at the top
                    self.ascending = matches!(s, Sort::Title | Sort::Artist | Sort::Key);
                }
            }
            // footer
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let n = self.selected.len();
                ui.label(RichText::new(if n == 0 { "Click tracks to select them, double-click to add one".to_string() } else { format!("{n} selected") }).size(12.0).color(theme::TEXT_DIM));
                if n > 0 && ui.small_button("Clear").clicked() {
                    self.selected.clear();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = match n {
                        0 => "Add".to_string(),
                        1 => "Add 1 track".to_string(),
                        n => format!("Add {n} tracks"),
                    };
                    let btn = egui::Button::new(RichText::new(label).size(14.0).strong().color(if n > 0 { Color32::from_gray(16) } else { theme::TEXT_DIM }))
                        .fill(if n > 0 { theme::accent() } else { theme::SEG_TRACK })
                        .corner_radius(8.0)
                        .min_size(egui::vec2(130.0, 32.0));
                    if ui.add_enabled(n > 0, btn).clicked() || (n > 0 && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command)) {
                        add = Some(self.selected.clone());
                    }
                    if ui.add(egui::Button::new(RichText::new("Cancel").size(14.0)).corner_radius(8.0).min_size(egui::vec2(90.0, 32.0))).clicked() {
                        close = true;
                    }
                });
            });
        });
        if let Some(ids) = add {
            out.push(PickerAction::Add(ids));
            close = true;
        }
        if close || resp.should_close() {
            self.open = false;
            if self.previewing.take().is_some() {
                out.push(PickerAction::StopPreview);
            }
        }
    }

    /// Open with a search already typed (developer control).
    pub fn show_with(&mut self, query: &str) {
        self.show();
        self.query = query.to_string();
    }

    /// Mark `id` as the track being previewed (developer control starts the preview itself).
    pub fn set_previewing(&mut self, id: TrackId) {
        self.previewing = Some(id);
    }

    fn toggle(&mut self, id: TrackId) {
        if let Some(i) = self.selected.iter().position(|x| *x == id) {
            self.selected.remove(i);
        } else {
            self.selected.push(id);
        }
    }

    /// The track being previewed: play/stop, its name and a clickable overview to jump around.
    fn preview_strip(&mut self, ui: &mut Ui, sh: &Shared, pos: f64, out: &mut Vec<PickerAction>) {
        let frame = egui::Frame::new().fill(theme::BG).corner_radius(8.0).inner_margin(egui::Margin::symmetric(12, 10));
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(52.0);
            let Some(id) = self.previewing else {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("▶  Press play on a track to listen from its drop. Click the waveform to jump around.").size(12.5).color(theme::TEXT_DIM));
                });
                return;
            };
            let (t, a) = (sh.track(id), sh.analyses.get(&id));
            ui.horizontal_centered(|ui| {
                if crate::widgets::play_button(ui, true, true, 34.0).on_hover_text("Stop").clicked() {
                    self.previewing = None;
                    out.push(PickerAction::StopPreview);
                }
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.set_width(230.0);
                    ui.add(egui::Label::new(RichText::new(t.map(|t| t.display_name()).unwrap_or_default()).size(13.5).strong()).truncate());
                    let info = a.map(|a| format!("{:.1} BPM · {} · {}", a.bpm().unwrap_or(0.0), a.key.map(theme::key_label).unwrap_or_default(), theme::fmt_time_short(pos))).unwrap_or_default();
                    ui.label(RichText::new(info).size(11.5).color(theme::TEXT_DIM));
                });
                ui.add_space(8.0);
                let size = egui::vec2(ui.available_width(), 46.0);
                match (sh.waveforms.get(&id), a) {
                    (Some(wf), Some(a)) => {
                        let wp = crate::waveform::WaveParams {
                            wf,
                            grid: a.grid,
                            cues: &a.cues,
                            sections: &a.sections,
                            style: crate::waveform::WaveStyle::Bands,
                            stem_gains: [1.0; 4],
                            loop_region: None,
                            accent: theme::accent(),
                            volume: 1.0,
                        };
                        if let Some(at) = crate::waveform::overview(ui, size, &wp, pos, a.duration) {
                            out.push(PickerAction::Preview(id, at));
                        }
                    }
                    _ => {
                        ui.allocate_exact_size(size, Sense::hover());
                    }
                }
            });
        });
    }
}

/// Green when the track mixes well after the last one, amber when it is close, dim otherwise.
fn fit_badge(ui: &mut Ui, f: Option<(f32, f64)>) {
    let Some((k, t)) = f else {
        ui.label(RichText::new("–").color(theme::TEXT_DIM));
        return;
    };
    let close = k >= 0.5 && t.abs() <= FIT_TEMPO_PCT * 1.5;
    let (col, txt) = if fits(f) {
        (theme::OK, "fits")
    } else if close {
        (theme::WARN, "close")
    } else {
        (theme::TEXT_DIM, "")
    };
    // a painted dot: the default fonts have no filled circle glyph
    let (r, resp) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
    if txt.is_empty() {
        ui.painter().circle_stroke(r.center(), 4.0, egui::Stroke::new(1.2, col));
    } else {
        ui.painter().circle_filled(r.center(), 4.0, col);
        ui.label(RichText::new(txt).size(12.0).color(col));
    }
    resp.on_hover_text(format!("Key match {:.0} %, tempo {t:+.1} %", k * 100.0));
}

/// Selection box that stays visible on striped rows: outlined square, accent-filled with a tick.
fn tick_box(ui: &mut Ui, on: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), Sense::click());
    let p = ui.painter();
    let b = egui::Rect::from_center_size(r.center(), egui::vec2(16.0, 16.0));
    if on {
        p.rect_filled(b, 4.0, theme::accent());
        let c = b.center();
        let ink = egui::Stroke::new(2.0, Color32::from_gray(16));
        p.line_segment([c + egui::vec2(-4.0, 0.0), c + egui::vec2(-1.0, 3.5)], ink);
        p.line_segment([c + egui::vec2(-1.0, 3.5), c + egui::vec2(4.5, -3.5)], ink);
    } else {
        let line = if resp.hovered() { theme::TEXT_DIM } else { theme::SEG_ON_LINE };
        p.rect_stroke(b, 4.0, egui::Stroke::new(1.3, line), egui::StrokeKind::Inside);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}
