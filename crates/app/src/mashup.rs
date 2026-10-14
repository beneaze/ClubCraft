//! Mashup finder: which part of which track can carry the vocals of another track.
//!
//! 1. Per-bar stem features ([`clubcraft_analysis::mashup`]), cached next to the stems and
//!    computed on demand for tracks separated before the finder existed.
//! 2. Rule-based search over the library: harmony of the key-shifted vocals against the
//!    instrumental bar by bar, masking, residual vocals, tempo and key cost.
//! 3. Optional re-ranking of the best candidates with COCOLA ([`clubcraft_analysis::cocola`]):
//!    5 s snippets of the vocals (pitch-shifted and time-stretched like the mix would play them)
//!    and of the instrumental are embedded and scored; the final score blends both (0.6 rule,
//!    0.4 learned: AUC 0.78 → 0.84 against random placements on the reference library).
//!
//! A match becomes a two-track mix (or part of a mix) joined by a [`TransitionStyle::Mashup`].

use anyhow::{bail, Context, Result};
use clubcraft_analysis::mashup::{self as mm, MashupFeatures, Match, MatchOptions};
use clubcraft_core::{Analysis, Library, Mix, MixTrack, TempoMode, TrackId, Transition, TransitionStyle};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

/// Weight of the learned score in the final ranking.
const LEARNED_WEIGHT: f32 = 0.4;
/// Candidates re-ranked with COCOLA.
const RERANK_TOP: usize = 20;

#[derive(Debug, Clone)]
pub struct Query {
    pub vocal: TrackId,
    /// Fixed vocal phrase (start bar); `None` = the strongest vocal phrases.
    pub vocal_bar: Option<i32>,
    /// Instrumental candidates; `None` = every track with stems.
    pub candidates: Option<Vec<TrackId>>,
    pub opts: MatchOptions,
    pub rerank: bool,
}

pub fn rerank_available() -> bool {
    clubcraft_analysis::cocola::Cocola::available()
}

/// Run a query (blocking; call from a worker thread). `progress(fraction, label)`.
pub fn run(q: &Query, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Vec<Match>> {
    let lib = Library::open_default()?;
    let analyses = lib.all_analyses()?;
    let has_stems = |id: TrackId| analyses.get(&id).is_some_and(|a| a.stems_model.is_some());
    if !has_stems(q.vocal) {
        bail!("the vocal track has no stems yet; separate it first");
    }
    let mut ids: Vec<TrackId> = match &q.candidates {
        Some(c) => c.clone(),
        None => lib.tracks()?.iter().map(|t| t.id).collect(),
    };
    ids.retain(|&id| has_stems(id) || id == q.vocal);
    if !ids.contains(&q.vocal) {
        ids.push(q.vocal);
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
                progress(0.6 * k as f32 / n as f32, &format!("Preparing {} ({}/{n})", t.display_name(), k + 1));
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
    let v = feats.get(&q.vocal).context("no features for the vocal track")?;
    progress(0.6, "Searching");
    let regions = match q.vocal_bar {
        Some(b) => vec![b],
        None => mm::vocal_regions(v, &q.opts, 3),
    };
    if regions.is_empty() {
        bail!("no vocal phrase of {} bars found in the vocal track", q.opts.bars);
    }
    let cands: Vec<(i64, &MashupFeatures)> = feats.iter().map(|(id, f)| (*id, f)).collect();
    let rerank_on = q.rerank && rerank_available();
    // re-ranking always looks at the best RERANK_TOP, the list is cut afterwards
    let opts = MatchOptions { max_results: if rerank_on { q.opts.max_results.max(RERANK_TOP) } else { q.opts.max_results }, ..q.opts.clone() };
    let mut found = mm::find((q.vocal, v), &regions, &cands, &opts);
    if rerank_on && !found.is_empty() {
        if let Err(e) = rerank(&lib, &feats, &mut found, cancel, progress) {
            log::warn!("re-ranking: {e:#}");
        }
    }
    found.truncate(q.opts.max_results);
    for m in &found {
        log::debug!("mashup: inst {} bar {} vocal bar {} key {:+} ratio {:.4} score {:.3} learned {:?}", m.inst_track, m.inst_bar, m.vocal_bar, m.key_shift, m.tempo_ratio, m.score, m.learned);
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
    let vid = found[0].vocal_track;
    let v = &feats[&vid];
    let vt = lib.track(vid)?.context("vocal track")?;
    let clip_secs = cocola::CLIP as f64 / cocola::SR as f64;
    // vocal snippets for every (match): source windows include room for stretching
    let vwin: Vec<(f64, f64)> = found[..top]
        .iter()
        .flat_map(|m| snippet_bars(v, m).map(|j| (v.bar_time(m.vocal_bar + j) - PRE, PRE + clip_secs * m.tempo_ratio.max(1.0) * 1.1 + 0.5)))
        .collect();
    let vsrc = stem_windows(&vt.hash, &vwin)?;
    let vfeat: Vec<Vec<f32>> = found[..top]
        .par_iter()
        .enumerate()
        .flat_map_iter(|(k, m)| {
            (0..2).map(move |w| (k, m, w)).collect::<Vec<_>>()
        })
        .map(|(k, m, w)| {
            let s = stretch_snippet(&vsrc[2 * k + w].0, m.tempo_ratio, m.key_shift);
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
        let wins: Vec<(f64, f64)> = ks.iter().flat_map(|&k| snippet_bars(v, &found[k]).map(|j| (f.bar_time(found[k].inst_bar + j), clip_secs + 0.1))).collect();
        let src = stem_windows(&t.hash, &wins)?;
        let ifeat: Vec<Vec<f32>> = src.par_iter().map(|(_, inst)| cocola::features(&cocola::clip_from(inst, 0))).collect();
        let iemb = model.embed(&ifeat, false)?;
        for (n, &k) in ks.iter().enumerate() {
            let l = (0..2).map(|w| cocola::normalized(model.score(&vemb[2 * k + w], &iemb[2 * n + w]))).sum::<f32>() / 2.0;
            let m = &mut found[k];
            m.learned = Some(l);
            m.score = (1.0 - LEARNED_WEIGHT) * m.score + LEARNED_WEIGHT * l;
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

/// A two-track mix: the vocal track plays `lead_bars` before its phrase, then sings over the
/// instrumental, which continues to its end.
pub fn mashup_mix(name: &str, m: &Match, analyses: &HashMap<TrackId, Analysis>, lead_bars: i32) -> Option<Mix> {
    let (tv, ti) = match_times(m, analyses)?;
    let gv = analyses.get(&m.vocal_track)?.grid?;
    let start = gv.time_of_bar((m.vocal_bar - lead_bars).max(0) as f64).max(0.0);
    let mut mix = Mix { name: name.to_string(), tempo: TempoMode::Glide, ..Default::default() };
    mix.tracks.push(MixTrack { track_id: m.vocal_track, mix_in: start.min(tv), mix_out: tv, key_shift: m.key_shift, gain_db: 0.0 });
    mix.tracks.push(MixTrack { track_id: m.inst_track, mix_in: ti, mix_out: analyses.get(&m.inst_track).map(|a| a.duration).unwrap_or(ti + 60.0).max(ti + 1.0), key_shift: 0, gain_db: 0.0 });
    mix.transitions.push(Transition::new(TransitionStyle::Mashup, m.bars as u32 * 4));
    mix.normalize();
    Some(mix)
}

/// Make transition `k` of `mix` (tracks `k` → `k + 1`) the mashup `m`.
pub fn apply_to_transition(mix: &mut Mix, k: usize, m: &Match, analyses: &HashMap<TrackId, Analysis>) -> bool {
    let Some((tv, ti)) = match_times(m, analyses) else { return false };
    if k + 1 >= mix.tracks.len() || mix.tracks[k].track_id != m.vocal_track || mix.tracks[k + 1].track_id != m.inst_track {
        return false;
    }
    let shift = mix.tracks[k].key_shift - m.key_shift;
    mix.tracks[k].mix_out = tv.max(mix.tracks[k].mix_in + 1.0);
    let b = &mut mix.tracks[k + 1];
    b.mix_in = ti;
    b.mix_out = b.mix_out.max(ti + 1.0);
    b.key_shift = shift;
    mix.transitions[k] = Transition::new(TransitionStyle::Mashup, m.bars as u32 * 4);
    mix.normalize();
    true
}

/// Insert the instrumental of `m` after the vocal track (at index `k`) of `mix`.
pub fn insert_after(mix: &mut Mix, k: usize, m: &Match, analyses: &HashMap<TrackId, Analysis>) -> bool {
    let Some((tv, ti)) = match_times(m, analyses) else { return false };
    if mix.tracks.get(k).map(|t| t.track_id) != Some(m.vocal_track) {
        return false;
    }
    let dur = analyses.get(&m.inst_track).map(|a| a.duration).unwrap_or(ti + 60.0);
    let out = analyses.get(&m.inst_track).map(clubcraft_core::planner::default_mix_out).unwrap_or(dur).max(ti + 1.0);
    let shift = mix.tracks[k].key_shift - m.key_shift;
    mix.tracks[k].mix_out = tv.max(mix.tracks[k].mix_in + 1.0);
    mix.tracks.insert(k + 1, MixTrack { track_id: m.inst_track, mix_in: ti, mix_out: out, key_shift: shift, gain_db: 0.0 });
    mix.transitions.insert(k, Transition::new(TransitionStyle::Mashup, m.bars as u32 * 4));
    mix.normalize();
    true
}
