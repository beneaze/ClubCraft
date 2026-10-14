//! Import from DJ.Studio (`~/Music/DJ.Studio/Database`): library tracks with their hot cues, and
//! projects as ClubCraft mixes (track order, mix-in/out points, transition lengths and styles,
//! key shifts, the tracks' own stem automation). Read-only: DJ.Studio's files are never modified.
//!
//! Format notes: every table is a folder of JSON files named by record key;
//! `audio-library` and `track-structures` records sit in hash buckets = (sum of the key's char
//! codes) % 256 as unpadded lowercase hex.

use anyhow::{Context, Result};
use clubcraft_core::{Cue, CueKind, CueSource, Lane, Library, Mix, MixTrack, Param, Side, StemKind, StemLane, TempoMode, TrackId, Transition, TransitionStyle};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// DJ.Studio keeps its database in `<Music>/DJ.Studio/Database` on macOS and Windows.
pub fn default_root() -> PathBuf {
    let music = dirs::audio_dir().or_else(|| dirs::home_dir().map(|h| h.join("Music"))).unwrap_or_default();
    music.join("DJ.Studio").join("Database")
}

pub fn bucket(key: &str) -> String {
    format!("{:x}", key.chars().map(|c| c as u32).sum::<u32>() % 256)
}

/// Parse a JSON file, tolerating trailing garbage (DJ.Studio leaves some behind).
/// A record file can hold several versions back to back (DJ.Studio appends); the last is current.
fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::Deserializer::from_str(&text).into_iter::<Value>().map_while(|v| v.ok()).last()
}

/// DJ.Studio key integer → key. 0..11 major by pitch class, 12 + i = relative minor of major i.
pub fn key_from_int(k: i64) -> Option<clubcraft_core::Key> {
    match k {
        0..=11 => Some(clubcraft_core::Key::new(k as u8, false)),
        12..=23 => Some(clubcraft_core::Key::new(((k - 12 + 9) % 12) as u8, true)),
        _ => None,
    }
}

/// A DJ.Studio library track.
#[derive(Debug, Clone)]
pub struct DjsTrack {
    pub key: String,
    pub path: PathBuf,
    pub bpm: Option<f64>,
    pub key_sig: Option<clubcraft_core::Key>,
    /// Beat 0 (first downbeat) time and BPM of the grid.
    pub beat0: Option<(f64, f64)>,
    pub hot_cues: Vec<(f64, String)>,
}

impl DjsTrack {
    fn beat_time(&self, beat: f64) -> Option<f64> {
        self.beat0.map(|(t0, bpm)| t0 + beat * 60.0 / bpm)
    }
}

fn parse_track(v: &Value) -> Option<DjsTrack> {
    let key = v.get("key")?.as_str()?.to_string();
    let path = ["/externalRec/fileLocationPath", "/lastKnownFilePath", "/externalRec/lastKnownFilePath"]
        .iter()
        .filter_map(|ptr| v.pointer(ptr).and_then(|p| p.as_str()).filter(|p| !p.is_empty()))
        .find(|p| Path::new(p).exists())
        .or_else(|| v.pointer("/externalRec/fileLocationPath").and_then(|p| p.as_str()).filter(|p| !p.is_empty()))?;
    let beat0 = v.get("bpmLine").and_then(|l| l.as_array()).and_then(|l| l.first()).and_then(|p| {
        let t = p.get("time")?.as_f64()?;
        let b = p.get("value")?.as_f64()?;
        let nr = p.get("beatNr").and_then(|n| n.as_f64()).unwrap_or(0.0);
        Some((t - nr * 60.0 / b, b))
    });
    let mut t = DjsTrack {
        key,
        path: PathBuf::from(path),
        bpm: v.get("bpm").and_then(|b| b.as_f64()).filter(|b| *b > 0.0),
        key_sig: v.get("camelotKey").and_then(|k| k.as_i64()).and_then(key_from_int),
        beat0,
        hot_cues: vec![],
    };
    if let Some(cues) = v.pointer("/cueData/hotCuePoints").and_then(|c| c.as_array()) {
        for c in cues {
            let beat = c.get("beat").and_then(|b| b.as_f64()).unwrap_or(-1.0);
            if beat < 0.0 {
                continue;
            }
            let time = c.get("time").and_then(|x| x.as_f64()).filter(|x| *x >= 0.0).or_else(|| t.beat_time(beat));
            if let Some(time) = time {
                let name = c.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                t.hot_cues.push((time, name));
            }
        }
    }
    Some(t)
}

pub fn read_library(root: &Path) -> Vec<DjsTrack> {
    let mut out = Vec::new();
    let dir = root.join("audio-library-table");
    for e in walkdir::WalkDir::new(&dir).min_depth(2).max_depth(2).into_iter().flatten() {
        if !e.file_type().is_file() || e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if let Some(t) = read_json(e.path()).as_ref().and_then(parse_track) {
            out.push(t);
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct DjsProject {
    pub key: String,
    pub name: String,
    pub modified: String,
    pub track_count: usize,
}

pub fn read_projects(root: &Path) -> Vec<DjsProject> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root.join("projects-table")) else { return out };
    for e in rd.flatten() {
        let Some(v) = read_json(&e.path()) else { continue };
        if v.get("isTemporary").and_then(|x| x.as_bool()).unwrap_or(false) || v.get("isTemplate").and_then(|x| x.as_bool()).unwrap_or(false) {
            continue;
        }
        let n = v.get("mixList").and_then(|l| l.as_array()).map(|l| l.len()).unwrap_or(0);
        if n == 0 {
            continue;
        }
        out.push(DjsProject {
            key: v.get("key").and_then(|k| k.as_str()).unwrap_or_default().to_string(),
            name: v.get("name").and_then(|k| k.as_str()).unwrap_or("Untitled").to_string(),
            modified: v.get("lastModified").and_then(|k| k.as_str()).unwrap_or_default().to_string(),
            track_count: n,
        });
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

fn style_from_effects(effects: &[Value]) -> TransitionStyle {
    let names: Vec<&str> = effects.iter().filter_map(|e| e.get("effectName").and_then(|n| n.as_str())).collect();
    let has = |p: &str| names.iter().any(|n| n.starts_with(p));
    if names.is_empty() {
        TransitionStyle::Cut
    } else if has("AE_Stems") {
        TransitionStyle::StemBlend
    } else if has("AE_Bass_Swap") || has("AE_Bass_") {
        TransitionStyle::BassSwap
    } else if has("AE_Echo") {
        TransitionStyle::EchoOut
    } else if has("AE_LowPass_Filter") || has("AE_HighPass_Filter") {
        TransitionStyle::FilterOut
    } else if has("AE_CrossFade") || has("AE_Fade") {
        TransitionStyle::Crossfade
    } else {
        TransitionStyle::BassSwap
    }
}

// ------------------------------------------------------------------ transitions

/// Volume in dB → ClubCraft's volume lane value (gain = value², see `Channel::lane_volume`).
fn vol_value(db: f32) -> f32 {
    if db <= -49.0 {
        0.0
    } else {
        10f32.powf(db / 20.0).sqrt().min(1.0)
    }
}

/// EQ band linear gain (DJ.Studio: 1 flat, 0.04 kill, up to 1.96) → ClubCraft's EQ knob.
fn eq_value(g: f32) -> f32 {
    clubcraft_engine::dsp::eq_gain_to_knob(g.max(0.0))
}

/// DJ.Studio filter level (−1 low-pass … 0 off … +1 high-pass) → ClubCraft's filter knob.
fn filter_value(level: f32) -> f32 {
    (0.5 + 0.5 * level).clamp(0.0, 1.0)
}

/// Sample `f(t)` for t in 0..1 over `len` beats (curves that aren't piecewise linear).
fn sampled(len: f32, n: usize, f: impl Fn(f32) -> f32) -> Vec<(f32, f32)> {
    (0..=n).map(|i| {
        let t = i as f32 / n as f32;
        (t * len, f(t))
    }).collect()
}

/// Stored automation lane of a transition side (`_0` outgoing, `_1` incoming), window beats.
fn lane_points(root: &Path, ae_key: &str, side: usize, control: &str) -> Option<Vec<(f32, f32)>> {
    let v = read_json(&root.join("automation-data-table").join(format!("{ae_key}_{side}")).join(control))?;
    let pts: Vec<(f32, f32)> = v.get("points")?.as_array()?.iter().filter_map(|p| Some((p.get("time")?.as_f64()? as f32, p.get("value")?.as_f64()? as f32))).collect();
    (!pts.is_empty()).then_some(pts)
}

/// A track's own stem lanes (`<proj>/<item>/virtual-stems-*`, in local track beats = `mixMap`
/// indices) in track seconds. Only lanes that do something are kept; the 4-stem lanes win over
/// the HQ vocals / instrumental ones.
fn track_stem_lanes(root: &Path, item_key: &str, md: &Value) -> Vec<StemLane> {
    let map: Vec<(f64, f64)> = md.get("mixMap").and_then(|m| m.as_array()).map(|m| m.iter().map(|b| (b.get("time").and_then(|t| t.as_f64()).unwrap_or(0.0), b.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0))).collect()).unwrap_or_default();
    if map.is_empty() {
        return Vec::new();
    }
    let secs = |beat: f64| {
        let i = (beat.floor().max(0.0) as usize).min(map.len() - 1);
        let (t, d) = map[i];
        t + (beat - i as f64).max(0.0) * d
    };
    let points = |control: &str| -> Option<Vec<(f32, f32)>> {
        let v = read_json(&root.join("automation-data-table").join(item_key).join(control))?;
        Some(v.get("points")?.as_array()?.iter().filter_map(|p| Some((secs(p.get("time")?.as_f64()?) as f32, p.get("value")?.as_f64()?.clamp(0.0, 1.0) as f32))).collect())
    };
    // the lanes only play while the track's stems are switched on
    if !points("virtual-stems-on").is_some_and(|p| p.iter().any(|q| q.1 > 0.5)) {
        return Vec::new();
    }
    let read = |control: &str| points(control).filter(|p| p.iter().any(|q| (q.1 - 1.0).abs() > 1e-3));
    let mut lanes: Vec<StemLane> = Vec::new();
    let mut put = |stem: StemKind, points: Vec<(f32, f32)>| {
        if !lanes.iter().any(|l| l.stem == stem) {
            lanes.push(StemLane { stem, points });
        }
    };
    for (control, stem) in [("virtual-stems-drums", StemKind::Drums), ("virtual-stems-bass", StemKind::Bass), ("virtual-stems-melody", StemKind::Other), ("virtual-stems-vocals", StemKind::Vocals)] {
        if let Some(p) = read(control) {
            put(stem, p);
        }
    }
    for control in ["virtual-stems-vocals-hq"] {
        if let Some(p) = read(control) {
            put(StemKind::Vocals, p);
        }
    }
    for control in ["virtual-stems-instrumental", "virtual-stems-instrumental-hq"] {
        if let Some(p) = read(control) {
            for stem in [StemKind::Drums, StemKind::Bass, StemKind::Other] {
                put(stem, p.clone());
            }
        }
    }
    lanes
}

/// Parameters of a generated DJ.Studio stem effect (`AE_Stems<Stem>_<Full|Swap|Crossfade>`).
#[derive(Clone, Copy, Debug)]
struct StemParams {
    level: f32,
    primary_level: f32,
    effect_center: f32,
    primary_effect_center: f32,
    custom_swap_point: f32,
    primary_custom_swap_point: f32,
    beats: f32,
    cross_fade: bool,
}

/// Default (effectCenter, primaryEffectCenter, customSwapPoint, primaryCustomSwapPoint) of each
/// generated stem effect, as DJ.Studio defines them (stored parameters override these).
fn stem_defaults(stem: &str, mode: &str) -> [f32; 4] {
    match (mode, stem) {
        ("Full", _) => [0.0, 1.0, 0.0, 1.0],
        ("Swap", _) => [0.5, 0.5, 0.5, -1.0],
        ("Crossfade", "Drums") => [0.0, 1.0, 0.591_751, 0.405_487],
        ("Crossfade", "Bass") => [0.25, 0.75, 0.863_630, 0.066_995],
        ("Crossfade", "Melody") => [0.25, 0.75, 0.991_287, -1.0],
        ("Crossfade", _) => [0.375, 0.625, 0.946_959, 0.005_610],
        _ => [0.0, 1.0, 0.0, 1.0],
    }
}

/// DJ.Studio's curve for a generated stem effect over a `d`-beat window, per side. The outgoing
/// (primary) track plays at `primaryLevel` until its swap point (`primaryCustomSwapPoint`, or the
/// incoming side's centre when that is −1) and fades linearly to silence by its own centre
/// (`primaryEffectCenter`, minus `beats`). The incoming track is silent until its centre
/// (`effectCenter`) and fades in to `level` by its swap point (`customSwapPoint`, or the
/// outgoing side's centre). Times are rounded to whole beats; without `crossFade` the stem
/// switches at the centre.
fn stem_points(p: &StemParams, primary: bool, d: f32) -> Vec<(f32, f32)> {
    let round = |x: f32| (x + 0.5).floor(); // JavaScript's Math.round
    let (own_center, other_center, level, swap) = if primary {
        (p.primary_effect_center, p.effect_center, p.primary_level, p.primary_custom_swap_point)
    } else {
        (p.effect_center, p.primary_effect_center, p.level, p.custom_swap_point)
    };
    let mut l = round(own_center * d);
    if primary {
        l -= p.beats;
    }
    let (from, to) = if primary { (level, 0.0) } else { (0.0, level) };
    let fe = if swap != -1.0 { round(swap * d) } else { round(other_center * d) };
    let (mut a, mut e) = if primary { (fe, l) } else { (l, fe) };
    if a > e {
        if primary {
            a = e;
        } else {
            e = a;
        }
    }
    let mut pts = vec![(0.0, from)];
    if p.cross_fade && a <= e {
        pts.push((a.clamp(0.0, d), from));
        pts.push((e.clamp(0.0, d), to));
    } else {
        pts.push((l.clamp(0.0, d), from));
        pts.push((l.clamp(0.0, d), to));
    }
    pts.push((d, to));
    pts
}

/// Convert a DJ.Studio transition ("autoEffect") into ClubCraft lanes: every effect with its
/// parameters (volume crossfade, bass swap, filters, echo, stems Full / Swap / Crossfade / None)
/// and, for `_Manual` effects, the stored curves (with their slopes), per side.
pub fn transition_from(root: &Path, ae: &Value) -> Transition {
    let d = ae.get("duration").and_then(|d| d.as_f64()).unwrap_or(32.0).max(1.0) as f32;
    let key = ae.get("key").and_then(|k| k.as_str()).unwrap_or_default().to_string();
    let effects: Vec<Value> = ae.get("effects").and_then(|e| e.as_array()).cloned().unwrap_or_default();
    let manual_all = ae.get("manual").and_then(|m| m.as_bool()).unwrap_or(false);
    let style = style_from_effects(&effects);
    let mut lanes: Vec<Lane> = Vec::new();
    let mut set = |lanes: &mut Vec<Lane>, side: Side, param: Param, pts: Vec<(f32, f32)>| {
        lanes.retain(|l| !(l.side == side && l.param == param));
        lanes.push(Lane::new(side, param, &pts));
    };
    let stored = |side: usize, control: &str| lane_points(root, &key, side, control);
    let eps = 0.01f32.min(d * 0.01);
    for e in &effects {
        let name = e.get("effectName").and_then(|n| n.as_str()).unwrap_or_default();
        let p = e.get("effectParams").cloned().unwrap_or(Value::Null);
        let num = |k: &str, dflt: f32| p.get(k).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(dflt);
        let flag = |k: &str| p.get(k).and_then(|v| v.as_bool()).unwrap_or(true);
        let manual = manual_all || name.ends_with("_Manual");
        let sides = [(Side::Out, 0usize), (Side::In, 1usize)];
        match name {
            // volume crossfade: in linear amplitude, the outgoing over the last `range`, the
            // incoming over the first `range`, down to `level` dB
            n if n.starts_with("AE_CrossFade") || n.starts_with("AE_FadeIn") || n.starts_with("AE_FadeOut") => {
                for (side, i) in sides {
                    if n.starts_with("AE_FadeIn") && side == Side::Out || n.starts_with("AE_FadeOut") && side == Side::In {
                        continue;
                    }
                    if manual {
                        if let Some(pts) = stored(i, "volume") {
                            set(&mut lanes, side, Param::Volume, pts.into_iter().map(|(t, v)| (t, vol_value(v))).collect());
                        }
                        continue;
                    }
                    let level = 10f32.powf(num(if side == Side::Out { "level" } else { "primaryLevel" }, -20.0) / 20.0);
                    let range = num("range", 1.0).clamp(0.01, 1.0);
                    set(&mut lanes, side, Param::Volume, sampled(d, 24, |t| {
                        let t = if side == Side::Out { t } else { 1.0 - t };
                        let k = ((t - (1.0 - range)) / range).clamp(0.0, 1.0);
                        let a = 1.0 + (level - 1.0) * k;
                        vol_value(20.0 * a.max(1e-6).log10())
                    }));
                }
            }
            n if n.starts_with("AE_Swap") || n.starts_with("AE_ManualVolume") => {
                for (side, i) in sides {
                    if manual || n.starts_with("AE_ManualVolume") {
                        if let Some(pts) = stored(i, "volume") {
                            set(&mut lanes, side, Param::Volume, pts.into_iter().map(|(t, v)| (t, vol_value(v))).collect());
                        }
                        continue;
                    }
                    let c = num("effectCenter", 0.5) * d;
                    let lv = vol_value(num("level", -50.0));
                    let pts = if side == Side::Out { vec![(0.0, 1.0), (c - eps, 1.0), (c, lv)] } else { vec![(0.0, lv), (c - eps, lv), (c, 1.0)] };
                    set(&mut lanes, side, Param::Volume, pts);
                }
            }
            // EQ bands: swap / crossfade / swap-fade / manual
            n if n.starts_with("AE_Bass_") || n.starts_with("AE_Mid_") || n.starts_with("AE_High_") => {
                let (param, control) = if n.starts_with("AE_Bass_") {
                    (Param::EqLow, "eq-low")
                } else if n.starts_with("AE_Mid_") {
                    (Param::EqMid, "eq-mid")
                } else {
                    (Param::EqHigh, "eq-high")
                };
                for (side, i) in sides {
                    if manual {
                        if let Some(pts) = stored(i, control) {
                            set(&mut lanes, side, param, pts.into_iter().map(|(t, v)| (t, eq_value(v))).collect());
                        }
                        continue;
                    }
                    let lv = eq_value(num(if side == Side::Out { "level" } else { "primaryLevel" }, 0.04));
                    let c = (num("effectCenter", 0.5) * d).round();
                    let pts = if n.contains("CrossFade") {
                        if side == Side::Out { vec![(0.0, 0.5), (d, lv)] } else { vec![(0.0, lv), (d, 0.5)] }
                    } else if n.contains("SwapFade") {
                        if side == Side::Out { vec![(0.0, 0.5), (c, lv)] } else { vec![(0.0, lv), (c - eps, lv), (c, 0.5)] }
                    } else {
                        let beats = num("beats", 0.0);
                        if side == Side::Out { vec![(0.0, 0.5), ((c - beats - eps).max(0.0), 0.5), ((c - beats).max(eps), lv)] } else { vec![(0.0, lv), (c - eps, lv), (c, 0.5)] }
                    };
                    set(&mut lanes, side, param, pts);
                }
            }
            // filters ("high to low" = low-pass on the way out)
            n if n.starts_with("AE_LowPass_Filter") || n.starts_with("AE_HighPass_Filter") || n.starts_with("AE_Manual_Filter") => {
                for (side, i) in sides {
                    if manual || n.starts_with("AE_Manual_Filter") {
                        if let Some(pts) = stored(i, "filter-level") {
                            set(&mut lanes, side, Param::Filter, pts.into_iter().map(|(t, v)| (t, filter_value(v))).collect());
                        }
                        continue;
                    }
                    let sign = if n.starts_with("AE_LowPass") { -1.0 } else { 1.0 };
                    let range = num("range", 1.0).clamp(0.01, 1.0);
                    let pts = if side == Side::Out {
                        vec![(0.0, 0.5), (d * (1.0 - range), 0.5), (d, filter_value(0.8 * sign))]
                    } else {
                        vec![(0.0, filter_value(-0.8 * sign)), (d, 0.5)]
                    };
                    set(&mut lanes, side, Param::Filter, pts);
                }
            }
            n if n.starts_with("AE_EchoTransition") => {
                let c = num("effectCenter", 0.0) * d;
                set(&mut lanes, Side::Out, Param::Echo, vec![(0.0, 0.0), (c, 0.0), (c + eps, 0.8)]);
                set(&mut lanes, Side::Out, Param::Volume, vec![(0.0, 1.0), (c, 1.0), ((c + 4.0).min(d), 0.0)]);
            }
            // stems: AE_Stems<Stem>_<Mode>[_Manual]
            n if n.starts_with("AE_Stems") => {
                let rest = &n["AE_Stems".len()..];
                let (stem, mode) = rest.split_once('_').unwrap_or((rest, "Full"));
                let (kinds, controls): (&[StemKind], &[&str]) = match stem {
                    "Drums" => (&[StemKind::Drums], &["virtual-stems-drums"]),
                    "Bass" => (&[StemKind::Bass], &["virtual-stems-bass"]),
                    "Melody" => (&[StemKind::Other], &["virtual-stems-melody"]),
                    "Vocals" | "VocalsHQ" => (&[StemKind::Vocals], &["virtual-stems-vocals", "virtual-stems-vocals-hq"]),
                    "Instrumental" | "InstrumentalHQ" => (&[StemKind::Drums, StemKind::Bass, StemKind::Other], &["virtual-stems-instrumental", "virtual-stems-instrumental-hq"]),
                    _ => continue,
                };
                // `_Manual` effects play their stored curves (none stored: the track's own stem
                // lane decides); the others generate theirs from the parameters (DJ.Studio's
                // stem generator, see `stem_points`)
                let is_manual = name.ends_with("_Manual");
                let mode = mode.trim_end_matches("_Manual");
                for (side, i) in sides {
                    let enabled = flag(if side == Side::Out { "primaryEnabled" } else { "secondaryEnabled" });
                    let pts: Option<Vec<(f32, f32)>> = if !enabled {
                        // switched off on this side for the whole transition
                        Some(vec![(0.0, 0.0), (d, 0.0)])
                    } else if is_manual {
                        controls.iter().find_map(|c| stored(i, c))
                    } else {
                        let dflt = stem_defaults(stem, mode);
                        let get = |k: &str, dv: f32| num(k, dv);
                        let sp = StemParams {
                            level: get("level", 1.0),
                            primary_level: get("primaryLevel", 1.0),
                            effect_center: get("effectCenter", dflt[0]),
                            primary_effect_center: get("primaryEffectCenter", dflt[1]),
                            custom_swap_point: get("customSwapPoint", dflt[2]),
                            primary_custom_swap_point: get("primaryCustomSwapPoint", dflt[3]),
                            beats: get("beats", 0.0),
                            cross_fade: p.get("crossFade").and_then(|v| v.as_bool()).unwrap_or(true),
                        };
                        Some(stem_points(&sp, side == Side::Out, d))
                    };
                    if let Some(pts) = pts {
                        for k in kinds {
                            set(&mut lanes, side, Param::Stem(*k), pts.clone());
                        }
                    }
                }
            }
            _ => log::debug!("DJ.Studio effect {name} not converted"),
        }
    }
    let locked = ae.get("locked").and_then(|l| l.as_bool()).unwrap_or(false);
    Transition { style, beats: d.round().max(1.0) as u32, lanes, locked }
}

pub struct ImportReport {
    pub tracks_imported: usize,
    pub cues_imported: usize,
    pub missing_files: Vec<PathBuf>,
    pub mixes: Vec<(i64, String)>,
}

/// Import library tracks that exist on disk (adds them to the ClubCraft library and stores their
/// DJ.Studio / Mixed In Key hot cues as imported markers). Returns DJ.Studio key → track id.
pub fn import_library(lib: &Library, root: &Path, report: &mut ImportReport) -> Result<HashMap<String, TrackId>> {
    let mut map = HashMap::new();
    for t in read_library(root) {
        if !t.path.exists() {
            report.missing_files.push(t.path.clone());
            continue;
        }
        let id = match crate::importer::import_file(lib, &t.path) {
            Ok(id) => id,
            Err(e) => {
                log::warn!("DJ.Studio import: {}: {e:#}", t.path.display());
                report.missing_files.push(t.path.clone());
                continue;
            }
        };
        report.tracks_imported += 1;
        map.insert(t.key.clone(), id);
        // Keep the user's own cues; replace previously imported ones.
        let mut cues: Vec<Cue> = lib.cues(id)?.into_iter().filter(|c| c.source != CueSource::Imported).collect();
        for (i, (time, name)) in t.hot_cues.iter().enumerate() {
            cues.push(Cue {
                slot: None,
                kind: CueKind::Hot,
                pos: *time,
                name: if name.is_empty() { format!("DJS {}", i + 1) } else { name.clone() },
                color: [200, 200, 210],
                loop_beats: None,
                source: CueSource::Imported,
            });
            report.cues_imported += 1;
        }
        lib.set_cues(id, &cues)?;
    }
    Ok(map)
}

/// Convert one DJ.Studio project into a ClubCraft mix (not yet saved).
pub fn project_to_mix(root: &Path, project_key: &str, tracks: &HashMap<String, TrackId>) -> Result<Mix> {
    let pv = read_json(&root.join("projects-table").join(project_key)).context("project not found")?;
    let name = pv.get("name").and_then(|n| n.as_str()).unwrap_or("DJ.Studio mix");
    let mut mix = Mix { name: format!("{name} (DJ.Studio)"), tempo: TempoMode::Glide, keylock: true, ..Default::default() };
    if pv.get("mixBpmFixedMode").and_then(|b| b.as_bool()).unwrap_or(false) {
        if let Some(b) = pv.get("mixBpmStart").and_then(|b| b.as_f64()).filter(|b| *b > 0.0) {
            mix.tempo = TempoMode::Fixed(b);
        }
    }
    let effects: Vec<Value> = pv.get("autoEffects").and_then(|e| e.as_array()).cloned().unwrap_or_default();
    let list = pv.get("mixList").and_then(|l| l.as_array()).cloned().unwrap_or_default();
    let mut item_keys = Vec::new();
    for item in &list {
        let lk = item.get("libraryKey").and_then(|k| k.as_str()).unwrap_or_default();
        let ik = item.get("key").and_then(|k| k.as_str()).unwrap_or_default();
        let Some(&tid) = tracks.get(lk) else { continue }; // jingles and missing files are skipped
        let mut mt = MixTrack { track_id: tid, mix_in: 0.0, mix_out: 0.0, key_shift: 0, gain_db: 0.0, stems: Vec::new() };
        if let Some(md) = read_json(&root.join("mix-data-table").join(ik)) {
            let sys = md.pointer("/cueData/systemCuePoints").and_then(|c| c.as_array()).cloned().unwrap_or_default();
            let time = |i: usize| sys.get(i).and_then(|c| c.get("time")).and_then(|t| t.as_f64());
            mt.mix_in = time(0).unwrap_or(0.0).max(0.0);
            mt.mix_out = time(1).unwrap_or(0.0);
            mt.key_shift = md.get("pitch").and_then(|p| p.as_f64()).unwrap_or(0.0).round() as i32;
            // DJ.Studio's trackOutputGain is its own loudness matching; ClubCraft normalises
            // loudness itself (auto-gain), so importing it would attenuate tracks twice.
            mt.gain_db = 0.0;
            mt.stems = track_stem_lanes(root, ik, &md);
        }
        mix.tracks.push(mt);
        item_keys.push(ik.to_string());
    }
    for k in item_keys.iter().take(item_keys.len().saturating_sub(1)) {
        let ae = effects.iter().find(|e| e.get("mixTrackKey").and_then(|m| m.as_str()) == Some(k.as_str()));
        let t = match ae {
            Some(ae) => transition_from(root, ae),
            None => Transition::new(TransitionStyle::BassSwap, 32),
        };
        mix.transitions.push(t);
    }
    // DJ.Studio plays the first track from its beginning (its cue-in only anchors transitions).
    if let Some(first) = mix.tracks.first_mut() {
        first.mix_in = 0.0;
    }
    mix.normalize();
    Ok(mix)
}

/// Import everything: library + all projects as mixes.
pub fn import_all(lib: &Library, root: &Path) -> Result<ImportReport> {
    let mut report = ImportReport { tracks_imported: 0, cues_imported: 0, missing_files: vec![], mixes: vec![] };
    let map = import_library(lib, root, &mut report)?;
    let existing: HashMap<String, i64> = lib.mixes()?.into_iter().map(|m| (m.1, m.0)).collect();
    let mut seen: std::collections::HashSet<String> = Default::default();
    for p in read_projects(root) {
        let mut mix = project_to_mix(root, &p.key, &map)?;
        // Projects are sorted newest first; keep only the newest of same-named versions.
        if mix.tracks.len() < 2 || !seen.insert(mix.name.clone()) {
            continue;
        }
        // Re-importing updates an earlier import (tracks, mix points, transitions, tempo) and
        // keeps what only exists in ClubCraft (sample lanes, lane mixer settings).
        if let Some(&id) = existing.get(&mix.name) {
            if let Ok(Some(old)) = lib.mix(id) {
                mix.id = id;
                mix.samples = old.samples;
                mix.sample_lanes = old.sample_lanes;
                mix.lanes = old.lanes;
            }
        }
        let id = lib.save_mix(&mut mix)?;
        report.mixes.push((id, mix.name.clone()));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(stem: &str, mode: &str) -> StemParams {
        let d = stem_defaults(stem, mode);
        StemParams { level: 1.0, primary_level: 1.0, effect_center: d[0], primary_effect_center: d[1], custom_swap_point: d[2], primary_custom_swap_point: d[3], beats: 0.0, cross_fade: true }
    }

    #[test]
    fn stem_effects_follow_djstudio() {
        // Full: the outgoing keeps the stem to the end, the incoming has it from the start
        assert_eq!(stem_points(&sp("Drums", "Full"), true, 32.0), vec![(0.0, 1.0), (32.0, 1.0), (32.0, 0.0), (32.0, 0.0)]);
        assert_eq!(stem_points(&sp("Drums", "Full"), false, 32.0), vec![(0.0, 0.0), (0.0, 0.0), (0.0, 1.0), (32.0, 1.0)]);
        // Melody crossfade: out fades over 25-75 %, in comes up over 25-99 % (whole beats)
        assert_eq!(stem_points(&sp("Melody", "Crossfade"), true, 32.0), vec![(0.0, 1.0), (8.0, 1.0), (24.0, 0.0), (32.0, 0.0)]);
        assert_eq!(stem_points(&sp("Melody", "Crossfade"), false, 32.0), vec![(0.0, 0.0), (8.0, 0.0), (32.0, 1.0), (32.0, 1.0)]);
        // Swap: a hard switch in the middle for both
        assert_eq!(stem_points(&sp("Vocals", "Swap"), true, 32.0), vec![(0.0, 1.0), (16.0, 1.0), (16.0, 0.0), (32.0, 0.0)]);
        assert_eq!(stem_points(&sp("Vocals", "Swap"), false, 32.0), vec![(0.0, 0.0), (16.0, 0.0), (16.0, 1.0), (32.0, 1.0)]);
        // without crossFade: a switch at the side's own centre; a lower level is kept
        let mut p = sp("Bass", "Crossfade");
        p.cross_fade = false;
        p.primary_level = 0.93;
        assert_eq!(stem_points(&p, true, 64.0), vec![(0.0, 0.93), (48.0, 0.93), (48.0, 0.0), (64.0, 0.0)]);
    }
    #[test]
    fn keys() {
        assert_eq!(key_from_int(0).unwrap().camelot(), "8B");
        assert_eq!(key_from_int(12).unwrap().camelot(), "8A");
        assert_eq!(key_from_int(22).unwrap().camelot(), "6A");
        assert_eq!(key_from_int(23).unwrap().camelot(), "1A");
        assert_eq!(key_from_int(13).unwrap().camelot(), "3A");
        assert_eq!(bucket("file_08ab3d2aa45c64d12c46834f736e801dd2a15f0fcd73997b42b4a703ff592267"), "1d");
    }
}

#[cfg(test)]
mod real_data {
    /// `cargo test -p clubcraft real_djstudio -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_djstudio() {
        let lib = clubcraft_core::Library::in_memory().unwrap();
        let r = super::import_all(&lib, &super::default_root()).unwrap();
        println!("tracks {} cues {} missing {} mixes {}", r.tracks_imported, r.cues_imported, r.missing_files.len(), r.mixes.len());
        for (id, name) in r.mixes.iter().take(5) {
            let m = lib.mix(*id).unwrap().unwrap();
            println!("{name}: {} tracks", m.tracks.len());
            for (i, t) in m.tracks.iter().enumerate().take(4) {
                let tr = lib.track(t.track_id).unwrap().unwrap();
                let tx = m.transitions.get(i).map(|t| format!("{:?} {}b", t.style, t.beats)).unwrap_or_default();
                println!("  {} in {:.1} out {:.1} shift {} gain {:.1} -> {}", tr.display_name(), t.mix_in, t.mix_out, t.key_shift, t.gain_db, tx);
            }
        }
    }
}

#[cfg(test)]
mod real_transitions {
    /// `cargo test -p clubcraft real_djstudio_transitions -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_djstudio_transitions() {
        let root = super::default_root();
        for p in super::read_projects(&root).into_iter().filter(|p| p.name.contains("Harvard-Square")).take(1) {
            let pv = super::read_json(&root.join("projects-table").join(&p.key)).unwrap();
            let only: Option<usize> = std::env::var("DJS_T").ok().and_then(|v| v.parse().ok());
            let order: Vec<String> = pv["mixList"].as_array().unwrap().iter().filter_map(|i| i["key"].as_str().map(String::from)).collect();
            for (n, k) in order.iter().enumerate().filter(|(n, _)| only.is_none_or(|o| o == *n)) {
                let Some(ae) = pv["autoEffects"].as_array().unwrap().iter().find(|e| e["mixTrackKey"].as_str() == Some(k.as_str())) else { continue };
                println!("## transition {n}: {:?}", ae["effects"].as_array().unwrap().iter().map(|e| e["effectName"].as_str().unwrap_or("")).collect::<Vec<_>>());
                let t = super::transition_from(&root, ae);
                println!("== {} beats {} style {:?} locked {}", &ae["key"].as_str().unwrap()[37..45], t.beats, t.style, t.locked);
                for l in &t.lanes {
                    let pts: Vec<String> = l.points.iter().map(|(x, y)| format!("({x:.1},{y:.2})")).collect();
                    println!("   {:?} {:?}: {}", l.side, l.param, pts.join(" "));
                }
                if t.uses_stems() {
                    let f = t.without_stems();
                    for side in [clubcraft_core::Side::Out, clubcraft_core::Side::In] {
                        let v: Vec<String> = (0..=8).map(|i| t.beats as f32 * i as f32 / 8.0).map(|x| format!("{:.2}", f.value(side, clubcraft_core::Param::Volume, x).powi(2))).collect();
                        println!("   without stems {side:?} volume gain: {}", v.join(" "));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod real_render {
    /// Import the DJ.Studio projects into `CLUBCRAFT_HOME` (use a copy of the library!) and render
    /// the mix named `DJS_MIX` to `DJS_OUT` (16-bit WAV), printing the transition start times:
    /// `CLUBCRAFT_HOME=… DJS_MIX=Harvard-Square-Mix DJS_OUT=/tmp/x.wav cargo test -p clubcraft real_djstudio_render -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_djstudio_render() {
        let home = std::env::var("CLUBCRAFT_HOME").expect("CLUBCRAFT_HOME (a copy)");
        assert!(!home.contains("Music/ClubCraft"), "use a copy of the library");
        let lib = clubcraft_core::Library::open_default().unwrap();
        let rep = super::import_all(&lib, &super::default_root()).unwrap();
        let name = std::env::var("DJS_MIX").unwrap();
        let (id, _) = rep.mixes.iter().find(|m| m.1.starts_with(&name)).expect("mix").clone();
        let mix = lib.mix(id).unwrap().unwrap();
        let all = lib.all_analyses().unwrap();
        let missing = mix.tracks.iter().filter(|t| all.get(&t.track_id).is_none_or(|a| a.stems_model.is_none())).count();
        println!("{} tracks, {} without stems, {} with own stem lanes", mix.tracks.len(), missing, mix.tracks.iter().filter(|t| !t.stems.is_empty()).count());
        let out = std::path::PathBuf::from(std::env::var("DJS_OUT").unwrap());
        let opts = clubcraft_engine::RenderOptions { format: clubcraft_engine::WavFormat::Pcm16, ..Default::default() };
        let r = clubcraft_engine::render_mix(&mix, &|id| all.get(&id).cloned(), &|id| crate::live::adapter::load_uncached(id).ok(), &out, &opts, &mut |_| true).unwrap();
        for (k, x) in r.timeline.transitions.iter().enumerate() {
            println!("transition {k}: {:.2} .. {:.2} s ({} beats)", x.start_sec, x.end_sec, x.beats);
        }
    }
}
