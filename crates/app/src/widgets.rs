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

/// Small on/off toggle. All toggles share one look: graphite when on, with the colour as the
/// label, outline and an "LED" bar along the bottom edge (no large coloured fills).
pub fn toggle(ui: &mut Ui, on: bool, label: &str, color: Color32, size: egui::Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    let fill = if on { theme::SELECT } else if resp.hovered() { Color32::from_rgb(46, 50, 60) } else { theme::PANEL_HI };
    p.rect_filled(rect, 4.0, fill);
    p.rect_stroke(rect, 4.0, Stroke::new(1.0, if on { color.linear_multiply(0.8) } else { theme::LINE }), StrokeKind::Inside);
    if on {
        let led = egui::Rect::from_min_max(egui::pos2(rect.left() + 4.0, rect.bottom() - 3.0), egui::pos2(rect.right() - 4.0, rect.bottom() - 1.5));
        p.rect_filled(led, 1.0, color);
    }
    let tc = if on { theme::blend(color, Color32::WHITE, 0.3) } else { theme::TEXT };
    p.text(rect.center() - egui::vec2(0.0, if on { 0.5 } else { 0.0 }), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(11.5), tc);
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

/// Round play / pause button with a drawn (optically centred) symbol.
pub fn play_button(ui: &mut Ui, playing: bool, enabled: bool, d: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(d, d), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    let c = rect.center();
    let fill = if !enabled { theme::LINE } else if resp.hovered() { theme::blend(theme::accent(), Color32::WHITE, 0.3) } else { theme::accent() };
    p.circle_filled(c, d * 0.5, fill);
    let ink = Color32::from_gray(16);
    let s = d * 0.36;
    if playing {
        let (w, h) = (s * 0.3, s * 0.95);
        for dx in [-s * 0.27, s * 0.27] {
            p.rect_filled(egui::Rect::from_center_size(c + egui::vec2(dx, 0.0), egui::vec2(w, h)), 1.5, ink);
        }
    } else {
        // the triangle's centroid sits on the circle's centre
        let h = s * 1.0;
        let w = h * 0.9;
        let pts = vec![c + egui::vec2(-w / 3.0, -h / 2.0), c + egui::vec2(2.0 * w / 3.0, 0.0), c + egui::vec2(-w / 3.0, h / 2.0)];
        p.add(egui::Shape::convex_polygon(pts, ink, Stroke::NONE));
    }
    resp
}

/// The ClubCraft mark: "CC" as two decks (two C platters around their spindles over a
/// crossfader), `size` px wide. Same geometry as the app icon (`tools/make_icon.py`).
pub fn logo_mark(ui: &mut Ui, size: f32, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size * 0.62), Sense::hover());
    let p = ui.painter();
    // icon units: the mark spans x 0.195..0.805 and y 0.29..0.76 of the icon
    let k = size / 0.61;
    let at = |x: f32, y: f32| rect.min + vec2((x - 0.195) * k, (y - 0.29) * k);
    let w = 0.058 * k;
    for cx in [0.345, 0.655] {
        let c = at(cx, 0.44);
        let r = (0.15 - 0.029) * k;
        let (gap, n) = (78f32, 40);
        let (a0, a1) = ((gap / 2.0).to_radians(), (360.0 - gap / 2.0).to_radians());
        let pts: Vec<egui::Pos2> = (0..=n).map(|i| a0 + (a1 - a0) * i as f32 / n as f32).map(|a| c + vec2(a.cos(), a.sin()) * r).collect();
        for q in [pts[0], pts[n]] {
            p.circle_filled(q, w / 2.0, color);
        }
        p.add(egui::Shape::line(pts, Stroke::new(w, color)));
        p.circle_filled(c, 0.026 * k, color);
    }
    let fw = (0.018 * k).max(1.0);
    p.line_segment([at(0.28, 0.715), at(0.72, 0.715)], Stroke::new(fw, color.gamma_multiply(0.45)));
    p.rect_filled(Rect::from_center_size(at(0.5, 0.715), vec2(0.05 * k, 0.085 * k)), 0.012 * k, color);
    resp
}

/// A small symbol for a stem, drawn into `r` (about 14 × 12 px): a drum, a bass guitar, a note
/// or a microphone.
pub fn stem_icon(p: &egui::Painter, r: Rect, stem: clubcraft_core::StemKind, color: Color32) {
    use clubcraft_core::StemKind::*;
    let s = r.height().min(r.width()) / 12.0;
    let c = r.center();
    let at = |x: f32, y: f32| c + vec2(x * s, y * s);
    let line = Stroke::new((1.3 * s).max(1.0), color);
    match stem {
        Drums => {
            // snare: shell with its top skin (a lighter ellipse), two crossed sticks above
            p.rect_filled(Rect::from_min_max(at(-5.5, 0.5), at(5.5, 5.0)), egui::CornerRadius { nw: 0, ne: 0, sw: (2.0 * s) as u8, se: (2.0 * s) as u8 }, color);
            p.add(egui::Shape::ellipse_filled(at(0.0, 0.5), vec2(5.5 * s, 2.0 * s), color));
            p.add(egui::Shape::ellipse_stroke(at(0.0, 0.5), vec2(4.3 * s, 1.1 * s), Stroke::new((0.9 * s).max(0.8), color.gamma_multiply(0.45))));
            p.line_segment([at(-5.0, -6.0), at(2.5, -1.4)], line);
            p.line_segment([at(5.0, -6.0), at(-2.5, -1.4)], line);
        }
        Bass => {
            // bass guitar: body low left, long neck to the upper right, headstock
            p.circle_filled(at(-2.6, 3.0), 2.9 * s, color);
            p.circle_filled(at(-0.6, 0.9), 2.0 * s, color);
            p.line_segment([at(-0.5, 1.0), at(4.4, -4.2)], Stroke::new((1.6 * s).max(1.0), color));
            p.line_segment([at(4.0, -4.6), at(5.6, -6.0)], Stroke::new((2.4 * s).max(1.2), color));
        }
        Other => {
            // beamed eighth notes
            p.add(egui::Shape::ellipse_filled(at(-3.3, 3.6), vec2(2.1 * s, 1.6 * s), color));
            p.add(egui::Shape::ellipse_filled(at(3.0, 2.4), vec2(2.1 * s, 1.6 * s), color));
            p.line_segment([at(-1.4, 3.4), at(-1.4, -4.6)], line);
            p.line_segment([at(4.9, 2.2), at(4.9, -5.8)], line);
            p.line_segment([at(-1.4, -4.6), at(4.9, -5.8)], Stroke::new((2.0 * s).max(1.2), color));
        }
        Vocals => {
            // microphone: capsule, holder and stand
            p.rect_filled(Rect::from_min_max(at(-2.2, -6.0), at(2.2, 0.8)), 2.2 * s, color);
            let holder: Vec<egui::Pos2> = (0..=12).map(|i| {
                let a = std::f32::consts::PI * i as f32 / 12.0;
                at(-3.6 * a.cos(), -1.4 + 3.6 * a.sin())
            }).collect();
            p.add(egui::Shape::line(holder, line));
            p.line_segment([at(0.0, 2.2), at(0.0, 5.0)], line);
            p.line_segment([at(-2.4, 5.4), at(2.4, 5.4)], line);
        }
    }
}

/// A padlock, closed or open, drawn into `r`.
pub fn lock_icon(p: &egui::Painter, r: Rect, locked: bool, color: Color32) {
    let w = r.width();
    let body = Rect::from_min_max(pos2(r.left() + w * 0.12, r.top() + r.height() * 0.45), pos2(r.right() - w * 0.12, r.bottom()));
    p.rect_filled(body, w * 0.12, color);
    // shackle: a half ring above the body (shifted up and open on the right when unlocked)
    let (cx, top, rad) = (body.center().x, body.top(), body.width() * 0.32);
    let lift = if locked { 0.0 } else { r.height() * 0.14 };
    let n = 14;
    let pts: Vec<egui::Pos2> = (0..=n)
        .filter_map(|i| {
            let a = std::f32::consts::PI * i as f32 / n as f32;
            if !locked && i < 3 {
                return None; // open on the right
            }
            Some(pos2(cx + rad * a.cos(), top - lift - rad * a.sin() * 1.15))
        })
        .collect();
    let stroke = Stroke::new((w * 0.14).max(1.2), color);
    p.add(egui::Shape::line(pts, stroke));
    p.line_segment([pos2(cx - rad, top - lift), pos2(cx - rad, top + 1.0)], stroke);
    if locked {
        p.line_segment([pos2(cx + rad, top), pos2(cx + rad, top + 1.0)], stroke);
    }
}
