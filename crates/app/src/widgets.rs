//! Custom DJ controls: knobs, faders, pads and level meters.

use crate::theme;
use eframe::egui::{self, pos2, vec2, Color32, Rect, Response, Sense, Stroke, StrokeKind, Ui};

/// Rotary knob. `value` in `range`; double-click resets to `default`. Vertical drag adjusts
/// (hold shift for fine control). Returns the response (`changed()` when moved).
pub fn knob(ui: &mut Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>, default: f32, label: &str, color: Color32, size: f32) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(size, size + 13.0), Sense::click_and_drag());
    let (lo, hi) = (*range.start(), *range.end());
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let d = -resp.drag_delta().y / if fine { 600.0 } else { 150.0 };
        *value = (*value + d * (hi - lo)).clamp(lo, hi);
        resp.mark_changed();
    }
    if resp.double_clicked() {
        *value = default;
        resp.mark_changed();
    }
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            *value = (*value + scroll / 400.0 * (hi - lo)).clamp(lo, hi);
            resp.mark_changed();
        }
    }
    let p = ui.painter();
    let c = pos2(rect.center().x, rect.top() + size / 2.0);
    let r = size / 2.0 - 2.0;
    p.circle_filled(c, r, theme::PANEL_HI);
    p.circle_stroke(c, r, Stroke::new(1.0, theme::LINE));
    let t = (*value - lo) / (hi - lo);
    let a0 = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let td = (default - lo) / (hi - lo);
    // arc from default to value
    let (from, to) = if t < td { (t, td) } else { (td, t) };
    let steps = 24;
    let mut pts = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let a = a0 + sweep * (from + (to - from) * i as f32 / steps as f32);
        pts.push(c + vec2(a.cos(), a.sin()) * (r - 2.5));
    }
    if to - from > 0.002 {
        p.add(egui::Shape::line(pts, Stroke::new(3.0, color)));
    }
    let a = a0 + sweep * t;
    p.line_segment([c + vec2(a.cos(), a.sin()) * (r * 0.25), c + vec2(a.cos(), a.sin()) * (r - 4.0)], Stroke::new(2.0, theme::TEXT));
    p.text(pos2(rect.center().x, rect.bottom() - 6.0), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(10.0), theme::TEXT_DIM);
    resp
}

/// Vertical fader. `value` 0..1 (top = 1). Double-click resets.
pub fn vfader(ui: &mut Ui, value: &mut f32, default: f32, size: egui::Vec2, color: Color32) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let track = Rect::from_center_size(rect.center(), vec2(4.0, rect.height() - 14.0));
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if resp.dragged() && !resp.drag_started() {
                let fine = ui.input(|i| i.modifiers.shift);
                *value = (*value - resp.drag_delta().y / track.height() * if fine { 0.2 } else { 1.0 }).clamp(0.0, 1.0);
            } else if resp.clicked() {
                *value = (1.0 - (pos.y - track.top()) / track.height()).clamp(0.0, 1.0);
            }
            resp.mark_changed();
        }
    }
    if resp.double_clicked() {
        *value = default;
        resp.mark_changed();
    }
    let p = ui.painter();
    p.rect_filled(track, 2.0, theme::BG);
    let y = track.bottom() - *value * track.height();
    p.rect_filled(Rect::from_min_max(pos2(track.left(), y), track.max), 2.0, color.linear_multiply(0.6));
    let cap = Rect::from_center_size(pos2(rect.center().x, y), vec2(rect.width() - 4.0, 12.0));
    p.rect_filled(cap, 3.0, Color32::from_rgb(70, 75, 86));
    p.line_segment([pos2(cap.left() + 3.0, y), pos2(cap.right() - 3.0, y)], Stroke::new(1.5, theme::TEXT));
    resp
}

/// Horizontal fader (crossfader / pitch). `value` in -1..1 when `bipolar`, else 0..1.
pub fn hfader(ui: &mut Ui, value: &mut f32, bipolar: bool, size: egui::Vec2, color: Color32) -> Response {
    hfader_with_default(ui, value, bipolar, if bipolar { 0.0 } else { 0.5 }, None, size, color)
}

/// Horizontal fader with a double-click `default` and an optional tick mark (e.g. 0 dB).
pub fn hfader_with_default(ui: &mut Ui, value: &mut f32, bipolar: bool, default: f32, tick: Option<f32>, size: egui::Vec2, color: Color32) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let track = Rect::from_center_size(rect.center(), vec2(rect.width() - 16.0, 4.0));
    let (lo, hi) = if bipolar { (-1.0, 1.0) } else { (0.0, 1.0) };
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        *value = (*value + resp.drag_delta().x / track.width() * (hi - lo) * if fine { 0.2 } else { 1.0 }).clamp(lo, hi);
        resp.mark_changed();
    }
    if resp.double_clicked() {
        *value = default;
        resp.mark_changed();
    }
    let p = ui.painter();
    p.rect_filled(track, 2.0, theme::BG);
    if let Some(tk) = tick {
        let tx = track.left() + (tk - lo) / (hi - lo) * track.width();
        p.line_segment([pos2(tx, track.top() - 5.0), pos2(tx, track.bottom() + 5.0)], Stroke::new(1.0, theme::TEXT_DIM));
    }
    let t = (*value - lo) / (hi - lo);
    let x = track.left() + t * track.width();
    if bipolar {
        p.line_segment([pos2(track.center().x, track.top() - 4.0), pos2(track.center().x, track.bottom() + 4.0)], Stroke::new(1.0, theme::LINE));
    }
    let cap = Rect::from_center_size(pos2(x, rect.center().y), vec2(12.0, rect.height() - 4.0));
    p.rect_filled(cap, 3.0, Color32::from_rgb(70, 75, 86));
    p.line_segment([pos2(x, cap.top() + 3.0), pos2(x, cap.bottom() - 3.0)], Stroke::new(1.5, color));
    resp
}

/// Square performance pad. `lit` draws it filled with `color`.
pub fn pad(ui: &mut Ui, label: &str, sub: &str, color: Color32, lit: bool, size: egui::Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let p = ui.painter();
    let fill = if lit {
        if resp.is_pointer_button_down_on() { color } else { color.linear_multiply(0.75) }
    } else if resp.hovered() {
        theme::PANEL_HI.gamma_multiply(1.3)
    } else {
        theme::PANEL_HI
    };
    p.rect_filled(rect, 5.0, fill);
    p.rect_stroke(rect, 5.0, Stroke::new(1.0, if lit { color } else { theme::LINE }), StrokeKind::Inside);
    let tc = if lit { Color32::BLACK } else { theme::TEXT };
    if sub.is_empty() {
        p.text(rect.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(12.0), tc);
    } else {
        p.text(rect.center() - vec2(0.0, 6.0), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(12.0), tc);
        p.text(rect.center() + vec2(0.0, 8.0), egui::Align2::CENTER_CENTER, sub, egui::FontId::proportional(9.5), if lit { Color32::from_black_alpha(180) } else { theme::TEXT_DIM });
    }
    resp
}

/// Small toggle button with an "LED" look.
pub fn toggle(ui: &mut Ui, on: bool, label: &str, color: Color32, size: egui::Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, 4.0, if on { color.linear_multiply(0.85) } else if resp.hovered() { Color32::from_rgb(46, 50, 60) } else { theme::PANEL_HI });
    p.rect_stroke(rect, 4.0, Stroke::new(1.0, if on { color } else { theme::LINE }), StrokeKind::Inside);
    p.text(rect.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(11.5), if on { Color32::BLACK } else { theme::TEXT });
    resp
}

/// Stereo VU meter (values are linear peak 0..~1.2).
pub fn vu(ui: &mut Ui, l: f32, r: f32, size: egui::Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, theme::BG);
    let w = (rect.width() - 2.0) / 2.0;
    for (i, v) in [l, r].into_iter().enumerate() {
        let db = 20.0 * v.max(1e-5).log10();
        let t = ((db + 48.0) / 51.0).clamp(0.0, 1.0);
        let col = Rect::from_min_size(pos2(rect.left() + i as f32 * (w + 2.0), rect.top()), vec2(w, rect.height()));
        let segs = 24;
        let lit = (t * segs as f32).round() as i32;
        for s in 0..segs {
            let h = col.height() / segs as f32;
            let y = col.bottom() - (s + 1) as f32 * h;
            let c = if s >= 22 { Color32::from_rgb(255, 60, 60) } else if s >= 18 { theme::WARN } else { theme::OK };
            let seg = Rect::from_min_size(pos2(col.left(), y + 0.5), vec2(col.width(), h - 1.0));
            p.rect_filled(seg, 0.0, if s < lit { c } else { c.linear_multiply(0.12) });
        }
    }
}
