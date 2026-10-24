//! Mashup finder: which part of which track can carry the vocals of another track.
//!
//! 1. Per-bar stem features ([`clubcraft_analysis::mashup`]), cached next to the stems and
//!    computed on demand for tracks separated before the finder existed.
//! 2. Rule-based search (design and sources: `docs/mashup-research.md`): whole vocal units of
//!    the vocal track (sung sections, chorus first, by the structure model's labels; boundaries
//!    on lines, from its [`clubcraft_analysis::vocals`] map) at the section starts of the
//!    instrumentals, a chorus on a drop, a verse on a breakdown. Harmony of the key-shifted
//!    vocals bar by bar, masking and residual vocals make the fit; it is scaled by the spot and
//!    the phrase.
//! 3. Optional re-ranking of the best candidates with COCOLA ([`clubcraft_analysis::cocola`]):
//!    5 s snippets of the vocals (pitch-shifted and time-stretched like the mix would play them)
//!    and of the instrumental are embedded and scored; the fit blends both (0.6 rule, 0.4
//!    learned: AUC 0.78 → 0.84 against random placements on the reference library).
//!
//! Both ways round: the track's vocals over the other tracks' instrumentals (old vocals in the
//! new song, [`TransitionStyle::Mashup`]) or the other tracks' vocals over its instrumental (new
//! vocals in the old song, [`TransitionStyle::MashupIn`]); the track plays first either way. A
//! match becomes a two-track mix (or part of a mix) joined by that transition.

use anyhow::{bail, Context, Result};
use clubcraft_analysis::mashup::{self as mm, MashupFeatures, MashupOrder, Match, MatchOptions};
use clubcraft_core::{Analysis, Library, Mix, MixTrack, TempoMode, TrackId, Transition, TransitionStyle};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

/// Weight of the learned score in the final ranking.
const LEARNED_WEIGHT: f32 = 0.4;
/// Candidates re-ranked with COCOLA.
const RERANK_TOP: usize = 20;

#[derive(Debug, Clone)]
pub struct Query {
    /// The track the search is for; it plays first in a mix made from a match.
    pub track: TrackId,
    /// `VocalsFirst`: the vocals of `track` over the instrumentals of the candidates (old vocals
    /// in the new song). `InstrumentalFirst`: the vocals of the candidates over the instrumental
    /// of `track` (new vocals in the old song).
    pub order: MashupOrder,
    /// Fixed vocal phrase (start bar); `None` = the best vocal phrases.
    pub vocal_bar: Option<i32>,
    /// The other tracks; `None` = every track with stems.
    pub candidates: Option<Vec<TrackId>>,
    pub opts: MatchOptions,
    pub rerank: bool,
}

pub fn rerank_available() -> bool {
    clubcraft_analysis::cocola::Cocola::available()
}

/// Attach the vocal map and the structure labels of a track that gives the vocals: from the
/// cache, else computed (the map ~1 s; the labels ~30 s, only when `labels` is set).
fn prepare_vocals(lib: &Library, analyses: &HashMap<TrackId, Analysis>, id: TrackId, f: &mut MashupFeatures, labels: bool, cancel: &AtomicBool, progress: &mut dyn FnMut(&str)) -> Result<()> {
    let t = lib.track(id)?.context("unknown track")?;
    let a = analyses.get(&id).context("the track is not analysed")?;
    let mut map = crate::pipeline::cached_vocal_map(&t.hash, a);
    let mut lab = crate::pipeline::cached_labels(&t.hash, a);
    if map.is_none() || (labels && lab.is_none()) {
        progress(&t.display_name());
        let _heavy = crate::jobs::heavy();
        if map.is_none() {
            map = crate::pipeline::vocal_map(lib, id).unwrap_or_else(|e| {
                log::warn!("vocal map: {e:#}");
                None
            });
        }
        if labels && lab.is_none() && !cancel.load(Ordering::Relaxed) {
            lab = crate::pipeline::labels(lib, id).unwrap_or_else(|e| {
                log::warn!("structure labels: {e:#}");
                None
            });
        }
    }
    if let Some(m) = map {
        f.set_vocal_map(m);
    }
    if let Some(l) = lab {
        f.set_labels(l);
    }
    Ok(())
}

/// Run a query (blocking; call from a worker thread). `progress(fraction, label)`.
pub fn run(q: &Query, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Vec<Match>> {
    let lib = Library::open_default()?;
    let analyses = lib.all_analyses()?;
    let has_stems = |id: TrackId| analyses.get(&id).is_some_and(|a| a.stems_model.is_some());
    if !has_stems(q.track) {
        bail!("the track has no stems yet; separate it first");
    }
    let mut ids: Vec<TrackId> = match &q.candidates {
        Some(c) => c.clone(),
        None => lib.tracks()?.iter().map(|t| t.id).collect(),
    };
    ids.retain(|&id| has_stems(id) || id == q.track);
    if !ids.contains(&q.track) {
        ids.push(q.track);
    }
    // features (computed for tracks separated before the finder existed: ~1 s each)
    let mut feats: HashMap<TrackId, MashupFeatures> = HashMap::new();
    let n = ids.len();
    for (k, &id) in ids.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let t = lib.track(id)?.context("unknown track")?;
        let a = &analyses[&id];
        let f = match crate::pipeline::cached_mashup_features(&t.hash, a) {
            Some(f) => Some(f),
            None => {
                progress(0.5 * k as f32 / n as f32, &format!("Preparing {} ({}/{n})", t.display_name(), k + 1));
                let _heavy = crate::jobs::heavy();
                crate::pipeline::mashup_features(&lib, id).unwrap_or_else(|e| {
                    log::warn!("mashup features {}: {e:#}", t.display_name());
                    None
                })
            }
        };
        if let Some(f) = f {
            feats.insert(id, f);
        }
    }
    let rerank_on = q.rerank && rerank_available();
    // re-ranking always looks at the best RERANK_TOP, the list is cut afterwards
    let opts = MatchOptions { max_results: if rerank_on { q.opts.max_results.max(RERANK_TOP) } else { q.opts.max_results }, order: q.order, ..q.opts.clone() };
    let units_of = |f: &MashupFeatures, max: usize| match q.vocal_bar {
        Some(b) => vec![mm::unit_at(f, b, &opts)],
        None => mm::vocal_units(f, &opts, max),
    };
    let log_units = |id: TrackId, units: &[mm::VocalUnit]| {
        for u in units {
            log::debug!("mashup: vocal unit of {id}: bar {} ({} bars, {}) quality {:.3}", u.bar + 1, u.bars, u.role.label(), u.quality);
        }
    };
    let mut found = match q.order {
        MashupOrder::VocalsFirst => {
            // where the vocal track really sings (lines, loops, leakage) and what its sections
            // are (chorus, verse): both pick the phrases, both are cached after the first search
            let mut f = feats.remove(&q.track).context("no features for the track")?;
            prepare_vocals(&lib, &analyses, q.track, &mut f, true, cancel, &mut |name| progress(0.55, &format!("Listening to the vocals of {name} (once, up to half a minute)")))?;
            progress(0.6, "Searching");
            let units = units_of(&f, 6);
            if units.is_empty() {
                bail!("no vocal phrase of {} bars with a clean start and end found in the track", q.opts.bars);
            }
            log_units(q.track, &units);
            let cands: Vec<(i64, &MashupFeatures)> = feats.iter().map(|(id, g)| (*id, g)).collect();
            let found = mm::find((q.track, &f), &units, &cands, &opts);
            feats.insert(q.track, f);
            found
        }
        MashupOrder::InstrumentalFirst => {
            // every other track's vocals over this instrumental: each needs its vocal map (once,
            // ~1 s); the structure labels are used where they are cached
            let inst = feats.remove(&q.track).context("no features for the track")?;
            let mut voc: Vec<TrackId> = feats.iter().filter(|(_, g)| mm::tempo_ratio(g.bpm, inst.bpm, opts.tempo_tolerance).is_some()).map(|(id, _)| *id).collect();
            voc.sort_unstable();
            let nv = voc.len();
            let mut found = Vec::new();
            for (k, id) in voc.into_iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    bail!("cancelled");
                }
                let Some(f) = feats.get_mut(&id) else { continue };
                prepare_vocals(&lib, &analyses, id, f, false, cancel, &mut |name| progress(0.5 + 0.1 * k as f32 / nv.max(1) as f32, &format!("Listening to the vocals of {name} ({}/{nv}, once)", k + 1)))?;
                let units = units_of(f, 4);
                log_units(id, &units);
                found.extend(mm::find((id, f), &units, &[(q.track, &inst)], &opts));
            }
            feats.insert(q.track, inst);
            found.sort_by(|a, b| b.score.total_cmp(&a.score));
            found.truncate(opts.max_results);
            found
        }
    };
    if rerank_on && !found.is_empty() {
        if let Err(e) = rerank(&lib, &feats, &mut found, cancel, progress) {
            log::warn!("re-ranking: {e:#}");
        }
    }
    found.truncate(q.opts.max_results);
    for m in &found {
        log::debug!("mashup: vocals {} bar {} over inst {} bar {} key {:+} ratio {:.4} score {:.3} fit {:.3} placement {:.3} phrase {:.3} learned {:?}", m.vocal_track, m.vocal_bar + 1, m.inst_track, m.inst_bar + 1, m.key_shift, m.tempo_ratio, m.score, m.fit, m.placement, m.phrase, m.learned);
    }
    progress(1.0, "Done");
    Ok(found)
}

/// Two 5 s windows of the phrase: the loudest vocal bar of each half.
fn snippet_bars(v: &MashupFeatures, m: &Match) -> [i32; 2] {
    let half = (m.bars as i32 / 2).max(1);
    let pick = |a: i32, b: i32| (a..b).max_by(|x, y| v.bar(m.vocal_bar + x).map(|q| q.vocal_db).unwrap_or(-99.0).total_cmp(&v.bar(m.vocal_bar + y).map(|q| q.vocal_db).unwrap_or(-99.0))).unwrap_or(a);
    [pick(0, half), pick(half, (m.bars as i32 - 2).max(half + 1))]
}

const SR: f64 = clubcraft_core::SAMPLE_RATE as f64;

/// Mono stems of a cached stem file for the given time windows (s, length s):
/// returns (vocals, instrumental) per window, 44.1 kHz.
fn stem_windows(hash: &str, windows: &[(f64, f64)]) -> Result<Vec<(Vec<f32>, Vec<f32>)>> {
    let st = clubcraft_stems::load_stem_track(&clubcraft_core::library::stems_path(hash))?;
    Ok(windows
        .iter()
        .map(|&(t, len)| {
            let a = (t.max(0.0) * SR) as usize;
            let b = ((t + len) * SR) as usize;
            let (mut voc, mut inst) = (Vec::with_capacity(b - a), Vec::with_capacity(b - a));
            for j in a..b {
                let f = st.frame(j).unwrap_or([0.0; 8]);
                voc.push(0.5 * (f[6] + f[7]));
                inst.push(0.5 * (f[0] + f[1] + f[2] + f[3] + f[4] + f[5]));
            }
            (voc, inst)
        })
        .collect())
}

const PRE: f64 = 0.3;

/// The vocals as the mix plays them: `rate` = playback speed, `semis` = pitch shift; 5 s at 44.1 kHz.
/// `x` starts `PRE` seconds before the snippet.
fn stretch_snippet(x: &[f32], rate: f64, semis: i32) -> Vec<f32> {
    let pre = (PRE * SR) as usize;
    let n = (clubcraft_analysis::cocola::CLIP as f64 / clubcraft_analysis::cocola::SR as f64 * SR) as usize;
    if (rate - 1.0).abs() < 1e-4 && semis == 0 {
        let mut o = x.get(pre..(pre + n).min(x.len())).unwrap_or(&[]).to_vec();
        o.resize(n, 0.0);
        return o;
    }
    let mut st = clubcraft_engine::stretch::Stretcher::new(1, (SR * 0.12) as usize, (SR * 0.03) as usize, false);
    st.set_transpose_factor(2f32.powf(semis as f32 / 12.0));
    let (il, ol) = (st.input_latency(), st.output_latency());
    st.seek(&x[pre.saturating_sub(il)..pre], rate);
    let out_n = n + ol;
    let in_n = (out_n as f64 * rate).round() as usize;
    let mut inp = x.get(pre..(pre + in_n).min(x.len())).unwrap_or(&[]).to_vec();
    inp.resize(in_n, 0.0);
    let mut out = vec![0f32; out_n];
    st.process(&inp, &mut out);
    out.drain(..ol);
    out
}

fn rerank(lib: &Library, feats: &HashMap<TrackId, MashupFeatures>, found: &mut [Match], cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<()> {
    use clubcraft_analysis::cocola;
    use rayon::prelude::*;
    progress(0.62, "Loading the AI model");
    let model = cocola::Cocola::load_default()?;
    let top = found.len().min(RERANK_TOP);
    let clip_secs = cocola::CLIP as f64 / cocola::SR as f64;
    // vocal snippets for every match (one stem file per vocal track): source windows include
    // room for stretching
    let mut vsrc: Vec<Vec<f32>> = vec![Vec::new(); 2 * top];
    let mut by_vocal: HashMap<TrackId, Vec<usize>> = HashMap::new();
    for (k, m) in found[..top].iter().enumerate() {
        by_vocal.entry(m.vocal_track).or_default().push(k);
    }
    for (vid, ks) in by_vocal {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let v = &feats[&vid];
        let vt = lib.track(vid)?.context("vocal track")?;
        let wins: Vec<(f64, f64)> = ks.iter().flat_map(|&k| snippet_bars(v, &found[k]).map(|j| (v.bar_time(found[k].vocal_bar + j) - PRE, PRE + clip_secs * found[k].tempo_ratio.max(1.0) * 1.1 + 0.5))).collect();
        for (w, (voc, _)) in stem_windows(&vt.hash, &wins)?.into_iter().enumerate() {
            vsrc[2 * ks[w / 2] + w % 2] = voc;
        }
    }
    let vfeat: Vec<Vec<f32>> = found[..top]
        .par_iter()
        .enumerate()
        .flat_map_iter(|(k, m)| {
            (0..2).map(move |w| (k, m, w)).collect::<Vec<_>>()
        })
        .map(|(k, m, w)| {
            let s = stretch_snippet(&vsrc[2 * k + w], m.tempo_ratio, m.key_shift);
            cocola::features(&cocola::clip_from(&s, 0))
        })
        .collect();
    let vemb = model.embed(&vfeat, false)?;
    // instrumental snippets, one stem file at a time
    let mut by_track: HashMap<TrackId, Vec<usize>> = HashMap::new();
    for (k, m) in found[..top].iter().enumerate() {
        by_track.entry(m.inst_track).or_default().push(k);
    }
    let ntracks = by_track.len();
    for (done, (tid, ks)) in by_track.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let t = lib.track(tid)?.context("track")?;
        progress(0.65 + 0.33 * done as f32 / ntracks as f32, &format!("AI listening: {}", t.display_name()));
        let f = &feats[&tid];
        let wins: Vec<(f64, f64)> = ks.iter().flat_map(|&k| snippet_bars(&feats[&found[k].vocal_track], &found[k]).map(|j| (f.bar_time(found[k].inst_bar + j), clip_secs + 0.1))).collect();
        let src = stem_windows(&t.hash, &wins)?;
        let ifeat: Vec<Vec<f32>> = src.par_iter().map(|(_, inst)| cocola::features(&cocola::clip_from(inst, 0))).collect();
        let iemb = model.embed(&ifeat, false)?;
        for (n, &k) in ks.iter().enumerate() {
            let l = (0..2).map(|w| cocola::normalized(model.score(&vemb[2 * k + w], &iemb[2 * n + w]))).sum::<f32>() / 2.0;
            found[k].set_learned(l, LEARNED_WEIGHT);
        }
    }
    found[..top].sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(())
}

// ------------------------------------------------------------------ matches → mixes

/// Grid times of a match: (vocal phrase start in the vocal track, instrumental start), seconds.
pub fn match_times(m: &Match, analyses: &HashMap<TrackId, Analysis>) -> Option<(f64, f64)> {
    let gv = analyses.get(&m.vocal_track)?.grid?;
    let gi = analyses.get(&m.inst_track)?.grid?;
    Some((gv.time_of_bar(m.vocal_bar as f64), gi.time_of_bar(m.inst_bar as f64)))
}

/// The two tracks of `m` in mix order with the time each one meets the other (the first
/// track's mix-out, the second track's mix-in), and the transition style.
fn ordered(m: &Match, analyses: &HashMap<TrackId, Analysis>) -> Option<((TrackId, f64), (TrackId, f64), TransitionStyle)> {
    let (tv, ti) = match_times(m, analyses)?;
    Some(match m.order {
        MashupOrder::VocalsFirst => ((m.vocal_track, tv), (m.inst_track, ti), TransitionStyle::Mashup),
        MashupOrder::InstrumentalFirst => ((m.inst_track, ti), (m.vocal_track, tv), TransitionStyle::MashupIn),
    })
}

/// Key shift of the second track, given the first track's: the vocals sound `m.key_shift`
/// semitones above their key relative to the instrumental.
fn second_shift(m: &Match, first: i32) -> i32 {
    match m.order {
        MashupOrder::VocalsFirst => first - m.key_shift,
        MashupOrder::InstrumentalFirst => first + m.key_shift,
    }
}

/// A two-track mix in the match's order: the first track plays `lead_bars` before the mashup,
/// then the vocals sing over the instrumental, and the second track plays on to its end.
pub fn mashup_mix(name: &str, m: &Match, analyses: &HashMap<TrackId, Analysis>, lead_bars: i32) -> Option<Mix> {
    let ((a, ta), (b, tb), style) = ordered(m, analyses)?;
    let ga = analyses.get(&a)?.grid?;
    let start = ga.time_of_bar((ga.bar_at(ta).round() as i32 - lead_bars).max(0) as f64).max(0.0);
    let shift_a = if a == m.vocal_track { m.key_shift } else { 0 };
    let mut mix = Mix { name: name.to_string(), tempo: TempoMode::Glide, ..Default::default() };
    mix.tracks.push(MixTrack { track_id: a, mix_in: start.min(ta), mix_out: ta, key_shift: shift_a, gain_db: 0.0, stems: Vec::new() });
    mix.tracks.push(MixTrack { track_id: b, mix_in: tb, mix_out: analyses.get(&b).map(|x| x.duration).unwrap_or(tb + 60.0).max(tb + 1.0), key_shift: second_shift(m, shift_a), gain_db: 0.0, stems: Vec::new() });
    mix.transitions.push(Transition::new(style, m.bars as u32 * 4));
    mix.normalize();
    Some(mix)
}

/// Make transition `k` of `mix` (tracks `k` → `k + 1`) the mashup `m`.
pub fn apply_to_transition(mix: &mut Mix, k: usize, m: &Match, analyses: &HashMap<TrackId, Analysis>) -> bool {
    let Some(((a, ta), (b, tb), style)) = ordered(m, analyses) else { return false };
    if k + 1 >= mix.tracks.len() || mix.tracks[k].track_id != a || mix.tracks[k + 1].track_id != b {
        return false;
    }
    let shift = second_shift(m, mix.tracks[k].key_shift);
    mix.tracks[k].mix_out = ta.max(mix.tracks[k].mix_in + 1.0);
    let t = &mut mix.tracks[k + 1];
    t.mix_in = tb;
    t.mix_out = t.mix_out.max(tb + 1.0);
    t.key_shift = shift;
    mix.transitions[k] = Transition::new(style, m.bars as u32 * 4);
    mix.normalize();
    true
}

/// Insert the second track of `m` after its first track (at index `k`) of `mix`.
pub fn insert_after(mix: &mut Mix, k: usize, m: &Match, analyses: &HashMap<TrackId, Analysis>) -> bool {
    let Some(((a, ta), (b, tb), style)) = ordered(m, analyses) else { return false };
    if mix.tracks.get(k).map(|t| t.track_id) != Some(a) {
        return false;
    }
    let dur = analyses.get(&b).map(|x| x.duration).unwrap_or(tb + 60.0);
    let out = analyses.get(&b).map(clubcraft_core::planner::default_mix_out).unwrap_or(dur).max(tb + 1.0);
    let shift = second_shift(m, mix.tracks[k].key_shift);
    mix.tracks[k].mix_out = ta.max(mix.tracks[k].mix_in + 1.0);
    mix.tracks.insert(k + 1, MixTrack { track_id: b, mix_in: tb, mix_out: out, key_shift: shift, gain_db: 0.0, stems: Vec::new() });
    mix.transitions.insert(k, Transition::new(style, m.bars as u32 * 4));
    mix.normalize();
    true
}
