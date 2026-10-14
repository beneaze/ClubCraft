//! Import from DJ.Studio (`~/Music/DJ.Studio/Database`): library tracks with their hot cues, and
//! projects as ClubCraft mixes (track order, mix-in/out points, transition lengths and styles,
//! key shifts and gains). Read-only: DJ.Studio's files are never modified.
//!
//! Format notes (see docs/research): every table is a folder of JSON files named by record key;
//! `audio-library` and `track-structures` records sit in hash buckets = (sum of the key's char
//! codes) % 256 as unpadded lowercase hex.

use anyhow::{Context, Result};
use clubcraft_core::{Cue, CueKind, CueSource, Library, Mix, MixTrack, TempoMode, TrackId, Transition, TransitionStyle};
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
fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut de = serde_json::Deserializer::from_str(&text).into_iter::<Value>();
    de.next()?.ok()
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
        let mut mt = MixTrack { track_id: tid, mix_in: 0.0, mix_out: 0.0, key_shift: 0, gain_db: 0.0 };
        if let Some(md) = read_json(&root.join("mix-data-table").join(ik)) {
            let sys = md.pointer("/cueData/systemCuePoints").and_then(|c| c.as_array()).cloned().unwrap_or_default();
            let time = |i: usize| sys.get(i).and_then(|c| c.get("time")).and_then(|t| t.as_f64());
            mt.mix_in = time(0).unwrap_or(0.0).max(0.0);
            mt.mix_out = time(1).unwrap_or(0.0);
            mt.key_shift = md.get("pitch").and_then(|p| p.as_f64()).unwrap_or(0.0).round() as i32;
            mt.gain_db = md.get("trackOutputGain").and_then(|g| g.as_f64()).unwrap_or(0.0) as f32;
        }
        mix.tracks.push(mt);
        item_keys.push(ik.to_string());
    }
    for k in item_keys.iter().take(item_keys.len().saturating_sub(1)) {
        let ae = effects.iter().find(|e| e.get("mixTrackKey").and_then(|m| m.as_str()) == Some(k.as_str()));
        let t = match ae {
            Some(ae) => {
                let beats = ae.get("duration").and_then(|d| d.as_f64()).unwrap_or(32.0).max(4.0) as u32;
                let style = style_from_effects(ae.get("effects").and_then(|e| e.as_array()).map(|v| v.as_slice()).unwrap_or(&[]));
                Transition::new(style, beats)
            }
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
    let mut existing: std::collections::HashSet<String> = lib.mixes()?.into_iter().map(|m| m.1).collect();
    for p in read_projects(root) {
        let mut mix = project_to_mix(root, &p.key, &map)?;
        // Projects are sorted newest first; keep only the newest of same-named versions.
        if mix.tracks.len() < 2 || !existing.insert(mix.name.clone()) {
            continue;
        }
        let id = lib.save_mix(&mut mix)?;
        report.mixes.push((id, mix.name.clone()));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
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
