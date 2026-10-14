//! Automation curve editor, drawn over a clip like DJ.Studio's: press on the line to add a point
//! and drag it, drag a point to move it (over a neighbour to delete that one), double-click or
//! right-click a point to remove it. Times snap to the beat grid (hold Shift for free placement),
//! values snap to 0, 100 % and (for EQ / filter) the centre.

use eframe::egui::{self, pos2, vec2, Color32, CursorIcon, Id, Pos2, Rect, Sense, Stroke, Ui};

/// How to map and draw one curve.
pub struct Curve<'a> {
    /// Plot area: value 0 at the bottom, 1 at the top.
    pub rect: Rect,
    /// Domain of the curve (e.g. 0..beats of a transition).
    pub x0: f32,
    pub x1: f32,
    pub to_x: &'a dyn Fn(f32) -> f32,
    pub from_x: &'a dyn Fn(f32) -> f32,
    /// Snap a domain value (`free` = Shift held).
    pub snap: &'a dyn Fn(f32, bool) -> f32,
    /// Value without automation.
    pub neutral: f32,
    /// EQ / filter: 0.5 is "no change" (drawn as a centre line, snaps there).
    pub centered: bool,
    pub color: Color32,
    pub editable: bool,
    /// Formats a value for the tooltip.
    pub fmt: &'a dyn Fn(f32) -> String,
    /// Formats a domain position for the tooltip.
    pub fmt_x: &'a dyn Fn(f32) -> String,
}

#[derive(Clone, Copy)]
struct Active(usize);

const POINT_R: f32 = 3.5;
const HIT: f32 = 8.0;

/// Draw `points` and edit them. Returns true when they changed.
pub fn curve_ui(ui: &mut Ui, id: Id, c: &Curve, points: &mut Vec<(f32, f32)>) -> bool {
    let p = ui.painter().with_clip_rect(c.rect.expand(POINT_R + 1.0).intersect(ui.clip_rect()));
    let to_y = |v: f32| c.rect.bottom() - v.clamp(0.0, 1.0) * c.rect.height();
    let from_y = |y: f32| ((c.rect.bottom() - y) / c.rect.height()).clamp(0.0, 1.0);
    let shown: Vec<(f32, f32)> = if points.is_empty() { vec![(c.x0, c.neutral), (c.x1, c.neutral)] } else { points.clone() };
    let value_at = |x: f32| clubcraft_core::Lane { side: clubcraft_core::Side::Out, param: clubcraft_core::Param::Volume, points: shown.clone() }.value_at(x);
    let screen: Vec<Pos2> = line_points(&shown, c, &to_y);

    // ---- interaction state
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let active = ui.data(|d| d.get_temp::<Active>(id)).map(|a| a.0);
    let near_point = pointer.and_then(|q| {
        shown.iter().enumerate().map(|(k, pt)| (k, pos2((c.to_x)(pt.0), to_y(pt.1)).distance(q))).filter(|(_, d)| *d <= HIT).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(k, _)| k)
    });
    let near_line = pointer.is_some_and(|q| c.rect.expand2(vec2(0.0, 4.0)).contains(q) && {
        let x = (c.from_x)(q.x).clamp(c.x0, c.x1);
        (to_y(value_at(x)) - q.y).abs() <= HIT
    });
    let mut changed = false;
    let engaged = c.editable && (active.is_some() || near_point.is_some() || near_line);
    if engaged {
        // a small hit area under the pointer takes the press before the clip below it
        let at = pointer.unwrap_or(c.rect.center());
        let r = ui.interact(Rect::from_center_size(at, vec2(2.0 * HIT + 4.0, 2.0 * HIT + 4.0)), id, Sense::click_and_drag());
        let fine = ui.input(|i| i.modifiers.shift);
        ui.ctx().set_cursor_icon(if active.is_some() { CursorIcon::Grabbing } else if near_point.is_some() { CursorIcon::Grab } else { CursorIcon::Crosshair });
        if r.drag_started() {
            if points.is_empty() {
                *points = shown.clone();
            }
            let k = match near_point {
                Some(k) => k,
                None => {
                    let q = r.interact_pointer_pos().unwrap_or(at);
                    let x = (c.snap)((c.from_x)(q.x), fine).clamp(c.x0, c.x1);
                    let v = value_at(x);
                    let k = points.iter().position(|pt| pt.0 > x).unwrap_or(points.len());
                    points.insert(k, (x, v));
                    changed = true;
                    k
                }
            };
            ui.data_mut(|d| d.insert_temp(id, Active(k)));
        }
        if r.dragged() {
            if let (Some(k), Some(q)) = (ui.data(|d| d.get_temp::<Active>(id)).map(|a| a.0), r.interact_pointer_pos()) {
                if k < points.len() {
                    changed |= move_point(points, k, q, c, fine, &from_y).map(|nk| ui.data_mut(|d| d.insert_temp(id, Active(nk)))).is_some();
                }
            }
        }
        if r.drag_stopped() {
            ui.data_mut(|d| d.remove::<Active>(id));
        }
        if (r.double_clicked() || r.secondary_clicked()) && near_point.is_some() {
            if points.is_empty() {
                *points = shown.clone();
            }
            let k = near_point.unwrap();
            if k < points.len() && points.len() > 1 {
                points.remove(k);
                changed = true;
            }
        }
    } else if active.is_some() {
        ui.data_mut(|d| d.remove::<Active>(id));
    }

    // ---- draw
    let base = if c.centered { to_y(0.5) } else { c.rect.bottom() };
    if c.centered {
        p.line_segment([pos2(c.rect.left(), base), pos2(c.rect.right(), base)], Stroke::new(1.0, Color32::from_white_alpha(28)));
    }
    let shown: Vec<(f32, f32)> = if points.is_empty() { vec![(c.x0, c.neutral), (c.x1, c.neutral)] } else { points.clone() };
    let screen = if changed { line_points(&shown, c, &to_y) } else { screen };
    for w in screen.windows(2) {
        if w[1].x > w[0].x {
            p.add(egui::Shape::convex_polygon(vec![w[0], w[1], pos2(w[1].x, base), pos2(w[0].x, base)], c.color.gamma_multiply(0.13), Stroke::NONE));
        }
    }
    let hot = engaged || ui.rect_contains_pointer(c.rect);
    p.add(egui::Shape::line(screen, Stroke::new(if hot { 2.2 } else { 1.6 }, c.color)));
    if c.editable {
        let act = ui.data(|d| d.get_temp::<Active>(id)).map(|a| a.0);
        for (k, pt) in shown.iter().enumerate() {
            let q = pos2((c.to_x)(pt.0), to_y(pt.1));
            let big = act == Some(k) || near_point == Some(k);
            p.circle_filled(q, if big { POINT_R + 1.5 } else { POINT_R }, if big { Color32::WHITE } else { c.color });
            p.circle_stroke(q, if big { POINT_R + 1.5 } else { POINT_R }, Stroke::new(1.0, Color32::from_black_alpha(160)));
        }
        if let (true, Some(q)) = (engaged, pointer) {
            let (x, v) = match act.or(near_point).and_then(|k| shown.get(k)) {
                Some(pt) => *pt,
                None => {
                    let x = (c.from_x)(q.x).clamp(c.x0, c.x1);
                    (x, value_at(x))
                }
            };
            let txt = format!("{} · {}", (c.fmt_x)(x), (c.fmt)(v));
            let galley = p.layout_no_wrap(txt, egui::FontId::proportional(11.0), Color32::WHITE);
            let at = pos2((q.x + 12.0).min(c.rect.right() - galley.size().x - 6.0), (q.y - 22.0).max(c.rect.top()));
            let bg = Rect::from_min_size(at - vec2(4.0, 2.0), galley.size() + vec2(8.0, 4.0));
            p.rect_filled(bg, 4.0, Color32::from_black_alpha(200));
            p.galley(at, galley, Color32::WHITE);
        }
    }
    changed
}

fn line_points(pts: &[(f32, f32)], c: &Curve, to_y: &dyn Fn(f32) -> f32) -> Vec<Pos2> {
    let mut v = Vec::with_capacity(pts.len() + 2);
    if let Some(first) = pts.first() {
        if first.0 > c.x0 {
            v.push(pos2((c.to_x)(c.x0), to_y(first.1)));
        }
    }
    v.extend(pts.iter().map(|pt| pos2((c.to_x)(pt.0), to_y(pt.1))));
    if let Some(last) = pts.last() {
        if last.0 < c.x1 {
            v.push(pos2((c.to_x)(c.x1), to_y(last.1)));
        }
    }
    v
}

/// Move point `k` to the pointer; points it passes over are removed (DJ.Studio's drag-to-delete).
/// Returns the point's new index.
fn move_point(points: &mut Vec<(f32, f32)>, k: usize, q: Pos2, c: &Curve, fine: bool, from_y: &dyn Fn(f32) -> f32) -> Option<usize> {
    let mut v = from_y(q.y);
    let snaps: &[f32] = if c.centered { &[0.0, 0.5, 1.0] } else { &[0.0, 1.0] };
    if let Some(s) = snaps.iter().find(|s| (v - **s).abs() < 0.03) {
        v = *s;
    }
    v = (v * 200.0).round() / 200.0;
    let (ox, _) = points[k];
    // the first and last point stay at the window edges
    let pinned = (ox - c.x0).abs() < 1e-4 || (ox - c.x1).abs() < 1e-4;
    let x = if pinned { ox } else { (c.snap)((c.from_x)(q.x), fine).clamp(c.x0, c.x1) };
    if (x - ox).abs() < 1e-6 && (v - points[k].1).abs() < 1e-6 {
        return None;
    }
    let (lo, hi) = (ox.min(x), ox.max(x));
    let mut i = 0;
    let mut nk = k;
    points.retain(|pt| {
        let keep = i == k || !(pt.0 > lo && pt.0 < hi) || (pt.0 - c.x0).abs() < 1e-4 || (pt.0 - c.x1).abs() < 1e-4;
        if !keep && i < k {
            nk -= 1;
        }
        i += 1;
        keep
    });
    points[nk] = (x, v);
    // keep the order for equal times (steps)
    while nk > 0 && points[nk - 1].0 > x {
        points.swap(nk - 1, nk);
        nk -= 1;
    }
    while nk + 1 < points.len() && points[nk + 1].0 < x {
        points.swap(nk + 1, nk);
        nk += 1;
    }
    Some(nk)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_curve(f: impl FnOnce(&Curve)) {
        // 100 px per unit, 0..8 on x, value 0..1 over 0..100 px (y down)
        let to_x = |b: f32| b * 100.0;
        let from_x = |x: f32| x / 100.0;
        let snap = |b: f32, free: bool| if free { b } else { b.round() };
        let fmt = |v: f32| format!("{v}");
        let c = Curve { rect: Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 100.0)), x0: 0.0, x1: 8.0, to_x: &to_x, from_x: &from_x, snap: &snap, neutral: 1.0, centered: false, color: Color32::WHITE, editable: true, fmt: &fmt, fmt_x: &fmt };
        f(&c);
    }
    fn from_y(y: f32) -> f32 {
        ((100.0 - y) / 100.0).clamp(0.0, 1.0)
    }

    #[test]
    fn dragging_moves_snaps_and_deletes_passed_points() {
        with_curve(|c| {
            let mut pts = vec![(0.0, 1.0), (2.0, 1.0), (4.0, 0.5), (6.0, 0.0), (8.0, 0.0)];
            // move the point at 2 to x 5.2 (snaps to 5), half height: the point at 4 is passed and goes
            let k = move_point(&mut pts, 1, pos2(520.0, 50.0), c, false, &from_y).unwrap();
            assert_eq!(pts, vec![(0.0, 1.0), (5.0, 0.5), (6.0, 0.0), (8.0, 0.0)]);
            assert_eq!(k, 1);
            // values snap to 0 / 100 % near the edges
            move_point(&mut pts, 1, pos2(500.0, 98.0), c, false, &from_y);
            assert_eq!(pts[1], (5.0, 0.0));
            // the end points only move vertically
            move_point(&mut pts, 0, pos2(300.0, 30.0), c, false, &from_y);
            assert_eq!(pts[0], (0.0, 0.7));
            // Shift: free placement
            move_point(&mut pts, 1, pos2(533.0, 50.0), c, true, &from_y);
            assert!((pts[1].0 - 5.33).abs() < 1e-4);
        });
    }
}
