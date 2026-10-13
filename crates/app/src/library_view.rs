//! Library browser: searchable, sortable track table with analysis columns.

use crate::app::Shared;
use crate::theme;
use clubcraft_core::{Analysis, Track, TrackId};
use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortBy {
    Artist,
    Title,
    Bpm,
    Key,
    Energy,
    Duration,
    Added,
}

pub struct LibraryView {
    pub search: String,
    pub sort: SortBy,
    pub ascending: bool,
    pub selected: HashSet<TrackId>,
    pub last_clicked: Option<TrackId>,
    /// Restrict to a playlist.
    pub playlist: Option<i64>,
}

impl Default for LibraryView {
    fn default() -> Self {
        LibraryView { search: String::new(), sort: SortBy::Artist, ascending: true, selected: HashSet::new(), last_clicked: None, playlist: None }
    }
}

/// What the user asked to do with tracks from the table.
#[derive(Debug, Clone)]
pub enum LibraryAction {
    LoadDeck(TrackId, usize),
    AddToMix(Vec<TrackId>),
    Process(Vec<TrackId>),
    Reanalyze(Vec<TrackId>),
    Remove(Vec<TrackId>),
    Reveal(TrackId),
}

/// Drag payload for tracks dragged out of the table.
#[derive(Clone, Debug)]
pub struct DraggedTracks(pub Vec<TrackId>);

pub fn matches(t: &Track, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    let hay = format!("{} {} {} {}", t.artist, t.title, t.album, t.genre).to_lowercase();
    q.to_lowercase().split_whitespace().all(|w| hay.contains(w))
}

impl LibraryView {
    pub fn visible<'a>(&self, sh: &'a Shared) -> Vec<&'a Track> {
        let ids: Option<HashSet<TrackId>> = self.playlist.and_then(|p| sh.playlist_tracks.get(&p)).map(|v| v.iter().copied().collect());
        let mut v: Vec<&Track> = sh.tracks.iter().filter(|t| matches(t, &self.search) && ids.as_ref().is_none_or(|s| s.contains(&t.id))).collect();
        let an = &sh.analyses;
        let bpm = |t: &Track| an.get(&t.id).and_then(|a| a.bpm()).or(t.tag_bpm).unwrap_or(0.0);
        let key = |t: &Track| an.get(&t.id).and_then(|a| a.key).map(|k| k.camelot_number() as i32 * 2 + (!k.minor) as i32).unwrap_or(99);
        match self.sort {
            SortBy::Artist => v.sort_by(|a, b| a.artist.to_lowercase().cmp(&b.artist.to_lowercase()).then(a.title.cmp(&b.title))),
            SortBy::Title => v.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
            SortBy::Bpm => v.sort_by(|a, b| bpm(a).total_cmp(&bpm(b))),
            SortBy::Key => v.sort_by_key(|t| key(t)),
            SortBy::Energy => v.sort_by_key(|t| an.get(&t.id).and_then(|a| a.energy).unwrap_or(0)),
            SortBy::Duration => v.sort_by(|a, b| a.duration.total_cmp(&b.duration)),
            SortBy::Added => v.sort_by_key(|t| t.added_at),
        }
        if !self.ascending {
            v.reverse();
        }
        v
    }

    /// Draw the table. `compact` hides less important columns (used under the decks).
    pub fn ui(&mut self, ui: &mut egui::Ui, sh: &Shared, compact: bool, actions: &mut Vec<LibraryAction>) {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("🔍 Search artist, title, genre…").desired_width(260.0));
            if !self.search.is_empty() && ui.small_button("✕").clicked() {
                self.search.clear();
            }
            ui.separator();
            let label = match self.playlist {
                None => "All tracks".to_string(),
                Some(p) => sh.playlists.iter().find(|x| x.0 == p).map(|x| x.1.clone()).unwrap_or_default(),
            };
            egui::ComboBox::from_id_salt(("pl", compact)).selected_text(label).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.playlist, None, "All tracks");
                for (id, name) in &sh.playlists {
                    ui.selectable_value(&mut self.playlist, Some(*id), name);
                }
            });
            ui.label(RichText::new(format!("{} tracks", sh.tracks.len())).color(theme::TEXT_DIM));
        });
        let rows = self.visible(sh);
        let row_ids: Vec<TrackId> = rows.iter().map(|t| t.id).collect();
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click_and_drag())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(18.0))
            .column(Column::initial(if compact { 260.0 } else { 300.0 }).at_least(80.0).clip(true))
            .column(Column::initial(if compact { 180.0 } else { 220.0 }).at_least(60.0).clip(true))
            .column(Column::exact(52.0))
            .column(Column::exact(40.0))
            .column(Column::exact(34.0))
            .column(Column::exact(46.0));
        if !compact {
            table = table.column(Column::initial(120.0).clip(true)).column(Column::remainder().at_least(80.0));
        } else {
            table = table.column(Column::remainder().at_least(60.0));
        }
        let mut header = |ui: &mut egui::Ui, name: &str, s: Option<SortBy>| {
            let active = s == Some(self.sort);
            let txt = if active { format!("{name} {}", if self.ascending { "⏶" } else { "⏷" }) } else { name.to_string() };
            if ui.add(egui::Label::new(RichText::new(txt).strong().size(11.5)).sense(egui::Sense::click())).clicked() {
                if let Some(s) = s {
                    if self.sort == s {
                        self.ascending = !self.ascending;
                    } else {
                        self.sort = s;
                        self.ascending = true;
                    }
                }
            }
        };
        table
            .header(20.0, |mut h| {
                h.col(|ui| header(ui, "", None));
                h.col(|ui| header(ui, "Title", Some(SortBy::Title)));
                h.col(|ui| header(ui, "Artist", Some(SortBy::Artist)));
                h.col(|ui| header(ui, "BPM", Some(SortBy::Bpm)));
                h.col(|ui| header(ui, "Key", Some(SortBy::Key)));
                h.col(|ui| header(ui, "Nrg", Some(SortBy::Energy)));
                h.col(|ui| header(ui, "Time", Some(SortBy::Duration)));
                if !compact {
                    h.col(|ui| header(ui, "Genre", None));
                }
                h.col(|ui| header(ui, "Status", None));
            })
            .body(|body| {
                body.rows(20.0, rows.len(), |mut row| {
                    let t = rows[row.index()];
                    let a: Option<&Analysis> = sh.analyses.get(&t.id);
                    let sel = self.selected.contains(&t.id);
                    row.set_selected(sel);
                    row.col(|ui| {
                        let has_stems = a.and_then(|a| a.stems_model.as_ref()).is_some();
                        let c = if has_stems { theme::OK } else if a.is_some_and(|a| a.version > 0) { theme::TEXT_DIM } else { theme::LINE };
                        let (r, resp) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                        ui.painter().circle_filled(r.center(), if has_stems { 4.5 } else { 3.0 }, c);
                        resp.on_hover_text(if has_stems { "Stems ready" } else { "No stems yet" });
                    });
                    row.col(|ui| {
                        ui.label(&t.title);
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(&t.artist).color(theme::TEXT_DIM));
                    });
                    row.col(|ui| {
                        match a.and_then(|a| a.bpm()) {
                            Some(b) => ui.label(format!("{b:.1}")),
                            None => ui.label(RichText::new(t.tag_bpm.map(|b| format!("{b:.0}")).unwrap_or_default()).color(theme::TEXT_DIM)),
                        };
                    });
                    row.col(|ui| {
                        if let Some(k) = a.and_then(|a| a.key) {
                            ui.label(RichText::new(k.camelot()).color(theme::key_color(k)).strong());
                        }
                    });
                    row.col(|ui| {
                        if let Some(e) = a.and_then(|a| a.energy) {
                            ui.label(RichText::new(format!("{e}")).color(Color32::from_rgb(255, 120 + (13 * (10 - e as u32)).min(135) as u8, 80)));
                        }
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(theme::fmt_time_short(a.map(|a| a.duration).filter(|d| *d > 0.0).unwrap_or(t.duration))).color(theme::TEXT_DIM));
                    });
                    if !compact {
                        row.col(|ui| {
                            ui.label(RichText::new(&t.genre).color(theme::TEXT_DIM));
                        });
                    }
                    row.col(|ui| {
                        if let Some(st) = sh.job_status.get(&Some(t.id)) {
                            ui.add(egui::ProgressBar::new(st.frac).text(RichText::new(&st.label).size(10.0)).desired_height(14.0));
                        } else if a.is_none_or(|a| a.version == 0) {
                            ui.label(RichText::new("not analysed").color(theme::TEXT_DIM).size(10.5));
                        } else if sh.cue_counts.get(&t.id).copied().unwrap_or(0) > 0 || a.is_some_and(|a| !a.cues.is_empty()) {
                            let n = a.map(|a| a.cues.iter().filter(|c| c.slot.is_some()).count()).unwrap_or(0);
                            ui.label(RichText::new(format!("{n} cues")).color(theme::TEXT_DIM).size(10.5));
                        }
                    });
                    let resp = row.response();
                    if resp.clicked() {
                        let (cmd, shift) = ui_mods(&resp.ctx);
                        if shift {
                            if let Some(last) = self.last_clicked {
                                let a = row_ids.iter().position(|x| *x == last).unwrap_or(0);
                                let b = row_ids.iter().position(|x| *x == t.id).unwrap_or(0);
                                for id in &row_ids[a.min(b)..=a.max(b)] {
                                    self.selected.insert(*id);
                                }
                            }
                        } else if cmd {
                            if !self.selected.remove(&t.id) {
                                self.selected.insert(t.id);
                            }
                        } else {
                            self.selected.clear();
                            self.selected.insert(t.id);
                        }
                        self.last_clicked = Some(t.id);
                    }
                    if resp.double_clicked() {
                        actions.push(LibraryAction::LoadDeck(t.id, usize::MAX));
                    }
                    if resp.drag_started() {
                        let ids = if self.selected.contains(&t.id) { self.selected.iter().copied().collect() } else { vec![t.id] };
                        resp.dnd_set_drag_payload(DraggedTracks(ids));
                    }
                    if resp.dragged() {
                        let n = if self.selected.contains(&t.id) { self.selected.len() } else { 1 };
                        egui::Tooltip::always_open(resp.ctx.clone(), resp.layer_id, egui::Id::new("drag-tip"), egui::PopupAnchor::Pointer).show(|ui| {
                            ui.label(if n > 1 { format!("{n} tracks") } else { t.display_name() });
                        });
                    }
                    resp.context_menu(|ui| {
                        let ids: Vec<TrackId> = if self.selected.contains(&t.id) { self.selected.iter().copied().collect() } else { vec![t.id] };
                        for d in 0..sh.deck_count {
                            if ui.button(format!("Load on deck {}", (b'A' + d as u8) as char)).clicked() {
                                actions.push(LibraryAction::LoadDeck(t.id, d));
                                ui.close();
                            }
                        }
                        if ui.button("Add to studio mix").clicked() {
                            actions.push(LibraryAction::AddToMix(ids.clone()));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Analyse + separate stems").clicked() {
                            actions.push(LibraryAction::Process(ids.clone()));
                            ui.close();
                        }
                        if ui.button("Re-analyse (keep stems)").clicked() {
                            actions.push(LibraryAction::Reanalyze(ids.clone()));
                            ui.close();
                        }
                        if ui.button("Show in Finder").clicked() {
                            actions.push(LibraryAction::Reveal(t.id));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Remove from library").clicked() {
                            actions.push(LibraryAction::Remove(ids));
                            ui.close();
                        }
                    });
                });
            });
    }
}

fn ui_mods(ctx: &egui::Context) -> (bool, bool) {
    ctx.input(|i| (i.modifiers.command, i.modifiers.shift))
}
