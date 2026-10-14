//! Home page shown on launch: create a new mix, open recent mixes, quick actions.

use crate::app::Shared;
use crate::theme;
use clubcraft_core::{Key, Mix, TrackId};
use eframe::egui::{self, vec2, Color32, RichText, Sense, Stroke, StrokeKind, Ui};

#[derive(Clone, Debug)]
pub struct MixSummary {
    pub id: i64,
    pub name: String,
    pub modified: i64,
    pub tracks: usize,
    pub keys: Vec<Option<Key>>,
    pub secs: f64,
}

impl MixSummary {
    pub fn from_mix(m: &Mix, modified: i64, sh: &Shared) -> MixSummary {
        let secs = m
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| if i + 1 < m.tracks.len() { (t.mix_out - t.mix_in).max(0.0) } else { sh.analyses.get(&t.track_id).map(|a| a.duration - t.mix_in).unwrap_or(0.0) })
            .sum();
        MixSummary {
            id: m.id,
            name: m.name.clone(),
            modified,
            tracks: m.tracks.len(),
            keys: m.tracks.iter().map(|t| sh.analyses.get(&t.track_id).and_then(|a| a.key).map(|k| k.shifted(t.key_shift))).collect(),
            secs,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Empty,
    Selection,
    Playlist(i64),
    AutoBuild,
}

pub struct HomeView {
    pub new_name: String,
    pub source: Source,
    pub auto_count: usize,
    pub bpm_range: (f64, f64),
    pub harmonize: bool,
}

impl Default for HomeView {
    fn default() -> Self {
        HomeView { new_name: String::new(), source: Source::Empty, auto_count: 12, bpm_range: (120.0, 130.0), harmonize: true }
    }
}

#[derive(Debug, Clone)]
pub enum HomeAction {
    /// Create a mix from these tracks (possibly empty), harmonised or in this order.
    NewMix { name: String, tracks: Vec<TrackId>, harmonize: bool },
    OpenMix(i64),
    DeleteMix(i64),
    AddMusic,
    ImportDjStudio,
    GoLibrary,
    GoLive,
}

fn fmt_dur(s: f64) -> String {
    let m = (s / 60.0).round() as i64;
    if m >= 60 {
        format!("{}h {:02}m", m / 60, m % 60)
    } else {
        format!("{m} min")
    }
}

fn fmt_date(ts: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let d = (now - ts).max(0);
    match d {
        0..=119 => "just now".into(),
        120..=7199 => format!("{} min ago", d / 60),
        7200..=172_799 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86_400),
    }
}

impl HomeView {
    pub fn ui(&mut self, ui: &mut Ui, sh: &Shared, mixes: &[MixSummary], selected: &[TrackId], out: &mut Vec<HomeAction>) {
        egui::ScrollArea::vertical().id_salt("home").show(ui, |ui| {
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                ui.vertical(|ui| {
                    use chrono::Timelike;
                    let greeting = match chrono::Local::now().hour() {
                        5..=11 => "Good morning",
                        12..=17 => "Good afternoon",
                        _ => "Good evening",
                    };
                    ui.label(RichText::new(greeting).size(28.0).strong().color(theme::TEXT));
                    ui.label(RichText::new("Design mixes with stems in the studio, perform them live on the decks.").size(15.0).color(theme::TEXT_DIM));
                });
            });
            ui.add_space(18.0);
            ui.horizontal_top(|ui| {
                ui.add_space(24.0);
                let w = (ui.available_width() - 48.0).max(600.0);
                ui.vertical(|ui| {
                    ui.set_width(w * 0.42);
                    self.new_mix_card(ui, sh, selected, out);
                    ui.add_space(14.0);
                    quick_actions(ui, sh, out);
                });
                ui.add_space(18.0);
                ui.vertical(|ui| {
                    ui.set_width(w * 0.58 - 18.0);
                    ui.label(RichText::new("Recent mixes").size(18.0).strong());
                    ui.add_space(6.0);
                    if mixes.is_empty() {
                        ui.label(RichText::new("No mixes yet. Create one on the left, or import your DJ.Studio projects.").color(theme::TEXT_DIM));
                    }
                    let avail = ui.available_width();
                    let cols = ((avail + 12.0) / 252.0).floor().max(1.0) as usize;
                    let card_w = ((avail - 12.0 * (cols as f32 - 1.0)) / cols as f32).floor();
                    egui::Grid::new("recent").spacing(vec2(12.0, 12.0)).show(ui, |ui| {
                        for (i, m) in mixes.iter().enumerate() {
                            mix_card(ui, m, card_w, out);
                            if (i + 1) % cols == 0 {
                                ui.end_row();
                            }
                        }
                    });
                });
            });
            ui.add_space(24.0);
        });
    }

    fn new_mix_card(&mut self, ui: &mut Ui, sh: &Shared, selected: &[TrackId], out: &mut Vec<HomeAction>) {
        card(ui, |ui| {
            ui.label(RichText::new("New mix").size(18.0).strong());
            ui.add_space(4.0);
            ui.add(egui::TextEdit::singleline(&mut self.new_name).hint_text("Mix name, e.g. Friday Warm-up").desired_width(f32::INFINITY).font(egui::TextStyle::Heading));
            ui.add_space(6.0);
            ui.label(RichText::new("Start from").color(theme::TEXT_DIM));
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.source, Source::Empty, "Empty");
                ui.add_enabled_ui(!selected.is_empty(), |ui| {
                    ui.selectable_value(&mut self.source, Source::Selection, format!("{} selected tracks", selected.len()));
                });
                ui.selectable_value(&mut self.source, Source::AutoBuild, "Auto-build from library");
                for (id, name) in &sh.playlists {
                    ui.selectable_value(&mut self.source, Source::Playlist(*id), format!("Playlist: {name}"));
                }
            });
            let pool: Vec<TrackId> = match self.source {
                Source::Empty => vec![],
                Source::Selection => selected.to_vec(),
                Source::Playlist(p) => sh.playlist_tracks.get(&p).cloned().unwrap_or_default(),
                Source::AutoBuild => {
                    ui.horizontal(|ui| {
                        ui.label("Tracks");
                        ui.add(egui::DragValue::new(&mut self.auto_count).range(2..=60));
                        ui.label("BPM");
                        ui.add(egui::DragValue::new(&mut self.bpm_range.0).range(60.0..=200.0).speed(0.5));
                        ui.label("–");
                        ui.add(egui::DragValue::new(&mut self.bpm_range.1).range(60.0..=200.0).speed(0.5));
                    });
                    auto_pick(sh, self.auto_count, self.bpm_range)
                }
            };
            if self.source != Source::Empty {
                ui.checkbox(&mut self.harmonize, "Harmonize order (key, tempo, energy)");
                let analysed = pool.iter().filter(|id| sh.analyses.get(id).is_some_and(|a| a.version > 0)).count();
                ui.label(RichText::new(format!("{} tracks · {} analysed", pool.len(), analysed)).size(11.5).color(theme::TEXT_DIM));
            }
            ui.add_space(6.0);
            let name = if self.new_name.trim().is_empty() { "New mix".to_string() } else { self.new_name.trim().to_string() };
            let b = egui::Button::new(RichText::new("Create mix ▶").size(15.0).strong().color(Color32::BLACK)).fill(theme::accent()).min_size(vec2(160.0, 34.0));
            if ui.add(b).clicked() {
                out.push(HomeAction::NewMix { name, tracks: pool, harmonize: self.harmonize && self.source != Source::Empty });
                self.new_name.clear();
            }
        });
    }
}

/// Pick analysed tracks in a BPM range, preferring stems-ready and higher-rated tracks.
fn auto_pick(sh: &Shared, n: usize, (lo, hi): (f64, f64)) -> Vec<TrackId> {
    let mut v: Vec<(TrackId, f64)> = sh
        .tracks
        .iter()
        .filter_map(|t| {
            let a = sh.analyses.get(&t.id)?;
            let b = a.bpm()?;
            if b < lo - 0.5 || b > hi + 0.5 {
                return None;
            }
            let score = a.stems_model.is_some() as i32 as f64 * 2.0 + t.rating as f64 + (t.play_count as f64).ln_1p() * 0.3;
            Some((t.id, score))
        })
        .collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v.into_iter().take(n).map(|x| x.0).collect()
}

fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(theme::PANEL_HI)
        .corner_radius(10.0)
        .inner_margin(16.0)
        .stroke(Stroke::new(1.0, theme::LINE))
        .show(ui, |ui| {
            ui.set_width(ui.available_width()); // cards fill their column
            add(ui)
        })
        .inner
}

fn mix_card(ui: &mut Ui, m: &MixSummary, width: f32, out: &mut Vec<HomeAction>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 112.0), Sense::click());
    let p = ui.painter();
    let hover = resp.hovered();
    p.rect_filled(rect, 10.0, if hover { theme::PANEL_HI.gamma_multiply(1.35) } else { theme::PANEL_HI });
    p.rect_stroke(rect, 10.0, Stroke::new(1.0, if hover { theme::accent() } else { theme::LINE }), StrokeKind::Inside);
    // key colour strip
    let strip = egui::Rect::from_min_size(rect.min + vec2(12.0, 12.0), vec2(rect.width() - 24.0, 8.0));
    let n = m.keys.len().max(1);
    for (i, k) in m.keys.iter().enumerate() {
        let w = strip.width() / n as f32;
        let r = egui::Rect::from_min_size(strip.min + vec2(i as f32 * w, 0.0), vec2(w - 1.0, strip.height()));
        p.rect_filled(r, 2.0, k.map(theme::key_color).unwrap_or(theme::LINE));
    }
    p.text(rect.min + vec2(12.0, 30.0), egui::Align2::LEFT_TOP, crate::studio::truncate(&m.name, ((width - 24.0) / 8.0) as usize), egui::FontId::proportional(15.0), theme::TEXT);
    p.text(rect.min + vec2(12.0, 54.0), egui::Align2::LEFT_TOP, format!("{} tracks · {}", m.tracks, fmt_dur(m.secs)), egui::FontId::proportional(12.0), theme::TEXT_DIM);
    p.text(rect.left_bottom() + vec2(12.0, -12.0), egui::Align2::LEFT_BOTTOM, fmt_date(m.modified), egui::FontId::proportional(11.0), theme::TEXT_DIM);
    if resp.clicked() {
        out.push(HomeAction::OpenMix(m.id));
    }
    resp.context_menu(|ui| {
        if ui.button("Open").clicked() {
            out.push(HomeAction::OpenMix(m.id));
            ui.close();
        }
        if ui.button("Delete mix").clicked() {
            out.push(HomeAction::DeleteMix(m.id));
            ui.close();
        }
    });
}

fn quick_actions(ui: &mut Ui, sh: &Shared, out: &mut Vec<HomeAction>) {
    card(ui, |ui| {
        ui.label(RichText::new("Library").size(18.0).strong());
        ui.add_space(8.0);
        let analysed = sh.analyses.values().filter(|a| a.version > 0).count();
        let stems = sh.analyses.values().filter(|a| a.stems_model.is_some()).count();
        let n = sh.tracks.len();
        // three stat tiles
        let gap = 10.0;
        let tile_w = ((ui.available_width() - 2.0 * gap) / 3.0).floor();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (value, label, done) in [(n, "tracks", None), (analysed, "analysed", Some(analysed)), (stems, "with stems", Some(stems))] {
                let (r, _) = ui.allocate_exact_size(vec2(tile_w, 58.0), Sense::hover());
                let p = ui.painter();
                p.rect_filled(r, 8.0, theme::PANEL);
                p.text(r.left_top() + vec2(12.0, 8.0), egui::Align2::LEFT_TOP, value.to_string(), egui::FontId::proportional(20.0), theme::TEXT);
                p.text(r.left_top() + vec2(12.0, 34.0), egui::Align2::LEFT_TOP, label, egui::FontId::proportional(11.5), theme::TEXT_DIM);
                if let Some(d) = done.filter(|_| n > 0) {
                    let bar = egui::Rect::from_min_max(r.left_bottom() + vec2(12.0, -7.0), r.right_bottom() + vec2(-12.0, -4.0));
                    p.rect_filled(bar, 1.5, theme::LINE);
                    p.rect_filled(egui::Rect::from_min_max(bar.min, egui::pos2(bar.left() + bar.width() * d as f32 / n as f32, bar.bottom())), 1.5, theme::accent());
                }
            }
        });
        ui.add_space(12.0);
        // actions: two equal columns
        let bw = ((ui.available_width() - gap) / 2.0).floor();
        for pair in [[("+ Add music folder", HomeAction::AddMusic), ("Import DJ.Studio", HomeAction::ImportDjStudio)], [("Open library", HomeAction::GoLibrary), ("Go live ▶", HomeAction::GoLive)]] {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (label, a) in pair {
                    if ui.add(egui::Button::new(RichText::new(label).size(13.5)).fill(theme::PANEL).stroke(Stroke::new(1.0, theme::LINE)).min_size(vec2(bw, 32.0))).clicked() {
                        out.push(a);
                    }
                }
            });
            ui.add_space(4.0);
        }
    });
}
