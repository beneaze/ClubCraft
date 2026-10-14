//! The mashup finder window: pick a vocal track, search the library for instrumentals that carry
//! it, preview a match and turn it into a mix.

use crate::app::Shared;
use crate::mashup::{self, Query};
use crate::theme;
use clubcraft_analysis::mashup::{Match, MatchOptions};
use clubcraft_core::{Mix, TrackId};
use crossbeam_channel::Receiver;
use eframe::egui::{self, Color32, RichText, Sense, Ui};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    /// Results become new mixes (or are inserted after the vocal track).
    Library,
    /// Results set transition `index` of the open mix.
    Transition { mix_id: i64, index: usize },
}

pub enum MashupAction {
    Preview(Match),
    StopPreview,
    NewMix(Match),
    Insert(Match),
    Apply(Match, usize),
}

struct Job {
    rx: Receiver<Result<Vec<Match>, String>>,
    progress: Arc<Mutex<(f32, String)>>,
    cancel: Arc<AtomicBool>,
}

pub struct MashupView {
    pub open: bool,
    pub vocal: Option<TrackId>,
    pick: String,
    auto_phrase: bool,
    phrase_bar: i32,
    bars: usize,
    key_range: i32,
    tempo_pct: u32,
    scope_mix: bool,
    rerank: bool,
    target: Target,
    /// Only this instrumental (transition mode) and bar ranges.
    inst_only: Option<TrackId>,
    vocal_range: Option<(i32, i32)>,
    inst_range: Option<(i32, i32)>,
    job: Option<Job>,
    results: Vec<Match>,
    message: String,
    pub previewing: Option<usize>,
    run_next_frame: bool,
}

impl Default for MashupView {
    fn default() -> Self {
        MashupView {
            open: false,
            vocal: None,
            pick: String::new(),
            auto_phrase: true,
            phrase_bar: 0,
            bars: 16,
            key_range: 2,
            tempo_pct: 6,
            scope_mix: false,
            rerank: true,
            target: Target::Library,
            inst_only: None,
            vocal_range: None,
            inst_range: None,
            job: None,
            results: Vec::new(),
            message: String::new(),
            previewing: None,
            run_next_frame: false,
        }
    }
}

fn track_label(sh: &Shared, id: TrackId) -> String {
    let t = sh.track(id);
    let a = sh.analyses.get(&id);
    let mut s = t.map(|t| t.display_name()).unwrap_or_else(|| format!("#{id}"));
    if let Some(t) = t.filter(|t| !t.artist.is_empty() && !t.title.is_empty()) {
        s = format!("{} – {}", t.artist, t.title);
    }
    if let Some(a) = a {
        if let Some(b) = a.bpm() {
            s += &format!(" · {b:.0}");
        }
        if let Some(k) = a.key {
            s += &format!(" · {}", theme::key_label(k));
        }
    }
    s
}

impl MashupView {
    /// Open for the vocals of `vocal`, searching the whole library.
    pub fn open_for(&mut self, vocal: Option<TrackId>, sh: &Shared) {
        self.open = true;
        self.target = Target::Library;
        self.inst_only = None;
        self.vocal_range = None;
        self.inst_range = None;
        self.auto_phrase = true;
        self.results.clear();
        self.message.clear();
        self.vocal = vocal.filter(|id| sh.analyses.get(id).is_some_and(|a| a.stems_model.is_some()));
        if vocal.is_some() && self.vocal.is_none() {
            self.message = "That track has no stems yet; separate it first (Library › Analyse + separate stems).".into();
        }
        self.run_next_frame = self.vocal.is_some();
    }

    /// Find where the vocals of track `k` fit best over track `k + 1` (transition `k`).
    pub fn open_for_transition(&mut self, mix: &Mix, k: usize, sh: &Shared) {
        let (Some(a), Some(b)) = (mix.tracks.get(k), mix.tracks.get(k + 1)) else { return };
        self.open_for(Some(a.track_id), sh);
        if self.vocal.is_none() {
            return;
        }
        if !sh.analyses.get(&b.track_id).is_some_and(|x| x.stems_model.is_some()) {
            self.message = "The next track has no stems yet; separate it first.".into();
            self.run_next_frame = false;
            return;
        }
        let bars_of = |id: TrackId| sh.analyses.get(&id).and_then(|x| x.grid.map(|g| g.bar_at(x.duration) as i32)).unwrap_or(0);
        let (na, nb) = (bars_of(a.track_id), bars_of(b.track_id));
        self.target = Target::Transition { mix_id: mix.id, index: k };
        self.inst_only = Some(b.track_id);
        // vocals from the second half of the outgoing track, instrumental from the first 60 %
        self.vocal_range = Some((na * 45 / 100, na));
        self.inst_range = Some((0, nb * 60 / 100));
    }

    pub fn transition_index(&self) -> Option<usize> {
        match self.target {
            Target::Transition { index, .. } => Some(index),
            Target::Library => None,
        }
    }

    pub fn result(&self, i: usize) -> Option<Match> {
        self.results.get(i).cloned()
    }

    fn start(&mut self, mix: &Mix) {
        let Some(vocal) = self.vocal else { return };
        if let Some(j) = self.job.take() {
            j.cancel.store(true, Ordering::Relaxed);
        }
        let candidates = match (self.inst_only, self.scope_mix) {
            (Some(i), _) => Some(vec![i]),
            (None, true) => Some(mix.tracks.iter().map(|t| t.track_id).collect()),
            (None, false) => None,
        };
        let transition = self.inst_only.is_some();
        let opts = MatchOptions {
            bars: self.bars,
            key_range: self.key_range,
            tempo_tolerance: self.tempo_pct as f64 / 100.0,
            vocal_bars: if self.auto_phrase { self.vocal_range } else { None },
            inst_bars: self.inst_range,
            per_track: if transition { 10 } else { 2 },
            max_results: if transition { 10 } else { 30 },
            ..Default::default()
        };
        let q = Query { vocal, vocal_bar: (!self.auto_phrase).then_some(self.phrase_bar), candidates, opts, rerank: self.rerank };
        let (tx, rx) = crossbeam_channel::bounded(1);
        let progress = Arc::new(Mutex::new((0.0, "Starting".to_string())));
        let cancel = Arc::new(AtomicBool::new(false));
        let (p2, c2) = (progress.clone(), cancel.clone());
        std::thread::Builder::new()
            .name("cc-mashup".into())
            .spawn(move || {
                let r = mashup::run(&q, &c2, &mut |f, l| *p2.lock() = (f, l.to_string())).map_err(|e| format!("{e:#}"));
                let _ = tx.send(r);
            })
            .ok();
        self.job = Some(Job { rx, progress, cancel });
        self.results.clear();
        self.previewing = None;
        self.message.clear();
    }

    pub fn ui(&mut self, ctx: &egui::Context, sh: &Shared, mix: &Mix, preview_playing: bool, out: &mut Vec<MashupAction>) {
        if !self.open {
            return;
        }
        if self.run_next_frame {
            self.run_next_frame = false;
            self.start(mix);
        }
        if let Some(j) = &self.job {
            if let Ok(r) = j.rx.try_recv() {
                match r {
                    Ok(v) => {
                        self.message = if v.is_empty() { "No match within the tempo and key limits. Try a wider tempo range or more key shift.".into() } else { String::new() };
                        self.results = v;
                    }
                    Err(e) => self.message = e,
                }
                self.job = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        if !preview_playing {
            self.previewing = None;
        }
        let mut open = self.open;
        egui::Window::new("🎤 Mashup finder")
            .open(&mut open)
            .default_size([980.0, 560.0])
            .min_width(760.0)
            .collapsible(false)
            .show(ctx, |ui| self.contents(ui, sh, mix, out));
        if !open {
            self.open = false;
            if let Some(j) = self.job.take() {
                j.cancel.store(true, Ordering::Relaxed);
            }
            if self.previewing.is_some() {
                out.push(MashupAction::StopPreview);
            }
        }
    }

    fn contents(&mut self, ui: &mut Ui, sh: &Shared, mix: &Mix, out: &mut Vec<MashupAction>) {
        let with_stems: Vec<TrackId> = sh.tracks.iter().filter(|t| sh.analyses.get(&t.id).is_some_and(|a| a.stems_model.is_some() && a.grid.is_some())).map(|t| t.id).collect();
        // ------------------------------------------------ query
        ui.horizontal(|ui| {
            ui.label(RichText::new("Vocals from").color(theme::TEXT_DIM));
            let sel = self.vocal.map(|id| track_label(sh, id)).unwrap_or_else(|| "choose a track…".into());
            egui::ComboBox::from_id_salt("mashup-vocal").width(420.0).selected_text(crate::studio::truncate(&sel, 60)).show_ui(ui, |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.pick).hint_text("search").desired_width(400.0));
                let q = self.pick.to_lowercase();
                let mut n = 0;
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for &id in &with_stems {
                        let l = track_label(sh, id);
                        if !q.is_empty() && !l.to_lowercase().contains(&q) {
                            continue;
                        }
                        n += 1;
                        if n > 300 {
                            break;
                        }
                        if ui.selectable_label(self.vocal == Some(id), l).clicked() {
                            self.vocal = Some(id);
                            self.target = Target::Library;
                            self.inst_only = None;
                            self.vocal_range = None;
                            self.inst_range = None;
                        }
                    }
                });
            });
            if let Target::Transition { index, .. } = self.target {
                ui.label(RichText::new(format!("over track {} (transition {} ▶ {})", index + 2, index + 1, index + 2)).color(theme::accent()));
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new("Phrase").color(theme::TEXT_DIM));
            ui.selectable_value(&mut self.auto_phrase, true, "Best vocal phrases");
            ui.selectable_value(&mut self.auto_phrase, false, "From bar");
            if !self.auto_phrase {
                let mut b = self.phrase_bar + 1;
                ui.add(egui::DragValue::new(&mut b).range(1..=999).speed(0.2));
                self.phrase_bar = b - 1;
            }
            ui.separator();
            ui.label(RichText::new("Length").color(theme::TEXT_DIM));
            for b in [8usize, 16, 32] {
                ui.selectable_value(&mut self.bars, b, format!("{b} bars"));
            }
            ui.separator();
            ui.label(RichText::new("Key shift").color(theme::TEXT_DIM));
            for k in 0..=3 {
                ui.selectable_value(&mut self.key_range, k, if k == 0 { "none".to_string() } else { format!("±{k}") });
            }
            ui.separator();
            ui.label(RichText::new("Tempo").color(theme::TEXT_DIM));
            for t in [3u32, 6, 10] {
                ui.selectable_value(&mut self.tempo_pct, t, format!("±{t} %"));
            }
        });
        ui.horizontal(|ui| {
            if self.inst_only.is_none() {
                ui.label(RichText::new("Search in").color(theme::TEXT_DIM));
                ui.selectable_value(&mut self.scope_mix, false, format!("Library ({} with stems)", with_stems.len()));
                ui.add_enabled_ui(!mix.tracks.is_empty(), |ui| {
                    ui.selectable_value(&mut self.scope_mix, true, "This mix");
                });
                ui.separator();
            }
            let avail = mashup::rerank_available();
            ui.add_enabled(avail, egui::Checkbox::new(&mut self.rerank, "AI re-rank (COCOLA)")).on_hover_text(if avail {
                "Listen to the best 20 candidates with a neural network trained on separated stems (a few seconds)"
            } else {
                "Model not installed (models/cocola.onnx, see tools/model-export/export_cocola.py)"
            });
            ui.add_space(10.0);
            let running = self.job.is_some();
            let b = egui::Button::new(RichText::new(if running { "Searching…" } else { "Find" }).strong().color(Color32::BLACK)).fill(theme::accent()).min_size(egui::vec2(90.0, 24.0));
            if ui.add_enabled(self.vocal.is_some() && !running, b).clicked() {
                self.start(mix);
            }
            if running && ui.button("Cancel").clicked() {
                if let Some(j) = self.job.take() {
                    j.cancel.store(true, Ordering::Relaxed);
                }
            }
        });
        if let Some(j) = &self.job {
            let (f, l) = j.progress.lock().clone();
            ui.add(egui::ProgressBar::new(f).text(l).desired_height(16.0));
        }
        if !self.message.is_empty() {
            ui.label(RichText::new(&self.message).color(theme::WARN));
        }
        if with_stems.len() < 10 {
            ui.label(RichText::new(format!("Only {} tracks have stems: separate more of your library for more matches.", with_stems.len())).size(11.0).color(theme::TEXT_DIM));
        }
        ui.separator();
        // ------------------------------------------------ results
        if self.results.is_empty() {
            ui.label(RichText::new("Matches show the bars of the instrumental where the vocal phrase fits, the key shift and tempo change it needs, and how well every bar fits (green = consonant, red = clash).").color(theme::TEXT_DIM));
            return;
        }
        let vocal_in_mix = self.vocal.and_then(|v| mix.tracks.iter().position(|t| t.track_id == v));
        let results = self.results.clone();
        egui::ScrollArea::vertical().id_salt("mashup-results").show(ui, |ui| {
            egui::Grid::new("mashup-grid").striped(true).num_columns(9).spacing([10.0, 6.0]).show(ui, |ui| {
                for h in ["", "Instrumental", "Where", "Vocals", "Shift", "Fit per bar", "Score", "AI", ""] {
                    ui.label(RichText::new(h).size(11.0).color(theme::TEXT_DIM));
                }
                ui.end_row();
                for (i, m) in results.iter().enumerate() {
                    ui.label(RichText::new(format!("{}", i + 1)).color(theme::TEXT_DIM));
                    ui.label(crate::studio::truncate(&track_label(sh, m.inst_track), 48));
                    let when = |id: TrackId, bar: i32| sh.analyses.get(&id).and_then(|a| a.grid).map(|g| theme::fmt_time_short(g.time_of_bar(bar as f64))).unwrap_or_default();
                    ui.label(format!("bars {}–{} · {}", m.inst_bar + 1, m.inst_bar + m.bars as i32, when(m.inst_track, m.inst_bar)));
                    ui.label(RichText::new(format!("bars {}–{} · {}", m.vocal_bar + 1, m.vocal_bar + m.bars as i32, when(m.vocal_track, m.vocal_bar))).color(theme::TEXT_DIM));
                    let tempo = (m.tempo_ratio - 1.0) * 100.0;
                    let mut shift = if m.key_shift == 0 { "key ±0".to_string() } else { format!("key {:+}", m.key_shift) };
                    if tempo.abs() >= 0.1 {
                        shift += &format!(" · {tempo:+.1} %");
                    }
                    ui.label(RichText::new(shift).monospace().size(11.5));
                    fit_strip(ui, &m.per_bar).on_hover_text(format!(
                        "harmony {:.0} % (weakest 4 bars {:.0} %) · masking {:.0} % · leftover vocals {:.0} %",
                        m.harmony * 100.0,
                        m.harmony_min * 100.0,
                        m.masking * 100.0,
                        m.residual_vocals * 100.0
                    ));
                    score_bar(ui, m.score);
                    ui.label(m.learned.map(|l| format!("{:.0}", l * 100.0)).unwrap_or_else(|| "–".into()));
                    ui.horizontal(|ui| {
                        let playing = self.previewing == Some(i);
                        if ui.button(if playing { "■" } else { "▶" }).on_hover_text(if playing { "Stop" } else { "Preview: vocals over this instrumental" }).clicked() {
                            if playing {
                                out.push(MashupAction::StopPreview);
                                self.previewing = None;
                            } else {
                                out.push(MashupAction::Preview(m.clone()));
                                self.previewing = Some(i);
                            }
                        }
                        match self.target {
                            Target::Transition { index, mix_id } if mix_id == mix.id => {
                                if ui.button("Use").on_hover_text("Set this transition to the mashup").clicked() {
                                    out.push(MashupAction::Apply(m.clone(), index));
                                }
                            }
                            _ => {
                                if ui.button("New mix").on_hover_text("Create a mix with this mashup").clicked() {
                                    out.push(MashupAction::NewMix(m.clone()));
                                }
                                if vocal_in_mix.is_some() && ui.button("Insert").on_hover_text("Put the instrumental after the vocal track in the open mix").clicked() {
                                    out.push(MashupAction::Insert(m.clone()));
                                }
                            }
                        }
                    });
                    ui.end_row();
                }
            });
        });
    }
}

/// Coloured bars: fit of every bar of the phrase.
fn fit_strip(ui: &mut Ui, per_bar: &[f32]) -> egui::Response {
    let w = (per_bar.len() as f32 * 7.0).clamp(60.0, 230.0);
    let (r, resp) = ui.allocate_exact_size(egui::vec2(w, 14.0), Sense::hover());
    let p = ui.painter();
    let bw = r.width() / per_bar.len().max(1) as f32;
    for (k, h) in per_bar.iter().enumerate() {
        let col = if h.is_nan() {
            theme::LINE
        } else if *h >= 0.62 {
            // calibrated: a vocal over its own instrumental averages ~0.63, random placements ~0.48
            theme::OK
        } else if *h >= 0.38 {
            theme::WARN
        } else {
            Color32::from_rgb(240, 80, 80)
        };
        let x = r.left() + k as f32 * bw;
        let hh = if h.is_nan() { 3.0 } else { 4.0 + 10.0 * h.clamp(0.0, 1.0) };
        p.rect_filled(egui::Rect::from_min_max(egui::pos2(x + 0.5, r.bottom() - hh), egui::pos2(x + bw - 0.5, r.bottom())), 1.0, col);
    }
    resp
}

fn score_bar(ui: &mut Ui, s: f32) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(86.0, 14.0), Sense::hover());
    let p = ui.painter();
    let bar = egui::Rect::from_min_size(r.left_center() - egui::vec2(0.0, 3.0), egui::vec2(56.0, 6.0));
    p.rect_filled(bar, 3.0, theme::LINE);
    p.rect_filled(egui::Rect::from_min_size(bar.min, egui::vec2(56.0 * s.clamp(0.0, 1.0), 6.0)), 3.0, theme::blend(theme::WARN, theme::OK, ((s - 0.4) / 0.4).clamp(0.0, 1.0)));
    p.text(egui::pos2(bar.right() + 6.0, r.center().y), egui::Align2::LEFT_CENTER, format!("{:.0}", s * 100.0), egui::FontId::proportional(12.0), theme::TEXT);
}
