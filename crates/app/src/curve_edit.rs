//! Automation curve editor, drawn over a clip.
//!
//! - Drag a point to move it (it stops at its neighbours; the first and last stay at the edges).
//!   Double-click or right-click it to remove it.
//! - Drag the line anywhere else to add a point there and move it.
//! - A hard switch (the vertical line of two points at one time) slides sideways as one: drag the
//!   vertical line or either of its points. The caller can move the other track's switch at the
//!   same time along (a swap), unless ⌥ is held. Double-click a slope to turn it into a hard
//!   switch.
//! - ⌘-drag draws like a pencil: flat steps on the grid, with hard switches between them.
//! - The round handle in the middle of a slope bends it (⌥-drag on the slope does too);
//!   double-click the handle to make it straight again.
//!
//! Times snap to the grid (hold Shift for free placement), values snap to 0, 100 % and (for EQ /
//! filter) the centre.

use clubcraft_core::{bend_through, Lane, SWITCH_GAP};
use eframe::egui::{self, pos2, vec2, Color32, CursorIcon, Id, Modifiers, Pos2, Rect, Sense, Stroke, Ui};

/// How to map and draw one curve.
pub struct Curve<'a> {
    /// Plot area: value 0 at the bottom, 1 at the top.
    pub rect: Rect,
    /// Domain of the curve (e.g. 0..beats of a transition).
    pub x0: f32,
    pub x1: f32,
    pub to_x: &'a dyn Fn(f32) -> f32,
    pub from_x: &'a dyn Fn(f32) -> f32,
    /// Grid of the domain: times snap to it and the pencil draws one step per cell.
    pub grid: f32,
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

impl Curve<'_> {
    fn snap(&self, x: f32, free: bool) -> f32 {
        let x = if free || self.grid <= 0.0 { x } else { (x / self.grid).round() * self.grid };
        x.clamp(self.x0, self.x1)
    }

    fn to_y(&self, v: f32) -> f32 {
        self.rect.bottom() - v.clamp(0.0, 1.0) * self.rect.height()
    }

    fn at(&self, pt: (f32, f32)) -> Pos2 {
        pos2((self.to_x)(pt.0), self.to_y(pt.1))
    }

    /// The value at screen height `y`, snapped to the edges (and the centre).
    fn value_of_y(&self, y: f32) -> f32 {
        let v = ((self.rect.bottom() - y) / self.rect.height()).clamp(0.0, 1.0);
        let snaps: &[f32] = if self.centered { &[0.0, 0.5, 1.0] } else { &[0.0, 1.0] };
        match snaps.iter().find(|s| (v - **s).abs() < 0.03) {
            Some(s) => *s,
            None => (v * 200.0).round() / 200.0,
        }
    }

    /// Size of a pencil cell.
    fn cell(&self) -> f32 {
        if self.grid > 0.0 {
            self.grid
        } else {
            (self.x1 - self.x0).max(1e-3) / 64.0
        }
    }
}

/// What the pointer is over.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Target {
    Point(usize),
    /// The vertical line between points `i` and `i + 1` (same time).
    Step(usize),
    /// The bend handle of the segment from point `i`.
    Bend(usize),
    /// The line, over the segment from point `i` (none before the first or after the last point).
    Line(Option<usize>),
    /// ⌘ held: the pencil.
    Paint,
}

/// What is being dragged.
#[derive(Clone, Copy)]
enum Active {
    /// `step`: the point belonged to a hard switch when it was grabbed, so the pair moves
    /// sideways together.
    Point { k: usize, step: bool },
    Step(usize),
    Bend(usize),
    /// The pencil, last at (time, value).
    Paint { x: f32, v: f32 },
}

const POINT_R: f32 = 3.5;
const HIT: f32 = 9.0;
const EPS: f32 = 1e-4;

/// What an edit did.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Edit {
    pub changed: bool,
    /// A hard switch slid from one time to another (of its later point). The other track's
    /// switch at that time goes along, so a swap stays a swap. None while ⌥ is held.
    pub slid: Option<(f32, f32)>,
}

/// Draw `lane` and edit it.
pub fn curve_ui(ui: &mut Ui, id: Id, c: &Curve, lane: &mut Lane) -> Edit {
    let p = ui.painter().with_clip_rect(c.rect.expand(POINT_R + 2.0).intersect(ui.clip_rect()));
    let mut work = normalized(lane, c);
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let mods = ui.input(|i| i.modifiers);
    let active = ui.data(|d| d.get_temp::<Active>(id));
    let hover = if c.editable { pointer.and_then(|q| target_at(&work, c, q, mods)) } else { None };
    let mut changed = false;
    let mut slid = None;
    // a press keeps the editor until it is let go: the pointer may leave a thin switch line
    // before the press counts as a drag, and the clip below must not get it then
    let held_id = id.with("held");
    let held = ui.data(|d| d.get_temp::<bool>(held_id).unwrap_or(false)) && ui.input(|i| i.pointer.primary_down());
    let engaged = c.editable && (active.is_some() || hover.is_some() || held);
    if engaged {
        // a small hit area under the pointer takes the press before the clip below it
        let at = pointer.unwrap_or(c.rect.center());
        let r = ui.interact(Rect::from_center_size(at, vec2(2.0 * HIT + 4.0, 2.0 * HIT + 4.0)), id, Sense::click_and_drag());
        ui.ctx().set_cursor_icon(cursor(active, hover));
        let free = mods.shift;
        let q = r.interact_pointer_pos().unwrap_or(at);
        if r.is_pointer_button_down_on() {
            ui.data_mut(|d| d.insert_temp(held_id, true));
        } else {
            ui.data_mut(|d| d.remove::<bool>(held_id));
        }
        if r.drag_started() {
            // what was under the press, not where the pointer is once it counts as a drag
            let q0 = ui.input(|i| i.pointer.press_origin()).unwrap_or(q);
            let a = match target_at(&work, c, q0, mods).or(hover) {
                Some(Target::Point(k)) => Some(Active::Point { k, step: twin(&work, k).is_some() }),
                Some(Target::Step(i)) => Some(Active::Step(i)),
                Some(Target::Bend(i)) => Some(Active::Bend(i)),
                Some(Target::Line(_)) => {
                    let x = c.snap((c.from_x)(q0.x), free);
                    let k = work.points.iter().position(|pt| pt.0 > x).unwrap_or(work.points.len());
                    work.insert_point(k, (x, work.value_at(x)));
                    changed = true;
                    Some(Active::Point { k, step: false })
                }
                Some(Target::Paint) => {
                    let (x, v) = ((c.from_x)(q0.x).clamp(c.x0, c.x1), c.value_of_y(q0.y));
                    changed |= paint_stroke(&mut work, c, (x, v), (x, v));
                    Some(Active::Paint { x, v })
                }
                None => None,
            };
            if let Some(a) = a {
                ui.data_mut(|d| d.insert_temp(id, a));
            }
        } else if r.dragged() {
            let held = ui.data(|d| d.get_temp::<Active>(id));
            // the later point of a switch being slid, and where it was
            let later = match held {
                Some(Active::Point { k, step: true }) => twin(&work, k).map(|t| k.max(t)),
                Some(Active::Step(i)) => Some(i + 1),
                _ => None,
            };
            let was = later.and_then(|j| work.points.get(j)).map(|p| p.0);
            match held {
                Some(Active::Point { k, step }) if k < work.points.len() => changed |= move_point(&mut work, k, step, q, c, free),
                Some(Active::Step(i)) if i + 1 < work.points.len() => changed |= move_step(&mut work, i, q.x, c, free),
                Some(Active::Bend(i)) if i + 1 < work.points.len() => {
                    let (a, b) = (work.points[i], work.points[i + 1]);
                    let bend = bend_through(a.1, b.1, c.value_of_y(q.y));
                    changed |= (bend - work.bend(i)).abs() > 1e-4;
                    work.set_bend(i, bend);
                }
                Some(Active::Paint { x: px, v: pv }) => {
                    let (x, v) = ((c.from_x)(q.x).clamp(c.x0, c.x1), c.value_of_y(q.y));
                    changed |= paint_stroke(&mut work, c, (px, pv), (x, v));
                    ui.data_mut(|d| d.insert_temp(id, Active::Paint { x, v }));
                }
                _ => {}
            }
            if let (Some(a), Some(b)) = (was, later.and_then(|j| work.points.get(j)).map(|p| p.0)) {
                if (a - b).abs() > 1e-6 && !mods.alt {
                    slid = Some((a, b));
                }
            }
        }
        if r.drag_stopped() {
            ui.data_mut(|d| d.remove::<Active>(id));
            // a point dropped onto its neighbour, or a stroke over its own value, leaves points
            // that change nothing
            let before = work.clone();
            work.tidy();
            changed |= work != before;
        }
        if r.clicked() && hover == Some(Target::Paint) {
            let (x, v) = ((c.from_x)(q.x).clamp(c.x0, c.x1), c.value_of_y(q.y));
            changed |= paint_stroke(&mut work, c, (x, v), (x, v));
        }
        let remove = r.double_clicked() || r.secondary_clicked();
        match hover {
            Some(Target::Point(k)) if remove && work.points.len() > 1 => {
                work.remove_point(k);
                changed = true;
            }
            Some(Target::Bend(i)) if r.double_clicked() => {
                changed |= work.bend(i) != 0.0;
                work.set_bend(i, 0.0);
            }
            Some(Target::Line(Some(i))) if r.double_clicked() => changed |= make_switch(&mut work, i, c.snap((c.from_x)(q.x), free)),
            _ => {}
        }
    } else {
        ui.data_mut(|d| {
            d.remove::<Active>(id);
            d.remove::<bool>(held_id);
        });
    }
    if changed {
        lane.points.clone_from(&work.points);
        lane.bends.clone_from(&work.bends);
    }

    // ---- draw
    let base = if c.centered { c.to_y(0.5) } else { c.rect.bottom() };
    if c.centered {
        p.line_segment([pos2(c.rect.left(), base), pos2(c.rect.right(), base)], Stroke::new(1.0, Color32::from_white_alpha(28)));
    }
    let screen = line_points(&work, c);
    for w in screen.windows(2) {
        if w[1].x > w[0].x {
            p.add(egui::Shape::convex_polygon(vec![w[0], w[1], pos2(w[1].x, base), pos2(w[0].x, base)], c.color.gamma_multiply(0.13), Stroke::NONE));
        }
    }
    let hot = engaged || ui.rect_contains_pointer(c.rect);
    p.add(egui::Shape::line(screen, Stroke::new(if hot { 2.2 } else { 1.6 }, c.color)));
    if !c.editable {
        return Edit { changed, slid };
    }
    let active = ui.data(|d| d.get_temp::<Active>(id));
    // what is under the pointer or held, highlighted
    let lit = match active {
        Some(Active::Point { k, .. }) => Some(Target::Point(k)),
        Some(Active::Step(i)) => Some(Target::Step(i)),
        Some(Active::Bend(i)) => Some(Target::Bend(i)),
        Some(Active::Paint { .. }) => Some(Target::Paint),
        None => hover,
    };
    if let Some(Target::Step(i)) = lit {
        if let (Some(a), Some(b)) = (work.points.get(i), work.points.get(i + 1)) {
            p.line_segment([c.at(*a), c.at(*b)], Stroke::new(4.0, Color32::WHITE));
        }
    }
    if let (Some(Target::Paint), Some(q)) = (lit, pointer) {
        // the cell the pencil fills
        let g = c.cell();
        let x = (c.from_x)(q.x).clamp(c.x0, c.x1);
        let s = ((x / g).floor() * g).max(c.x0);
        let (l, r) = ((c.to_x)(s), (c.to_x)((s + g).min(c.x1)));
        let y = c.to_y(c.value_of_y(q.y));
        p.rect_filled(Rect::from_min_max(pos2(l, c.rect.top()), pos2(r.max(l + 2.0), c.rect.bottom())), 0.0, Color32::from_white_alpha(18));
        p.line_segment([pos2(l, y), pos2(r.max(l + 2.0), y)], Stroke::new(2.5, Color32::WHITE));
    }
    if hot {
        for i in 0..work.points.len().saturating_sub(1) {
            if let Some(h) = handle(&work, c, i) {
                let big = lit == Some(Target::Bend(i));
                p.circle_filled(h, if big { 4.5 } else { 3.0 }, if big { Color32::WHITE } else { Color32::from_black_alpha(200) });
                p.circle_stroke(h, if big { 4.5 } else { 3.0 }, Stroke::new(1.2, if big { c.color } else { Color32::WHITE.gamma_multiply(0.8) }));
            }
        }
    }
    for (k, pt) in work.points.iter().enumerate() {
        let big = lit == Some(Target::Point(k));
        let r = if big { POINT_R + 1.5 } else if hot { POINT_R } else { 2.5 };
        p.circle_filled(c.at(*pt), r, if big { Color32::WHITE } else { c.color });
        if hot {
            p.circle_stroke(c.at(*pt), r, Stroke::new(1.0, Color32::from_black_alpha(160)));
        }
    }
    if let (true, Some(q)) = (engaged, pointer) {
        let (x, v) = match lit {
            Some(Target::Point(k)) => work.points.get(k).copied().unwrap_or_default(),
            // where it switches
            Some(Target::Step(i)) => work.points.get(i + 1).copied().unwrap_or_default(),
            Some(Target::Bend(i)) => {
                let x = work.points.get(i..=i + 1).map_or(0.0, |s| (s[0].0 + s[1].0) / 2.0);
                (x, work.value_at(x))
            }
            Some(Target::Paint) => ((c.from_x)(q.x).clamp(c.x0, c.x1), c.value_of_y(q.y)),
            _ => {
                let x = (c.from_x)(q.x).clamp(c.x0, c.x1);
                (x, work.value_at(x))
            }
        };
        let hint = match lit {
            Some(Target::Point(_)) => "drag to move · double-click removes",
            Some(Target::Step(_)) => "drag to move the switch · Option: this track only",
            Some(Target::Bend(_)) => "drag to bend · double-click: straight",
            Some(Target::Paint) => "draws steps on the grid",
            Some(Target::Line(Some(i))) if is_slope(&work, i) => "drag: new point · double-click: hard switch · ⌘-drag: draw",
            _ => "drag: new point · ⌘-drag: draw",
        };
        // on the tooltip layer: a thin band would clip it
        let tp = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, id.with("tip")));
        let galley = tp.layout_no_wrap(format!("{} · {}", (c.fmt_x)(x), (c.fmt)(v)), egui::FontId::proportional(11.0), Color32::WHITE);
        let hint = tp.layout_no_wrap(hint.into(), egui::FontId::proportional(10.0), Color32::from_gray(170));
        let size = vec2(galley.size().x.max(hint.size().x), galley.size().y + hint.size().y);
        let screen = ui.ctx().content_rect();
        let at = pos2((q.x + 14.0).min(screen.right() - size.x - 6.0), (q.y - size.y - 14.0).max(screen.top() + 2.0));
        tp.rect_filled(Rect::from_min_size(at - vec2(4.0, 2.0), size + vec2(8.0, 4.0)), 4.0, Color32::from_black_alpha(215));
        let gh = galley.size().y;
        tp.galley(at, galley, Color32::WHITE);
        tp.galley(at + vec2(0.0, gh), hint, Color32::from_gray(170));
    }
    Edit { changed, slid }
}

fn cursor(active: Option<Active>, hover: Option<Target>) -> CursorIcon {
    match (active, hover) {
        (Some(Active::Step(_)), _) | (None, Some(Target::Step(_))) => CursorIcon::ResizeHorizontal,
        (Some(Active::Bend(_)), _) | (None, Some(Target::Bend(_))) => CursorIcon::ResizeVertical,
        (Some(Active::Paint { .. }), _) | (None, Some(Target::Paint)) => CursorIcon::Cell,
        (Some(Active::Point { .. }), _) => CursorIcon::Grabbing,
        (None, Some(Target::Point(_))) => CursorIcon::Grab,
        _ => CursorIcon::Crosshair,
    }
}

/// `lane` with points at both ends of the domain (an empty lane is flat at the neutral value),
/// so every edit sees the whole curve.
fn normalized(lane: &Lane, c: &Curve) -> Lane {
    let mut l = lane.clone();
    match (l.points.first().copied(), l.points.last().copied()) {
        (Some(first), Some(last)) => {
            if first.0 > c.x0 + EPS {
                l.insert_point(0, (c.x0, first.1));
            }
            if last.0 < c.x1 - EPS {
                l.points.push((c.x1, last.1));
            }
        }
        _ => l.points = vec![(c.x0, c.neutral), (c.x1, c.neutral)],
    }
    l
}

/// What is under `q`: points first, then the vertical line of a hard switch, a bend handle and
/// the line.
fn target_at(l: &Lane, c: &Curve, q: Pos2, m: Modifiers) -> Option<Target> {
    if !c.rect.expand(HIT).contains(q) {
        return None;
    }
    if m.command {
        return c.rect.expand2(vec2(0.0, 4.0)).contains(q).then_some(Target::Paint);
    }
    let pts = &l.points;
    let near = pts.iter().enumerate().map(|(k, pt)| (k, c.at(*pt).distance(q))).filter(|(_, d)| *d <= HIT).min_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((k, _)) = near {
        return Some(Target::Point(k));
    }
    let step = (0..pts.len().saturating_sub(1)).find(|&i| {
        let (a, b) = (c.at(pts[i]), c.at(pts[i + 1]));
        l.is_switch(i) && ((a.x + b.x) / 2.0 - q.x).abs() <= 6.0 && q.y >= a.y.min(b.y) - 3.0 && q.y <= a.y.max(b.y) + 3.0
    });
    if let Some(i) = step {
        return Some(Target::Step(i));
    }
    if let Some(i) = (0..pts.len().saturating_sub(1)).find(|&i| handle(l, c, i).is_some_and(|h| h.distance(q) <= 7.0)) {
        return Some(Target::Bend(i));
    }
    let x = (c.from_x)(q.x).clamp(c.x0, c.x1);
    if !c.rect.expand2(vec2(0.0, 4.0)).contains(q) || (c.to_y(l.value_at(x)) - q.y).abs() > HIT {
        return None;
    }
    let seg = (0..pts.len().saturating_sub(1)).find(|&i| pts[i].0 <= x && x <= pts[i + 1].0 && pts[i + 1].0 - pts[i].0 > EPS);
    match seg {
        Some(i) if m.alt && is_slope(l, i) => Some(Target::Bend(i)),
        _ => Some(Target::Line(seg)),
    }
}

/// Whether the segment from point `i` rises or falls (a ramp that can bend or become a switch).
fn is_slope(l: &Lane, i: usize) -> bool {
    l.points.get(i..=i + 1).is_some_and(|s| s[1].0 - s[0].0 >= SWITCH_GAP && (s[1].1 - s[0].1).abs() > 1e-3)
}

/// The bend handle of the segment from point `i`: at its middle, when the slope is big enough on
/// screen to grab it apart from its points.
fn handle(l: &Lane, c: &Curve, i: usize) -> Option<Pos2> {
    let (a, b) = (*l.points.get(i)?, *l.points.get(i + 1)?);
    let (pa, pb) = (c.at(a), c.at(b));
    if !is_slope(l, i) || pb.x - pa.x < 28.0 || (pb.y - pa.y).abs() < 8.0 {
        return None;
    }
    let mid = (a.0 + b.0) / 2.0;
    Some(c.at((mid, l.value_at(mid))))
}

/// Screen polyline of the curve, across the whole domain; bent segments are sampled.
fn line_points(l: &Lane, c: &Curve) -> Vec<Pos2> {
    let pts = &l.points;
    let mut v = Vec::with_capacity(pts.len() + 2);
    if let Some(first) = pts.first() {
        if first.0 > c.x0 {
            v.push(c.at((c.x0, first.1)));
        }
    }
    for (i, pt) in pts.iter().enumerate() {
        if i > 0 && l.bend(i - 1) != 0.0 {
            let a = pts[i - 1];
            let n = (((c.to_x)(pt.0) - (c.to_x)(a.0)) / 3.0).clamp(1.0, 64.0) as usize;
            for s in 1..n {
                let x = a.0 + (pt.0 - a.0) * s as f32 / n as f32;
                v.push(c.at((x, l.value_at(x))));
            }
        }
        v.push(c.at(*pt));
    }
    if let Some(last) = pts.last() {
        if last.0 < c.x1 {
            v.push(c.at((c.x1, last.1)));
        }
    }
    v
}

/// The other point of the hard switch that point `k` belongs to.
fn twin(l: &Lane, k: usize) -> Option<usize> {
    if k > 0 && l.is_switch(k - 1) {
        Some(k - 1)
    } else if l.is_switch(k) {
        Some(k + 1)
    } else {
        None
    }
}

/// Slide the hard switch made of points `i` and `i + 1` to screen x `px` (its later point
/// snaps there), between its neighbours.
fn move_step(l: &mut Lane, i: usize, px: f32, c: &Curve, free: bool) -> bool {
    l.slide_switch(i, c.snap((c.from_x)(px), free))
}

/// Move point `k` to the pointer, between its neighbours. A point grabbed as part of a hard
/// switch (`step`) takes its twin along sideways.
fn move_point(l: &mut Lane, k: usize, step: bool, q: Pos2, c: &Curve, free: bool) -> bool {
    let v = c.value_of_y(q.y);
    if let Some(t) = twin(l, k).filter(|_| step) {
        let moved = move_step(l, k.min(t), q.x, c, free);
        let same = (l.points[k].1 - v).abs() < 1e-6;
        l.points[k].1 = v;
        return moved || !same;
    }
    let points = &mut l.points;
    let (ox, ov) = points[k];
    // the first and last point stay at the edges
    let pinned = k == 0 || k + 1 == points.len();
    let lo = if k > 0 { points[k - 1].0 } else { c.x0 };
    let hi = if k + 1 < points.len() { points[k + 1].0 } else { c.x1 };
    let x = if pinned { ox } else { c.snap((c.from_x)(q.x), free).clamp(lo, hi) };
    if (x - ox).abs() < 1e-6 && (v - ov).abs() < 1e-6 {
        return false;
    }
    points[k] = (x, v);
    true
}

/// Turn the slope from point `i` into a hard switch at `x`: its start value until `x`, its end
/// value after.
fn make_switch(l: &mut Lane, i: usize, x: f32) -> bool {
    if !is_slope(l, i) {
        return false;
    }
    let (a, b) = (l.points[i], l.points[i + 1]);
    let x = x.clamp(a.0, b.0);
    l.set_bend(i, 0.0);
    l.insert_point(i + 1, (x, a.1));
    l.insert_point(i + 2, (x, b.1));
    l.tidy();
    true
}

/// Draw with the pencil from `a` to `b` (time, value): every grid cell in between becomes flat,
/// at a value that runs from `a`'s to `b`'s.
fn paint_stroke(l: &mut Lane, c: &Curve, a: (f32, f32), b: (f32, f32)) -> bool {
    let g = c.cell();
    let before = l.clone();
    let (na, nb) = ((a.0 / g).floor() as i64, (b.0 / g).floor() as i64);
    for n in na.min(nb)..=na.max(nb) {
        let (s, e) = ((n as f32 * g).max(c.x0), ((n + 1) as f32 * g).min(c.x1));
        if e - s < EPS {
            continue;
        }
        let t = if na == nb { 1.0 } else { (n - na) as f32 / (nb - na) as f32 };
        l.paint(s, e, a.1 + (b.1 - a.1) * t);
    }
    *l != before
}

#[cfg(test)]
mod tests {
    use super::*;
    use clubcraft_core::{Param, Side};

    fn with_curve(f: impl FnOnce(&Curve)) {
        // 100 px per unit, 0..8 on x, value 0..1 over 0..100 px (y down)
        let to_x = |b: f32| b * 100.0;
        let from_x = |x: f32| x / 100.0;
        let fmt = |v: f32| format!("{v}");
        let c = Curve { rect: Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 100.0)), x0: 0.0, x1: 8.0, to_x: &to_x, from_x: &from_x, grid: 1.0, neutral: 1.0, centered: false, color: Color32::WHITE, editable: true, fmt: &fmt, fmt_x: &fmt };
        f(&c);
    }
    fn lane(pts: &[(f32, f32)]) -> Lane {
        Lane::new(Side::Out, Param::Volume, pts)
    }

    #[test]
    fn dragging_moves_and_snaps_points_between_their_neighbours() {
        with_curve(|c| {
            let mut l = lane(&[(0.0, 1.0), (2.0, 1.0), (4.0, 0.5), (6.0, 0.0), (8.0, 0.0)]);
            // move the point at 2 towards x 5.2: it stops at its neighbour at 4, at half height
            assert!(move_point(&mut l, 1, false, pos2(520.0, 50.0), c, false));
            assert_eq!(l.points[1], (4.0, 0.5));
            // and back out again: it was not grabbed as a switch, so it moves on its own
            move_point(&mut l, 1, false, pos2(300.0, 50.0), c, false);
            assert_eq!(&l.points[1..3], &[(3.0, 0.5), (4.0, 0.5)]);
            // values snap to 0 / 100 % near the edges
            move_point(&mut l, 1, false, pos2(300.0, 98.0), c, false);
            assert_eq!(l.points[1], (3.0, 0.0));
            // the end points only move vertically
            move_point(&mut l, 0, false, pos2(300.0, 30.0), c, false);
            assert_eq!(l.points[0], (0.0, 0.7));
            // Shift: free placement
            move_point(&mut l, 1, false, pos2(333.0, 50.0), c, true);
            assert!((l.points[1].0 - 3.33).abs() < 1e-4);
        });
    }

    #[test]
    fn hard_switches_move_sideways_as_one() {
        with_curve(|c| {
            // off until 2, on from 2 (a step), down at 6
            let mut l = lane(&[(0.0, 0.0), (2.0, 0.0), (2.0, 1.0), (6.0, 1.0), (8.0, 0.0)]);
            // drag the edge to 3.8 → snaps to 4; both points move, values stay
            assert!(move_step(&mut l, 1, 380.0, c, false));
            assert_eq!(&l.points[1..3], &[(4.0, 0.0), (4.0, 1.0)]);
            // not past its neighbours
            move_step(&mut l, 1, 750.0, c, false);
            assert_eq!(l.points[1].0, 6.0);
            move_step(&mut l, 1, 300.0, c, false);
            // dragging the upper point of the step: sideways together, vertically only itself
            move_point(&mut l, 2, true, pos2(100.0, 50.0), c, false);
            assert_eq!(&l.points[1..3], &[(1.0, 0.0), (1.0, 0.5)]);
        });
    }

    #[test]
    fn a_swap_slides_although_its_points_are_a_hair_apart() {
        with_curve(|c| {
            // as the presets make it: on until 4, off from 4, the switch 0.01 beat wide
            let mut l = lane(&[(0.0, 1.0), (3.99, 1.0), (4.0, 0.0), (8.0, 0.0)]);
            assert_eq!(target_at(&l, c, pos2(400.0, 50.0), Modifiers::NONE), Some(Target::Step(1)));
            assert_eq!(twin(&l, 2), Some(1));
            // grabbed by its lower point and dragged to 6: both go, the gap stays, values stay
            assert!(move_point(&mut l, 2, true, pos2(610.0, 100.0), c, false));
            assert!((l.points[1].0 - 5.99).abs() < 1e-5 && l.points[2] == (6.0, 0.0) && l.points[1].1 == 1.0);
            // not past its neighbour
            move_step(&mut l, 1, 790.0, c, false);
            assert_eq!(l.points[2].0, 8.0);
            // a slope is no switch
            let ramp = lane(&[(0.0, 1.0), (4.0, 0.0)]);
            assert_eq!(twin(&ramp, 1), None);
        });
    }

    #[test]
    fn the_pointer_finds_points_switches_handles_and_the_line() {
        with_curve(|c| {
            let l = lane(&[(0.0, 0.0), (2.0, 0.0), (2.0, 1.0), (6.0, 0.0), (8.0, 0.0)]);
            let none = Modifiers::NONE;
            assert_eq!(target_at(&l, c, pos2(203.0, 2.0), none), Some(Target::Point(2)));
            assert_eq!(target_at(&l, c, pos2(202.0, 50.0), none), Some(Target::Step(1)));
            // the handle of the slope from 2 to 6 sits at its middle
            assert_eq!(target_at(&l, c, pos2(400.0, 50.0), none), Some(Target::Bend(2)));
            assert_eq!(target_at(&l, c, pos2(300.0, 25.0), none), Some(Target::Line(Some(2))));
            assert_eq!(target_at(&l, c, pos2(300.0, 25.0), Modifiers::ALT), Some(Target::Bend(2)));
            assert_eq!(target_at(&l, c, pos2(700.0, 99.0), none), Some(Target::Line(Some(3))));
            assert_eq!(target_at(&l, c, pos2(700.0, 50.0), none), None);
            assert_eq!(target_at(&l, c, pos2(700.0, 50.0), Modifiers::COMMAND), Some(Target::Paint));
        });
    }

    #[test]
    fn slopes_become_switches_and_the_pencil_draws_steps() {
        with_curve(|c| {
            let mut l = lane(&[(0.0, 1.0), (4.0, 0.0), (8.0, 0.0)]);
            assert!(make_switch(&mut l, 0, 3.0));
            assert_eq!(l.points, vec![(0.0, 1.0), (3.0, 1.0), (3.0, 0.0), (8.0, 0.0)]);
            // flat lines have no switch to make
            assert!(!make_switch(&mut l, 2, 5.0));
            // a stroke from 5.5 (half) to 7.2 (full): one flat cell per beat, ramping up
            assert!(paint_stroke(&mut l, c, (5.5, 0.5), (7.2, 1.0)));
            assert_eq!(l.value_at(5.5), 0.5);
            assert_eq!(l.value_at(6.5), 0.75);
            assert_eq!(l.value_at(7.5), 1.0);
            assert_eq!(l.value_at(4.5), 0.0);
        });
    }

    #[test]
    fn edits_see_the_whole_domain() {
        with_curve(|c| {
            assert_eq!(normalized(&lane(&[]), c).points, vec![(0.0, 1.0), (8.0, 1.0)]);
            assert_eq!(normalized(&lane(&[(2.0, 0.3)]), c).points, vec![(0.0, 0.3), (2.0, 0.3), (8.0, 0.3)]);
        });
    }
}
