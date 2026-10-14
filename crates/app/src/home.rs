//! Home page shown on launch: a full-height dashboard with the recent mixes on the left, and a
//! new-mix panel and the library status on the right.

use crate::app::Shared;
use crate::theme;
use clubcraft_core::{Key, Mix, TrackId};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, RichText, Sense, Stroke, StrokeKind, Ui};

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
    /// Filter for the recent mixes.
    pub filter: String,
}

impl Default for HomeView {
    fn default() -> Self {
        HomeView { new_name: String::new(), source: Source::Empty, auto_count: 12, bpm_range: (120.0, 130.0), harmonize: true, filter: String::new() }
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

const MARGIN: f32 = 28.0;
const GAP: f32 = 18.0;
const SIDE_W: f32 = 380.0;

impl HomeView {
    pub fn ui(&mut self, ui: &mut Ui, sh: &Shared, mixes: &[MixSummary], selected: &[TrackId], out: &mut Vec<HomeAction>) {
        let full = ui.available_rect_before_wrap();
        let area = Rect::from_min_max(full.min + vec2(MARGIN, 20.0), full.max - vec2(MARGIN, MARGIN));
        // ---- greeting row
        let top = Rect::from_min_size(area.min, vec2(area.width(), 44.0));
        let mut row = ui.new_child(egui::UiBuilder::new().max_rect(top).layout(egui::Layout::left_to_right(egui::Align::Center)));
        use chrono::Timelike;
        let greeting = match chrono::Local::now().hour() {
            5..=11 => "Good morning",
            12..=17 => "Good afternoon",
            _ => "Good evening",
        };
        row.label(RichText::new(greeting).size(26.0).strong().color(theme::TEXT));
        row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if ui.add(button("Go live ▶", true)).clicked() {
                out.push(HomeAction::GoLive);
            }
            if ui.add(button("Open library", false)).clicked() {
                out.push(HomeAction::GoLibrary);
            }
        });
        // ---- body: recent mixes | new mix + library
        let body = Rect::from_min_max(pos2(area.left(), top.bottom() + 16.0), area.max);
        let side = Rect::from_min_max(pos2(body.right() - SIDE_W, body.top()), body.max);
        let main = Rect::from_min_max(body.min, pos2(side.left() - GAP, body.bottom()));
        self.recent_panel(ui, main, mixes, out);
        let new_h = self.new_mix_panel(ui, side, sh, selected, out);
        library_panel(ui, Rect::from_min_max(pos2(side.left(), side.top() + new_h + GAP), side.max), sh, out);
        ui.allocate_rect(full, Sense::hover());
    }

    fn recent_panel(&mut self, ui: &mut Ui, r: Rect, mixes: &[MixSummary], out: &mut Vec<HomeAction>) {
        panel_bg(ui, r);
        let inner = r.shrink(18.0);
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
        let ui = &mut ui;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Recent mixes").size(16.0).strong().color(theme::TEXT));
            ui.label(RichText::new(mixes.len().to_string()).size(13.0).color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Search mixes").desired_width(220.0));
            });
        });
        ui.add_space(10.0);
        // column header
        let w = ui.available_width();
        let cols = [w - 330.0, 80.0, 90.0, 120.0];
        let (hr, _) = ui.allocate_exact_size(vec2(w, 22.0), Sense::hover());
        let p = ui.painter();
        let mut x = hr.left() + 12.0;
        for (k, (name, cw)) in ["NAME", "TRACKS", "LENGTH", "EDITED"].iter().zip(cols).enumerate() {
            let pos = if k == 0 { pos2(x, hr.center().y) } else { pos2(x + cw - 12.0, hr.center().y) };
            p.text(pos, if k == 0 { Align2::LEFT_CENTER } else { Align2::RIGHT_CENTER }, *name, FontId::proportional(11.0), theme::TEXT_DIM);
            x += cw;
        }
        p.line_segment([hr.left_bottom(), hr.right_bottom()], Stroke::new(1.0, theme::LINE));
        let q = self.filter.trim().to_lowercase();
        let shown: Vec<&MixSummary> = mixes.iter().filter(|m| q.is_empty() || m.name.to_lowercase().contains(&q)).collect();
        if mixes.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No mixes yet").size(15.0).color(theme::TEXT));
                ui.label(RichText::new("Create one on the right, or import your DJ.Studio projects.").color(theme::TEXT_DIM));
            });
            return;
        }
        egui::ScrollArea::vertical().id_salt("recent-mixes").auto_shrink([false, false]).show(ui, |ui| {
            for m in shown {
                mix_row(ui, m, w, cols, out);
            }
        });
    }

    /// The new-mix panel at the top of `r`; returns its height.
    fn new_mix_panel(&mut self, ui: &mut Ui, r: Rect, sh: &Shared, selected: &[TrackId], out: &mut Vec<HomeAction>) -> f32 {
        let resp = egui::Frame::new()
            .fill(theme::PANEL)
            .corner_radius(12.0)
            .stroke(Stroke::new(1.0, theme::LINE))
            .inner_margin(18.0)
            .show(&mut ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::top_down(egui::Align::Min))), |ui| {
                ui.set_width(r.width() - 36.0);
                ui.label(RichText::new("New mix").size(16.0).strong().color(theme::TEXT));
                ui.add_space(10.0);
                ui.add(egui::TextEdit::singleline(&mut self.new_name).hint_text("Mix name, e.g. Friday Warm-up").desired_width(f32::INFINITY).margin(vec2(10.0, 7.0)));
                ui.add_space(12.0);
                ui.label(RichText::new("Start from").size(11.5).color(theme::TEXT_DIM));
                ui.add_space(4.0);
                let opts = [(Source::Empty, "Empty".to_string(), true), (Source::Selection, format!("Selection ({})", selected.len()), !selected.is_empty()), (Source::AutoBuild, "Auto-build".to_string(), true)];
                if let Some(k) = segmented(ui, &opts.iter().map(|o| (o.1.as_str(), o.2)).collect::<Vec<_>>(), opts.iter().position(|o| o.0 == self.source)) {
                    self.source = opts[k].0;
                }
                if !sh.playlists.is_empty() {
                    ui.add_space(6.0);
                    let cur = match self.source {
                        Source::Playlist(id) => sh.playlists.iter().find(|p| p.0 == id).map(|p| p.1.clone()).unwrap_or_default(),
                        _ => "From a playlist…".into(),
                    };
                    egui::ComboBox::from_id_salt("home-playlist").selected_text(cur).width(ui.available_width()).show_ui(ui, |ui| {
                        for (id, name) in &sh.playlists {
                            ui.selectable_value(&mut self.source, Source::Playlist(*id), name);
                        }
                    });
                }
                let pool: Vec<TrackId> = match self.source {
                    Source::Empty => vec![],
                    Source::Selection => selected.to_vec(),
                    Source::Playlist(p) => sh.playlist_tracks.get(&p).cloned().unwrap_or_default(),
                    Source::AutoBuild => {
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Tracks").color(theme::TEXT_DIM));
                            ui.add(egui::DragValue::new(&mut self.auto_count).range(2..=60));
                            ui.add_space(8.0);
                            ui.label(RichText::new("BPM").color(theme::TEXT_DIM));
                            ui.add(egui::DragValue::new(&mut self.bpm_range.0).range(60.0..=200.0).speed(0.5));
                            ui.label("–");
                            ui.add(egui::DragValue::new(&mut self.bpm_range.1).range(60.0..=200.0).speed(0.5));
                        });
                        auto_pick(sh, self.auto_count, self.bpm_range)
                    }
                };
                if self.source != Source::Empty {
                    ui.add_space(6.0);
                    ui.checkbox(&mut self.harmonize, "Harmonize order (key, tempo, energy)");
                    let analysed = pool.iter().filter(|id| sh.analyses.get(id).is_some_and(|a| a.version > 0)).count();
                    ui.label(RichText::new(format!("{} tracks · {} analysed", pool.len(), analysed)).size(11.5).color(theme::TEXT_DIM));
                }
                ui.add_space(14.0);
                let name = if self.new_name.trim().is_empty() { "New mix".to_string() } else { self.new_name.trim().to_string() };
                if wide_button(ui, "Create mix", 36.0, true).clicked() {
                    out.push(HomeAction::NewMix { name, tracks: pool, harmonize: self.harmonize && self.source != Source::Empty });
                    self.new_name.clear();
                }
            });
        resp.response.rect.height()
    }
}

fn panel_bg(ui: &Ui, r: Rect) {
    let p = ui.painter();
    p.rect_filled(r, 12.0, theme::PANEL);
    p.rect_stroke(r, 12.0, Stroke::new(1.0, theme::LINE), StrokeKind::Inside);
}

fn button(text: &str, primary: bool) -> egui::Button<'_> {
    let t = RichText::new(text).size(13.0);
    let b = egui::Button::new(if primary { t.strong().color(Color32::BLACK) } else { t.color(theme::TEXT) }).corner_radius(8.0).min_size(vec2(0.0, 32.0));
    if primary {
        b.fill(theme::accent())
    } else {
        b.fill(theme::PANEL).stroke(Stroke::new(1.0, theme::LINE))
    }
}

/// Full-width button with a centred label.
fn wide_button(ui: &mut Ui, text: &str, h: f32, primary: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    let p = ui.painter();
    let hover = resp.hovered();
    let (fill, col) = if primary { (if hover { theme::accent().gamma_multiply(0.88) } else { theme::accent() }, Color32::BLACK) } else { (if hover { theme::PANEL_HI } else { theme::PANEL }, theme::TEXT) };
    p.rect_filled(r, 8.0, fill);
    if !primary {
        p.rect_stroke(r, 8.0, Stroke::new(1.0, theme::LINE), StrokeKind::Inside);
    }
    p.text(r.center(), Align2::CENTER_CENTER, text, FontId::proportional(13.5), col);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Segmented control: one option per segment (label, enabled); returns the clicked index.
fn segmented(ui: &mut Ui, opts: &[(&str, bool)], sel: Option<usize>) -> Option<usize> {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
    let p = ui.painter().clone();
    p.rect_filled(r, 8.0, theme::SEG_TRACK);
    let w = (r.width() - 6.0) / opts.len() as f32;
    let mut clicked = None;
    for (k, (label, enabled)) in opts.iter().enumerate() {
        let s = Rect::from_min_size(pos2(r.left() + 3.0 + w * k as f32, r.top() + 3.0), vec2(w, r.height() - 6.0));
        let resp = ui.interact(s, ui.id().with(("seg", k)), if *enabled { Sense::click() } else { Sense::hover() });
        if sel == Some(k) {
            p.rect_filled(s, 6.0, theme::SEG_ON);
            p.rect_stroke(s, 6.0, Stroke::new(1.0, theme::SEG_ON_LINE), StrokeKind::Inside);
        } else if resp.hovered() && *enabled {
            p.rect_filled(s, 6.0, theme::SEG_HOVER);
        }
        let col = if !enabled { theme::TEXT_DIM.gamma_multiply(0.5) } else if sel == Some(k) || resp.hovered() { theme::TEXT } else { theme::TEXT_DIM };
        p.text(s.center(), Align2::CENTER_CENTER, *label, FontId::proportional(12.5), col);
        if resp.clicked() {
            clicked = Some(k);
        }
    }
    clicked
}

fn mix_row(ui: &mut Ui, m: &MixSummary, w: f32, cols: [f32; 4], out: &mut Vec<HomeAction>) {
    let (r, resp) = ui.allocate_exact_size(vec2(w, 56.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, 8.0, theme::PANEL_HI);
    }
    p.line_segment([r.left_bottom() + vec2(12.0, 0.0), r.right_bottom() - vec2(12.0, 0.0)], Stroke::new(1.0, theme::LINE.gamma_multiply(0.6)));
    let x0 = r.left() + 12.0;
    let name_w = cols[0] - 24.0;
    p.text(pos2(x0, r.top() + 12.0), Align2::LEFT_TOP, crate::studio::truncate(&m.name, (name_w / 7.5) as usize), FontId::proportional(14.5), theme::TEXT);
    // key strip: one segment per track, in its (shifted) key colour
    let strip = Rect::from_min_size(pos2(x0, r.top() + 36.0), vec2(name_w.min(260.0), 5.0));
    let n = m.keys.len().max(1);
    let sw = strip.width() / n as f32;
    for (i, k) in m.keys.iter().enumerate() {
        let s = Rect::from_min_size(strip.min + vec2(i as f32 * sw, 0.0), vec2((sw - 1.5).max(1.0), strip.height()));
        p.rect_filled(s, 1.5, k.map(theme::key_color).unwrap_or(theme::LINE));
    }
    let mut x = r.left() + cols[0];
    for (txt, cw) in [(m.tracks.to_string(), cols[1]), (fmt_dur(m.secs), cols[2]), (fmt_date(m.modified), cols[3])] {
        p.text(pos2(x + cw - 12.0, r.center().y), Align2::RIGHT_CENTER, txt, FontId::proportional(13.0), theme::TEXT_DIM);
        x += cw;
    }
    if resp.clicked() {
        out.push(HomeAction::OpenMix(m.id));
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).context_menu(|ui| {
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

/// Library status (tracks, analysis and stem progress) and the import actions, filling `r`.
fn library_panel(ui: &mut Ui, r: Rect, sh: &Shared, out: &mut Vec<HomeAction>) {
    if r.height() < 120.0 {
        return;
    }
    panel_bg(ui, r);
    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink(18.0)).layout(egui::Layout::top_down(egui::Align::Min)));
    let ui = &mut ui;
    let n = sh.tracks.len();
    let analysed = sh.analyses.values().filter(|a| a.version > 0).count();
    let stems = sh.analyses.values().filter(|a| a.stems_model.is_some()).count();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Library").size(16.0).strong().color(theme::TEXT));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(format!("{n} tracks")).size(13.0).color(theme::TEXT_DIM));
        });
    });
    ui.add_space(14.0);
    for (label, done) in [("Analysed", analysed), ("Stems separated", stems)] {
        let w = ui.available_width();
        let (row, _) = ui.allocate_exact_size(vec2(w, 40.0), Sense::hover());
        let p = ui.painter();
        p.text(row.left_top(), Align2::LEFT_TOP, label, FontId::proportional(13.0), theme::TEXT);
        p.text(row.right_top(), Align2::RIGHT_TOP, format!("{done} / {n}"), FontId::proportional(13.0), theme::TEXT_DIM);
        let bar = Rect::from_min_size(row.left_top() + vec2(0.0, 24.0), vec2(w, 5.0));
        p.rect_filled(bar, 2.5, theme::SEG_TRACK);
        if n > 0 {
            p.rect_filled(Rect::from_min_size(bar.min, vec2(w * done as f32 / n as f32, 5.0)), 2.5, theme::accent());
        }
        ui.add_space(6.0);
    }
    // actions at the bottom of the panel
    let avail = ui.available_rect_before_wrap();
    let bw = ((avail.width() - 8.0) / 2.0).floor();
    let row = Rect::from_min_size(pos2(avail.left(), avail.bottom() - 34.0), vec2(avail.width(), 34.0));
    // recently added tracks in between
    let list = Rect::from_min_max(pos2(avail.left(), avail.top() + 8.0), pos2(avail.right(), row.top() - 14.0));
    if list.height() > 60.0 {
        let p = ui.painter().clone();
        p.text(list.left_top(), Align2::LEFT_TOP, "RECENTLY ADDED", FontId::proportional(11.0), theme::TEXT_DIM);
        let mut tracks: Vec<&clubcraft_core::Track> = sh.tracks.iter().collect();
        tracks.sort_by(|a, b| b.added_at.cmp(&a.added_at).then(b.id.cmp(&a.id)));
        let rh = 40.0;
        let rows = ((list.height() - 22.0) / rh).floor().max(0.0) as usize;
        for (i, t) in tracks.into_iter().take(rows).enumerate() {
            let r = Rect::from_min_size(pos2(list.left(), list.top() + 22.0 + i as f32 * rh), vec2(list.width(), rh));
            let a = sh.analyses.get(&t.id);
            let right = list.width() - 96.0;
            let title = if t.title.is_empty() { t.display_name() } else { t.title.clone() };
            p.text(pos2(r.left(), r.top() + 4.0), Align2::LEFT_TOP, crate::studio::truncate(&title, (right / 7.0) as usize), FontId::proportional(13.0), theme::TEXT);
            p.text(pos2(r.left(), r.top() + 21.0), Align2::LEFT_TOP, crate::studio::truncate(&t.artist, (right / 6.5) as usize), FontId::proportional(11.5), theme::TEXT_DIM);
            if let Some(b) = a.and_then(|a| a.bpm()) {
                p.text(pos2(r.right() - 44.0, r.center().y), Align2::RIGHT_CENTER, format!("{b:.0}"), FontId::proportional(12.5), theme::TEXT_DIM);
            }
            if let Some(k) = a.and_then(|a| a.key) {
                let chip = Rect::from_center_size(pos2(r.right() - 18.0, r.center().y), vec2(34.0, 18.0));
                p.rect_stroke(chip, 5.0, Stroke::new(1.0, theme::key_color(k)), StrokeKind::Inside);
                p.text(chip.center(), Align2::CENTER_CENTER, k.camelot(), FontId::proportional(11.0), theme::key_color(k));
            }
        }
    }
    let mut b = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::left_to_right(egui::Align::Center)));
    b.spacing_mut().item_spacing.x = 8.0;
    for (label, act) in [("Add music folder", HomeAction::AddMusic), ("Import DJ.Studio", HomeAction::ImportDjStudio)] {
        let mut c = b.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(b.cursor().min, vec2(bw, 34.0))));
        if wide_button(&mut c, label, 34.0, false).clicked() {
            out.push(act);
        }
        b.add_space(bw + 8.0);
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

