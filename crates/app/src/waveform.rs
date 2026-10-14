//! Waveform painting: scrolling deck waveform, track overview and timeline clips.

use crate::theme;
use clubcraft_core::{BeatGrid, Cue, Section, StemKind, Waveform};
use eframe::egui::{self, epaint::Mesh, pos2, vec2, Color32, Painter, Rect, Stroke};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WaveStyle {
    /// Per-stem colours (needs separated stems; falls back to `Bands`).
    Stems,
    /// Low / mid / high frequency colouring.
    Bands,
}

pub struct WaveParams<'a> {
    pub wf: &'a Waveform,
    pub grid: Option<BeatGrid>,
    pub cues: &'a [Cue],
    pub sections: &'a [Section],
    pub style: WaveStyle,
    /// Gains 0..1 per stem; muted stems are drawn dimmed.
    pub stem_gains: [f32; 4],
    pub loop_region: Option<(f64, f64)>,
    pub accent: Color32,
}

#[inline]
fn amp(v: u8) -> f32 {
    v as f32 / 255.0
}

/// Linear interpolation at a fractional bin position (smooth waveforms when zoomed in far).
#[inline]
fn bin_lerp(v: &[u8], pos: f64) -> u8 {
    if v.is_empty() || pos < 0.0 {
        return 0;
    }
    let i = pos.floor() as usize;
    let f = (pos - i as f64) as f32;
    let a = *v.get(i).unwrap_or(&0) as f32;
    let b = *v.get(i + 1).unwrap_or(&(a as u8)) as f32;
    (a + (b - a) * f).round() as u8
}

#[inline]
fn pick(v: &[u8], a: usize, b: usize, fine: bool, mid: f64) -> u8 {
    if fine {
        bin_lerp(v, mid - 0.5)
    } else {
        bin_max(v, a, b)
    }
}

/// Aggregate (max) of bins [a, b).
#[inline]
fn bin_max(v: &[u8], a: usize, b: usize) -> u8 {
    let b = b.min(v.len());
    if a >= b {
        return v.get(a.min(v.len().saturating_sub(1))).copied().unwrap_or(0);
    }
    v[a..b].iter().copied().max().unwrap_or(0)
}

fn add_bar(mesh: &mut Mesh, x0: f32, x1: f32, cy: f32, h: f32, c: Color32) {
    if h < 0.25 {
        return;
    }
    mesh.add_colored_rect(Rect::from_min_max(pos2(x0, cy - h), pos2(x1, cy + h)), c);
}

/// Draw waveform columns for the time window [t0, t1) into `rect`.
pub fn paint_wave(p: &Painter, rect: Rect, w: &WaveParams, t0: f64, t1: f64, mirrored: bool) {
    let wf = w.wf;
    if wf.is_empty() || t1 <= t0 {
        return;
    }
    let cols = rect.width().max(1.0) as usize;
    let col_w = rect.width() / cols as f32;
    let spp = (t1 - t0) / cols as f64;
    let cy = if mirrored { rect.center().y } else { rect.bottom() };
    let hmax = if mirrored { rect.height() / 2.0 - 1.0 } else { rect.height() - 1.0 };
    let mut mesh = Mesh::default();
    let stems = if w.style == WaveStyle::Stems { wf.stems.as_ref() } else { None };
    let wp_gains = w.stem_gains;
    for c in 0..cols {
        let ta = t0 + c as f64 * spp;
        let tb = ta + spp;
        if tb < 0.0 || ta > wf.duration() {
            continue;
        }
        let a = (ta.max(0.0) * wf.rate) as usize;
        let b = ((tb.max(0.0) * wf.rate).ceil() as usize).max(a + 1);
        let fine = spp * wf.rate < 1.0;
        let mid_bin = (ta + tb) * 0.5 * wf.rate;
        let x0 = rect.left() + c as f32 * col_w;
        let x1 = x0 + col_w + 0.35;
        if let Some(st) = stems {
            // Stacked: the column height is the mix peak, split between the stems in proportion
            // to their energy. Inner → outer: bass, drums, melody, vocals. Muted stems shrink.
            let total = amp(pick(&wf.peak, a, b, fine, mid_bin));
            let order = [StemKind::Bass, StemKind::Drums, StemKind::Other, StemKind::Vocals];
            let mut w = [0f32; 4];
            let mut sum = 0.0;
            for (j, k) in order.iter().enumerate() {
                let v = amp(pick(&st[k.index()], a, b, fine, mid_bin));
                w[j] = v * v;
                sum += w[j];
            }
            if sum > 1e-9 {
                let mut gsum = 0.0;
                let mut eff = [0f32; 4];
                for (j, k) in order.iter().enumerate() {
                    let g = params_gain(&wp_gains, k.index());
                    eff[j] = w[j] * (0.12 + 0.88 * g);
                    gsum += w[j] * g;
                }
                let scale_h = total * hmax * (gsum / sum).max(0.12).sqrt().min(1.0) / eff.iter().sum::<f32>().max(1e-9);
                let mut acc = 0.0;
                for (j, k) in order.iter().enumerate() {
                    let h = eff[j] * scale_h;
                    if h < 0.2 {
                        continue;
                    }
                    let g = params_gain(&wp_gains, k.index());
                    let col = theme::rgb(k.color());
                    let col = if g < 0.05 { col.linear_multiply(0.18) } else { col.linear_multiply(0.45 + 0.55 * g.min(1.0)) };
                    mesh.add_colored_rect(Rect::from_min_max(pos2(x0, cy - acc - h), pos2(x1, cy - acc)), col);
                    if mirrored {
                        mesh.add_colored_rect(Rect::from_min_max(pos2(x0, cy + acc), pos2(x1, cy + acc + h)), col);
                    }
                    acc += h;
                }
            }
        } else {
            let low = amp(pick(&wf.low, a, b, fine, mid_bin));
            let mid = amp(pick(&wf.mid, a, b, fine, mid_bin));
            let high = amp(pick(&wf.high, a, b, fine, mid_bin));
            let peak = amp(pick(&wf.peak, a, b, fine, mid_bin));
            add_bar(&mut mesh, x0, x1, cy, peak * hmax, Color32::from_rgb(40, 60, 110));
            add_bar(&mut mesh, x0, x1, cy, low * hmax, Color32::from_rgb(40, 110, 255));
            add_bar(&mut mesh, x0, x1, cy, mid * hmax * 0.85, Color32::from_rgb(255, 170, 40));
            add_bar(&mut mesh, x0, x1, cy, high * hmax * 0.6, Color32::from_rgb(235, 235, 245));
        }
    }
    p.add(egui::Shape::mesh(mesh));
}

#[inline]
fn params_gain(g: &[f32; 4], i: usize) -> f32 {
    g[i].clamp(0.0, 1.0)
}

fn time_to_x(rect: Rect, t0: f64, t1: f64, t: f64) -> f32 {
    rect.left() + ((t - t0) / (t1 - t0)) as f32 * rect.width()
}

/// Scrolling deck waveform centred on `pos` showing `span` seconds. Returns the time under the
/// pointer if hovered.
pub fn scrolling(p: &Painter, rect: Rect, w: &WaveParams, pos: f64, span: f64) {
    p.rect_filled(rect, 4.0, theme::BG);
    let t0 = pos - span / 2.0;
    let t1 = pos + span / 2.0;
    let pp = p.with_clip_rect(rect);
    // sections strip at the bottom
    for s in w.sections {
        if s.end < t0 || s.start > t1 {
            continue;
        }
        let xa = time_to_x(rect, t0, t1, s.start).max(rect.left());
        let xb = time_to_x(rect, t0, t1, s.end).min(rect.right());
        pp.rect_filled(Rect::from_min_max(pos2(xa, rect.bottom() - 4.0), pos2(xb, rect.bottom())), 0.0, theme::rgb(s.kind.color()).linear_multiply(0.7));
    }
    if let Some((a, b)) = w.loop_region {
        let xa = time_to_x(rect, t0, t1, a);
        let xb = time_to_x(rect, t0, t1, b);
        pp.rect_filled(Rect::from_min_max(pos2(xa, rect.top()), pos2(xb, rect.bottom())), 0.0, Color32::from_rgba_unmultiplied(255, 170, 0, 40));
    }
    paint_wave(&pp, rect.shrink2(vec2(0.0, 6.0)), w, t0, t1, true);
    // beat grid
    if let Some(g) = w.grid {
        let b0 = g.beat_at(t0).floor() as i64;
        let b1 = g.beat_at(t1).ceil() as i64;
        let px_per_beat = rect.width() as f64 * g.beat_len() / span;
        for b in b0..=b1 {
            let t = g.time_of_beat(b as f64);
            let x = time_to_x(rect, t0, t1, t);
            let bar = b.rem_euclid(g.beats_per_bar as i64) == 0;
            let phrase = b.rem_euclid(g.beats_per_bar as i64 * 16) == 0;
            if !bar && px_per_beat < 6.0 {
                continue;
            }
            let (c, wdt) = if phrase {
                (Color32::from_rgba_unmultiplied(255, 60, 60, 200), 1.5)
            } else if bar {
                (Color32::from_rgba_unmultiplied(255, 255, 255, 110), 1.0)
            } else {
                (Color32::from_rgba_unmultiplied(255, 255, 255, 40), 1.0)
            };
            pp.line_segment([pos2(x, rect.top()), pos2(x, rect.top() + 8.0)], Stroke::new(wdt, c));
            pp.line_segment([pos2(x, rect.bottom() - 12.0), pos2(x, rect.bottom() - 4.0)], Stroke::new(wdt, c));
            if bar && px_per_beat > 10.0 {
                let barn = b.div_euclid(g.beats_per_bar as i64) + 1;
                pp.text(pos2(x + 2.0, rect.top() + 1.0), egui::Align2::LEFT_TOP, format!("{barn}"), egui::FontId::proportional(9.0), Color32::from_white_alpha(90));
            }
        }
    }
    // cues
    for c in w.cues {
        if c.pos < t0 || c.pos > t1 {
            continue;
        }
        let x = time_to_x(rect, t0, t1, c.pos);
        let col = theme::rgb(c.color);
        pp.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.5, col.linear_multiply(0.9)));
        let label = match c.slot {
            Some(s) => format!("{} {}", s + 1, c.name),
            None => c.name.clone(),
        };
        let galley = pp.layout_no_wrap(label, egui::FontId::proportional(10.0), Color32::BLACK);
        let r = Rect::from_min_size(pos2(x, rect.top() + 10.0), galley.size() + vec2(6.0, 2.0));
        pp.rect_filled(r, 2.0, col);
        pp.galley(r.min + vec2(3.0, 1.0), galley, Color32::BLACK);
    }
    // playhead
    let x = rect.center().x;
    pp.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(2.0, Color32::WHITE));
    pp.add(egui::Shape::convex_polygon(vec![pos2(x - 6.0, rect.top()), pos2(x + 6.0, rect.top()), pos2(x, rect.top() + 7.0)], Color32::WHITE, Stroke::NONE));
}

/// Whole-track overview with playhead, cues and sections. Returns the clicked time, if any.
pub fn overview(ui: &mut egui::Ui, size: egui::Vec2, w: &WaveParams, pos: f64, duration: f64) -> Option<f64> {
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, theme::BG);
    let dur = duration.max(w.wf.duration()).max(1e-3);
    for s in w.sections {
        let xa = time_to_x(rect, 0.0, dur, s.start);
        let xb = time_to_x(rect, 0.0, dur, s.end);
        p.rect_filled(Rect::from_min_max(pos2(xa, rect.bottom() - 3.0), pos2(xb, rect.bottom())), 0.0, theme::rgb(s.kind.color()));
    }
    paint_wave(&p, rect.shrink2(vec2(0.0, 3.0)), w, 0.0, dur, true);
    // played part darkening
    let xp = time_to_x(rect, 0.0, dur, pos);
    p.rect_filled(Rect::from_min_max(rect.min, pos2(xp, rect.bottom())), 0.0, Color32::from_black_alpha(110));
    for c in w.cues {
        let x = time_to_x(rect, 0.0, dur, c.pos);
        p.line_segment([pos2(x, rect.top()), pos2(x, rect.top() + 6.0)], Stroke::new(2.0, theme::rgb(c.color)));
    }
    p.line_segment([pos2(xp, rect.top()), pos2(xp, rect.bottom())], Stroke::new(1.5, w.accent));
    if resp.clicked() || resp.dragged() {
        if let Some(pt) = resp.interact_pointer_pos() {
            return Some(((pt.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * dur);
        }
    }
    None
}
