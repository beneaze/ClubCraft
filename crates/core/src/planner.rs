//! Studio intelligence: harmonic/tempo/energy track ordering and automatic mix planning
//! (mix points, transition lengths, styles and key shifts) from analysis results.

use crate::analysis::{Analysis, CueKind, SectionKind};
use crate::key::Key;
use crate::library::TrackId;
use crate::mix::{Mix, MixTrack, TempoMode, Transition, TransitionStyle};

/// What the ordering optimises for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderOptions {
    /// 0 = only tempo matters, 1 = only key matters.
    pub key_weight: f32,
    /// Weight of a smooth, rising energy arc.
    pub energy_weight: f32,
    /// Allow ±1 semitone key shifts to fix clashes (small penalty). Off by default: tracks keep
    /// their own key unless the user shifts them.
    pub allow_key_shift: bool,
    pub lock_first: bool,
    pub lock_last: bool,
    /// Optimisation iterations (simulated annealing).
    pub iterations: usize,
}

impl Default for OrderOptions {
    fn default() -> Self {
        OrderOptions {
            key_weight: 0.5,
            energy_weight: 0.3,
            allow_key_shift: false,
            lock_first: false,
            lock_last: false,
            iterations: 200_000,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OrderItem {
    pub id: TrackId,
    pub bpm: Option<f64>,
    pub key: Option<Key>,
    pub energy: Option<u8>,
}

impl OrderItem {
    pub fn from_analysis(id: TrackId, a: Option<&Analysis>) -> Self {
        OrderItem { id, bpm: a.and_then(|a| a.bpm()), key: a.and_then(|a| a.key), energy: a.and_then(|a| a.energy) }
    }
}

/// Tempo distance in BPM between two tracks, allowing half/double time.
pub fn bpm_distance(a: f64, b: f64) -> f64 {
    [b, b * 2.0, b * 0.5].iter().map(|x| (x - a).abs()).fold(f64::MAX, f64::min)
}

/// Best key shift (semitones, -1..=1) to apply to `b` so it is compatible after `a`, and the
/// resulting compatibility.
pub fn best_key_shift(a: Key, b: Key, allow: bool) -> (i32, f32) {
    let mut best = (0, a.compatibility(b));
    if allow {
        for s in [-1, 1] {
            let c = a.compatibility(b.shifted(s)) * 0.85;
            if c > best.1 + 1e-6 {
                best = (s, c);
            }
        }
    }
    best
}

/// Cost of playing `b` directly after `a` (lower is better).
pub fn pair_cost(a: &OrderItem, b: &OrderItem, o: &OrderOptions) -> f32 {
    let key_cost = match (a.key, b.key) {
        (Some(ka), Some(kb)) => 1.0 - best_key_shift(ka, kb, o.allow_key_shift).1,
        _ => 0.5,
    };
    let bpm_cost = match (a.bpm, b.bpm) {
        (Some(x), Some(y)) => {
            let d = bpm_distance(x, y) as f32;
            // within 2 BPM is free-ish, beyond 6 BPM gets expensive; dropping tempo costs extra
            let base = (d / 6.0).powi(2).min(4.0);
            let down = if bpm_distance(x, y) == (y - x).abs() && y < x - 0.5 { 0.25 } else { 0.0 };
            base + down
        }
        _ => 0.5,
    };
    let energy_cost = match (a.energy, b.energy) {
        (Some(x), Some(y)) => {
            let d = y as f32 - x as f32;
            // gentle rises are good, big jumps or drops are not
            if d >= 0.0 { (d - 1.0).max(0.0) * 0.25 } else { -d * 0.35 }
        }
        _ => 0.0,
    };
    let kw = o.key_weight.clamp(0.0, 1.0);
    kw * key_cost * 2.0 + (1.0 - kw) * bpm_cost * 2.0 + o.energy_weight * energy_cost
}

fn arc_cost(items: &[OrderItem], order: &[usize], o: &OrderOptions) -> f32 {
    // Energy arc: penalise the energy peak being in the first half of the set.
    if o.energy_weight <= 0.0 || order.len() < 4 {
        return 0.0;
    }
    let n = order.len() as f32;
    let mut c = 0.0;
    for (i, &ix) in order.iter().enumerate() {
        if let Some(e) = items[ix].energy {
            let pos = i as f32 / (n - 1.0);
            // target: rises from ~4 to ~8 at 75 %, eases off at the end
            let target = if pos < 0.75 { 4.0 + pos / 0.75 * 4.0 } else { 8.0 - (pos - 0.75) * 4.0 };
            c += ((e as f32 - target) / 4.0).powi(2);
        }
    }
    o.energy_weight * c / n * 2.0
}

fn total_cost(items: &[OrderItem], order: &[usize], m: &[Vec<f32>], o: &OrderOptions) -> f32 {
    order.windows(2).map(|w| m[w[0]][w[1]]).sum::<f32>() + arc_cost(items, order, o)
}

/// Order tracks for a smooth harmonic set. Deterministic (fixed seed).
pub fn order_tracks(items: &[OrderItem], o: &OrderOptions) -> Vec<TrackId> {
    let n = items.len();
    if n < 3 {
        return items.iter().map(|i| i.id).collect();
    }
    let m: Vec<Vec<f32>> =
        (0..n).map(|i| (0..n).map(|j| if i == j { 0.0 } else { pair_cost(&items[i], &items[j], o) }).collect()).collect();
    order_indices(items, &m, o).into_iter().map(|i| items[i].id).collect()
}

/// Order groups of tracks that must stay together in their order (locked transitions): the cost
/// of `a → b` is that of the last track of `a` into the first of `b`. Returns group indices.
pub fn order_groups(groups: &[Vec<OrderItem>], o: &OrderOptions) -> Vec<usize> {
    let n = groups.len();
    if n < 3 {
        return (0..n).collect();
    }
    let m: Vec<Vec<f32>> = (0..n)
        .map(|i| (0..n).map(|j| if i == j { 0.0 } else { match (groups[i].last(), groups[j].first()) {
                        (Some(a), Some(b)) => pair_cost(a, b, o),
                        _ => 0.0,
                    } }).collect())
        .collect();
    let reps: Vec<OrderItem> = groups.iter().filter_map(|g| g.first().cloned()).collect();
    if reps.len() != n {
        return (0..n).collect();
    }
    order_indices(&reps, &m, o)
}

fn order_indices(items: &[OrderItem], m: &[Vec<f32>], o: &OrderOptions) -> Vec<usize> {
    let n = items.len();
    // Greedy start: begin at the locked first track or the slowest / lowest energy track.
    let mut start = 0;
    if !o.lock_first {
        start = (0..n)
            .min_by(|&a, &b| {
                let ka = items[a].bpm.unwrap_or(999.0) + items[a].energy.unwrap_or(5) as f64 * 0.5;
                let kb = items[b].bpm.unwrap_or(999.0) + items[b].energy.unwrap_or(5) as f64 * 0.5;
                ka.total_cmp(&kb)
            })
            .unwrap_or(0);
    }
    let last_locked = if o.lock_last { Some(n - 1) } else { None };
    let mut used = vec![false; n];
    let mut order = vec![start];
    used[start] = true;
    if let Some(l) = last_locked {
        used[l] = true;
    }
    while order.len() < n - last_locked.is_some() as usize {
        let cur = order.last().copied().unwrap_or(start);
        let Some(next) = (0..n).filter(|&j| !used[j]).min_by(|&a, &b| m[cur][a].total_cmp(&m[cur][b])) else { break };
        used[next] = true;
        order.push(next);
    }
    if let Some(l) = last_locked {
        order.push(l);
    }

    // Simulated annealing over 2-opt reversals and single moves; locked ends stay in place.
    let lo = if o.lock_first { 1 } else { 0 };
    let hi = if o.lock_last { n - 1 } else { n }; // exclusive
    if hi - lo < 2 {
        return order;
    }
    let mut rng = 0x2545F4914F6CDD1Du64;
    let mut rand = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut cur_cost = total_cost(items, &order, m, o);
    let mut best = order.clone();
    let mut best_cost = cur_cost;
    let iters = o.iterations.max(1);
    let t0 = 1.0f32;
    for it in 0..iters {
        let temp = t0 * (1.0 - it as f32 / iters as f32).powi(3) + 1e-4;
        let a = lo + (rand() as usize) % (hi - lo);
        let b = lo + (rand() as usize) % (hi - lo);
        if a == b {
            continue;
        }
        let (i, j) = (a.min(b), a.max(b));
        let mut cand = order.clone();
        if rand() % 2 == 0 {
            cand[i..=j].reverse();
        } else {
            let t = cand.remove(a);
            cand.insert(b, t);
        }
        let c = total_cost(items, &cand, m, o);
        let accept = c < cur_cost || {
            let p = (-(c - cur_cost) / temp).exp();
            (rand() % 1_000_000) as f32 / 1_000_000.0 < p
        };
        if accept {
            order = cand;
            cur_cost = c;
            if c < best_cost {
                best_cost = c;
                best = order.clone();
            }
        }
    }
    best
}

// ------------------------------------------------------------------ mix planning

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlanOptions {
    /// Preferred transition length in beats (default 32 = 8 bars, shortened to fit).
    pub transition_beats: Option<u32>,
    pub style: Option<TransitionStyle>,
    /// Shift tracks by ±1 semitone to fix key clashes. Off by default (see [`OrderOptions`]).
    pub allow_key_shift: bool,
    pub has_stems: bool,
}

impl Default for PlanOptions {
    fn default() -> Self {
        PlanOptions { transition_beats: None, style: None, allow_key_shift: false, has_stems: true }
    }
}

/// Default mix-in point (s): MixIn cue, else first downbeat.
pub fn default_mix_in(a: &Analysis) -> f64 {
    if let Some(c) = a.cue(CueKind::MixIn) {
        return c.pos;
    }
    if let Some(c) = a.cue(CueKind::FirstBeat) {
        return c.pos;
    }
    a.grid.map(|g| g.downbeat).unwrap_or(0.0)
}

/// Default mix-out point (s): MixOut cue, else start of the outro, else 32 bars before the end.
pub fn default_mix_out(a: &Analysis) -> f64 {
    if let Some(c) = a.cue(CueKind::MixOut) {
        return c.pos;
    }
    if let Some(s) = a.sections.iter().rev().find(|s| s.kind == SectionKind::Outro) {
        return s.start;
    }
    match a.grid {
        Some(g) => {
            let last_bar = g.bar_at(a.duration).floor();
            let bar = ((last_bar - 32.0) / 8.0).floor() * 8.0;
            g.time_of_bar(bar.max(0.0))
        }
        None => (a.duration - 60.0).max(0.0),
    }
}

/// Choose the transition length (beats) for `out` → `inc` with the given mix points.
pub fn choose_transition_beats(out: &Analysis, mix_out: f64, inc: &Analysis, mix_in: f64, pref: Option<u32>) -> u32 {
    let (Some(go), Some(gi)) = (out.grid, inc.grid) else { return pref.unwrap_or(32) };
    // room after mix-out in the outgoing track, and before the first drop/vocal in the incoming
    let room_out = go.beat_at(out.duration) - go.beat_at(mix_out);
    let first_peak = inc
        .sections
        .iter()
        .find(|s| s.start > mix_in + 1.0 && matches!(s.kind, SectionKind::Drop | SectionKind::Verse))
        .map(|s| s.start)
        .unwrap_or(inc.duration);
    let room_in = gi.beat_at(first_peak) - gi.beat_at(mix_in);
    let want = pref.unwrap_or(32) as f64;
    let mut beats = want;
    while beats > 8.0 && (beats > room_out - 1.0 || beats > room_in.max(16.0) + 0.5) {
        beats /= 2.0;
    }
    (beats.max(8.0).round() as u32 / 4 * 4).max(8)
}

/// Build a mix for `order`, using analyses for mix points and transition choices.
pub fn plan_mix(name: &str, order: &[TrackId], analysis: &dyn Fn(TrackId) -> Option<Analysis>, o: &PlanOptions) -> Mix {
    let mut mix = Mix { name: name.to_string(), tempo: TempoMode::Glide, ..Default::default() };
    let analyses: Vec<Option<Analysis>> = order.iter().map(|&id| analysis(id)).collect();
    let mut cumulative_shift = 0;
    for (i, &id) in order.iter().enumerate() {
        let a = analyses[i].as_ref();
        let mix_in = if i == 0 { a.map(|a| a.cue(CueKind::FirstBeat).map(|c| c.pos).unwrap_or(0.0)).unwrap_or(0.0) } else { a.map(default_mix_in).unwrap_or(0.0) };
        let mix_out = a.map(default_mix_out).unwrap_or(0.0);
        let mut key_shift = 0;
        if i > 0 && o.allow_key_shift {
            if let (Some(prev), Some(cur)) = (analyses[i - 1].as_ref().and_then(|a| a.key), a.and_then(|a| a.key)) {
                let prev = prev.shifted(cumulative_shift);
                let (s, _) = best_key_shift(prev, cur, true);
                key_shift = s;
            }
        }
        cumulative_shift = key_shift;
        mix.tracks.push(MixTrack { track_id: id, mix_in, mix_out: mix_out.max(mix_in + 1.0), key_shift, gain_db: 0.0, stems: Vec::new() });
    }
    for i in 0..order.len().saturating_sub(1) {
        let (out, inc) = (analyses[i].clone().unwrap_or_default(), analyses[i + 1].clone().unwrap_or_default());
        let beats = choose_transition_beats(&out, mix.tracks[i].mix_out, &inc, mix.tracks[i + 1].mix_in, o.transition_beats);
        let style = o.style.unwrap_or(if o.has_stems { TransitionStyle::StemBlend } else { TransitionStyle::BassSwap });
        mix.transitions.push(Transition::new(style, beats));
    }
    mix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_harmonically() {
        let k = |s: &str| Key::from_camelot(s);
        let items = vec![
            OrderItem { id: 1, bpm: Some(124.0), key: k("8A"), energy: Some(5) },
            OrderItem { id: 2, bpm: Some(124.0), key: k("3B"), energy: Some(6) },
            OrderItem { id: 3, bpm: Some(124.0), key: k("9A"), energy: Some(6) },
            OrderItem { id: 4, bpm: Some(124.0), key: k("2B"), energy: Some(7) },
            OrderItem { id: 5, bpm: Some(124.0), key: k("10A"), energy: Some(7) },
            OrderItem { id: 6, bpm: Some(124.0), key: k("1B"), energy: Some(5) },
        ];
        let o = OrderOptions { key_weight: 1.0, energy_weight: 0.0, allow_key_shift: false, ..Default::default() };
        let order = order_tracks(&items, &o);
        let cost: f32 = order
            .windows(2)
            .map(|w| {
                let a = items.iter().find(|i| i.id == w[0]).unwrap();
                let b = items.iter().find(|i| i.id == w[1]).unwrap();
                pair_cost(a, b, &o)
            })
            .sum();
        // 8A-9A-10A and 1B-2B-3B chains joined by one bad edge
        assert!(cost <= 2.0 * (1.0 - 0.0) + 4.0 * 2.0 * 0.1 + 0.01, "order {order:?} cost {cost}");
    }

    #[test]
    fn plans_without_key_shifts_by_default() {
        // 8A then 3A clash; the planner may only shift keys when asked to
        let an = |id: TrackId| {
            let key = Key::from_camelot(if id == 1 { "8A" } else { "3A" });
            Some(Analysis { version: 1, duration: 240.0, key, grid: Some(crate::BeatGrid::new(124.0, 0.0)), ..Default::default() })
        };
        let mix = plan_mix("m", &[1, 2], &an, &PlanOptions::default());
        assert!(mix.tracks.iter().all(|t| t.key_shift == 0), "{:?}", mix.tracks.iter().map(|t| t.key_shift).collect::<Vec<_>>());
        let shifted = plan_mix("m", &[1, 2], &an, &PlanOptions { allow_key_shift: true, ..Default::default() });
        assert_ne!(shifted.tracks[1].key_shift, 0, "with the option on, the clash is shifted away");
    }

    #[test]
    fn bpm_half_double() {
        assert!(bpm_distance(128.0, 64.0) < 1e-9);
        assert!((bpm_distance(124.0, 126.0) - 2.0).abs() < 1e-9);
    }
}
