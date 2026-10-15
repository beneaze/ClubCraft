//! Import from DJ.Studio: library tracks with their cues, and projects as ClubCraft mixes.
//! Read-only: DJ.Studio's files are never modified.
//!
//! Written under a clean-room process, only from observation of DJ.Studio's own data files and
//! exports and from its public documentation. Every behaviour relied on here is described, with
//! its source, in `docs/interop/djstudio.md` (section numbers in the comments refer to it).

use crate::importer::import_file;
use anyhow::{Context, Result};
use clubcraft_core::{BeatGrid, Cue, CueKind, CueSource, Lane, Library, Mix, MixTrack, Param, Side, StemKind, StemLane, TempoMode, TrackId, Transition, TransitionStyle};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// DJ.Studio's database folder in the user's music folder.
pub fn default_root() -> PathBuf {
    dirs::audio_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Music")).join("DJ.Studio").join("Database")
}

pub struct ImportReport {
    pub tracks_imported: usize,
    pub cues_imported: usize,
    pub missing_files: Vec<PathBuf>,
    pub mixes: Vec<(i64, String)>,
    /// What could not be imported as it is in DJ.Studio (sample clips, cuts, ...), per project.
    pub notes: Vec<String>,
}

/// Import DJ.Studio's library and projects into `lib`. Projects already imported (same name)
/// are updated in place; their sample clips, sample lanes and lane mixer settings are kept.
pub fn import_all(lib: &Library, root: &Path) -> Result<ImportReport> {
    anyhow::ensure!(root.join("audio-library-table").is_dir() || root.join("projects-table").is_dir(), "no DJ.Studio database in {}", root.display());
    let mut report = ImportReport { tracks_imported: 0, cues_imported: 0, missing_files: Vec::new(), mixes: Vec::new(), notes: Vec::new() };
    let tracks = import_library(lib, root, &mut report)?;
    let analyses = lib.all_analyses()?;
    let mut used_names: HashSet<String> = HashSet::new();
    for (name, version) in current_projects(root) {
        let own_gain = |id: TrackId| analyses.get(&id).and_then(|a| a.gain_db).unwrap_or(0.0);
        let (mut mix, notes) = match convert_project(root, &version, &tracks, &own_gain) {
            Ok(x) => x,
            Err(e) => {
                report.notes.push(format!("{name}: not imported ({e:#})"));
                continue;
            }
        };
        if mix.tracks.is_empty() {
            continue;
        }
        let mut unique = name.clone();
        let mut n = 2;
        while !used_names.insert(unique.clone()) {
            unique = format!("{name} ({n})");
            n += 1;
        }
        mix.name = unique;
        report.notes.extend(notes.into_iter().map(|s| format!("{}: {s}", mix.name)));
        let name = mix.name.clone();
        let id = save_mix(lib, mix)?;
        report.mixes.push((id, name));
    }
    for n in &report.notes {
        log::info!("DJ.Studio import: {n}");
    }
    Ok(report)
}

/// Save `mix`, replacing an earlier import of the same name (its samples, sample lanes and lane
/// mixer settings are kept).
fn save_mix(lib: &Library, mut mix: Mix) -> Result<i64> {
    if let Some((id, _, _)) = lib.mixes()?.into_iter().find(|(_, n, _)| *n == mix.name) {
        if let Some(old) = lib.mix(id)? {
            mix.id = id;
            mix.samples = old.samples;
            mix.sample_lanes = old.sample_lanes;
            mix.lanes = old.lanes;
        }
    }
    mix.normalize();
    lib.save_mix(&mut mix)
}

// ------------------------------------------------------------------------------- files (§1)

/// The first JSON value in a file (some table files carry trailing bytes after the record).
fn read_json(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::Deserializer::from_slice(&bytes).into_iter::<Value>().next()?.ok()
}

/// Every record of a table folder: files directly in it, or in one level of bucket folders.
fn records(dir: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
            continue;
        }
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                let mut inner: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
                inner.sort();
                out.extend(inner.iter().filter_map(|p| read_json(p)));
            }
        } else if let Some(v) = read_json(&p) {
            out.push(v);
        }
    }
    out
}

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64)
}

fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn list<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k).and_then(Value::as_array).map(|a| a.as_slice()).unwrap_or(&[])
}

fn flag(v: &Value, k: &str, default: bool) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(default)
}

/// The part of a record key after the project key (`"<project>/<id>"` → `"<id>"`).
fn local_key(key: &str) -> &str {
    key.rsplit('/').next().unwrap_or(key)
}

/// An automation curve: `(time, value)` points of one control (§5).
type Curve = Vec<(f64, f64)>;

/// The automation curves of one owner (a mix track, or one side of a transition), by control name.
fn automation(root: &Path, project: &str, owner: &str) -> HashMap<String, Curve> {
    let mut out = HashMap::new();
    for r in records(&root.join("automation-data-table").join(project).join(owner)) {
        let pts: Curve = list(&r, "points").iter().filter_map(|p| Some((num(p, "time")?, num(p, "value")?))).collect();
        out.insert(text(&r, "controlName").to_string(), pts);
    }
    out
}

// ---------------------------------------------------------------------------- library (§2)

/// A straight beat grid: beat `b0` at `t0` s, beat length `len` s.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Grid {
    t0: f64,
    b0: f64,
    len: f64,
}

impl Grid {
    fn time_of(&self, beat: f64) -> f64 {
        self.t0 + (beat - self.b0) * self.len
    }

    /// ClubCraft's grid: DJ.Studio's beat numbers start at 0 one beat before the beat it numbers
    /// 0 in its beat data, so beat 1 is taken as the bar anchor (§3.2).
    fn beat_grid(&self) -> BeatGrid {
        BeatGrid { bpm: 60.0 / self.len, downbeat: self.time_of(1.0), beats_per_bar: 4 }
    }
}

/// A library record as far as the import needs it.
#[derive(Debug, Clone, Default)]
pub(crate) struct LibTrack {
    /// Beat grid from the record's tempo line (`bpmLine` end points).
    grid: Option<Grid>,
    /// DJ.Studio's tempo value of the track (`bpmLine[0].value`, §4.3).
    tempo_value: Option<f64>,
    /// Mix-in / mix-out beats (`cueData.systemCuePoints`), hot cue beats by pad, memory cue beats.
    system_cues: Vec<f64>,
    hot_cues: Vec<(u8, f64, String)>,
    memory_cues: Vec<(f64, String)>,
}

fn lib_track(r: &Value) -> LibTrack {
    let line = list(r, "bpmLine");
    let grid = match (line.first(), line.last()) {
        (Some(a), Some(b)) if line.len() >= 2 => {
            let (t0, b0, t1, b1) = (num(a, "time").unwrap_or(0.0), num(a, "beatNr").unwrap_or(0.0), num(b, "time").unwrap_or(0.0), num(b, "beatNr").unwrap_or(0.0));
            (b1 > b0 && t1 > t0).then(|| Grid { t0, b0, len: (t1 - t0) / (b1 - b0) })
        }
        _ => None,
    };
    let cues = r.get("cueData").cloned().unwrap_or(Value::Null);
    let beat = |c: &Value| num(c, "beat").filter(|b| *b >= 0.0);
    LibTrack {
        grid,
        tempo_value: line.first().and_then(|a| num(a, "value")).filter(|v| *v > 20.0),
        system_cues: list(&cues, "systemCuePoints").iter().filter_map(beat).collect(),
        hot_cues: list(&cues, "hotCuePoints").iter().enumerate().filter_map(|(i, c)| Some((i as u8, beat(c)?, text(c, "name").to_string()))).collect(),
        memory_cues: list(&cues, "memCuePoints").iter().filter_map(|c| Some((beat(c)?, text(c, "name").to_string()))).collect(),
    }
}

/// The audio file of a library record: the recorded location, else the last known one.
fn record_path(r: &Value) -> (Option<PathBuf>, Option<PathBuf>) {
    let ext = r.get("externalRec").cloned().unwrap_or(Value::Null);
    let cands: Vec<PathBuf> = ["fileLocationPath", "lastKnownFilePath"].iter().map(|k| text(&ext, k)).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
    (cands.iter().find(|p| p.is_file()).cloned(), cands.first().cloned())
}

/// Cues of a library record in track seconds (`Imported`): mix-in / mix-out markers, hot cues on
/// their pads, memory cues as markers.
fn record_cues(t: &LibTrack) -> Vec<Cue> {
    let Some(g) = t.grid else { return Vec::new() };
    let mk = |slot: Option<u8>, kind: CueKind, beat: f64, name: &str| Cue {
        slot,
        kind,
        pos: g.time_of(beat).max(0.0),
        name: if name.is_empty() { kind.label().to_string() } else { name.to_string() },
        color: kind.default_color(),
        loop_beats: None,
        source: CueSource::Imported,
    };
    let mut out = Vec::new();
    for (i, &b) in t.system_cues.iter().take(2).enumerate() {
        out.push(mk(None, if i == 0 { CueKind::MixIn } else { CueKind::MixOut }, b, ""));
    }
    for (slot, b, name) in &t.hot_cues {
        out.push(mk(Some(*slot), CueKind::Hot, *b, name));
    }
    for (b, name) in &t.memory_cues {
        out.push(mk(None, CueKind::Hot, *b, name));
    }
    out
}

/// Add every library track whose file exists, with its cues. Returns them by DJ.Studio key.
fn import_library(lib: &Library, root: &Path, report: &mut ImportReport) -> Result<LibTracks> {
    let mut out = HashMap::new();
    for r in records(&root.join("audio-library-table")) {
        let key = text(&r, "key").to_string();
        if key.is_empty() {
            continue;
        }
        let (path, recorded) = record_path(&r);
        let Some(path) = path else {
            if let Some(p) = recorded {
                report.missing_files.push(p);
            }
            continue;
        };
        let id = match import_file(lib, &path) {
            Ok(id) => id,
            Err(e) => {
                report.notes.push(format!("{}: not imported ({e:#})", path.display()));
                continue;
            }
        };
        report.tracks_imported += 1;
        let t = lib_track(&r);
        let ours = record_cues(&t);
        if !ours.is_empty() {
            // keep the user's own cues; earlier imports are replaced
            let mut cues: Vec<Cue> = lib.cues(id)?.into_iter().filter(|c| c.source != CueSource::Imported).collect();
            let taken: HashSet<u8> = cues.iter().filter_map(|c| c.slot).collect();
            let new: Vec<Cue> = ours.into_iter().filter(|c| c.slot.is_none_or(|s| !taken.contains(&s))).collect();
            report.cues_imported += new.len();
            cues.extend(new);
            lib.set_cues(id, &cues)?;
        }
        out.insert(key, (id, t));
    }
    Ok(out)
}

// --------------------------------------------------------------------------- projects (§3)

/// `(name, current version key)` of every project that is not deleted.
fn current_projects(root: &Path) -> Vec<(String, String)> {
    let exists = |k: &str| !k.is_empty() && root.join("projects-table").join(k).is_file();
    let mut out = Vec::new();
    let metas = records(&root.join("projects-meta-table"));
    for m in &metas {
        if flag(m, "isDeleted", false) {
            continue;
        }
        let versions: Vec<&str> = list(m, "versions").iter().map(|v| text(v, "key")).collect();
        let current = versions.iter().rev().copied().find(|k| exists(k)).or_else(|| Some(text(m, "key")).filter(|k| exists(k)));
        if let Some(v) = current {
            out.push((text(m, "name").to_string(), v.to_string()));
        }
    }
    if metas.is_empty() {
        // no meta table: the latest saved version of each project
        let mut latest: BTreeMap<String, (String, String, String)> = BTreeMap::new();
        for p in records(&root.join("projects-table")) {
            let meta = if text(&p, "metaKey").is_empty() { text(&p, "key") } else { text(&p, "metaKey") }.to_string();
            let e = (text(&p, "lastModified").to_string(), text(&p, "key").to_string(), text(&p, "name").to_string());
            if latest.get(&meta).is_none_or(|old| old.0 < e.0) {
                latest.insert(meta, e);
            }
        }
        out.extend(latest.into_values().map(|(_, k, n)| (n, k)));
    }
    out
}

/// Beat grid of a mix track from its beat map (`mixMap`: track time of every beat it plays):
/// a least-squares line over the beats. Returns the grid and the largest deviation from it (s),
/// which is large when the map contains cuts.
fn map_grid(map: &[Value]) -> Option<(Grid, f64)> {
    // the last entry marks the end of the track (duration 0)
    let pts: Vec<(f64, f64)> = map.iter().filter(|b| num(b, "duration").is_none_or(|d| d > 0.0)).filter_map(|b| Some((num(b, "trackBeatIx")?, num(b, "time")?))).collect();
    if pts.len() < 2 {
        return None;
    }
    let n = pts.len() as f64;
    let (mx, my) = (pts.iter().map(|p| p.0).sum::<f64>() / n, pts.iter().map(|p| p.1).sum::<f64>() / n);
    let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    if sxx <= 0.0 {
        return None;
    }
    let len = sxy / sxx;
    if len <= 0.0 {
        return None;
    }
    let g = Grid { t0: my - len * mx, b0: 0.0, len };
    let dev = pts.iter().map(|p| (g.time_of(p.0) - p.1).abs()).fold(0.0, f64::max);
    Some((g, dev))
}

/// The imported library tracks by DJ.Studio library key.
pub(crate) type LibTracks = HashMap<String, (TrackId, LibTrack)>;

/// Seconds the incoming track lands late per unit of relative tempo change (incoming / outgoing
/// tempo − 1), measured on DJ.Studio's exports (§4.4).
const TEMPO_CHANGE_LAG: f64 = 0.256;

/// Track beat of every beat the mix plays (`mixMap` without its end marker).
fn beat_map(map: &[Value]) -> Vec<f64> {
    map.iter().filter(|b| num(b, "duration").is_none_or(|d| d > 0.0)).filter_map(|b| num(b, "trackBeatIx")).collect()
}

/// The parts of a track the mix plays between beat-map positions `from` and `to`, as track
/// beats `(in, out)`: one part, or one more after every cut (a jump in the beat map, §3.4). Each
/// part after a cut starts one beat before the jump target, so that a one-beat hard cut from
/// the part before lands on it.
fn pieces(map: &[f64], from: f64, to: f64) -> Vec<(f64, f64)> {
    let at = |pos: f64| -> f64 {
        let i = pos.floor().max(0.0) as usize;
        match map.get(i) {
            Some(&b) => b + (pos - i as f64),
            None => map.last().map(|&b| b + (pos - (map.len() - 1) as f64)).unwrap_or(pos),
        }
    };
    let mut out = Vec::new();
    let mut start = at(from);
    let (i0, i1) = (from.ceil().max(1.0) as usize, (to.floor() as usize).min(map.len().saturating_sub(1)));
    for j in i0..=i1 {
        if j < map.len() && j > 0 && (map[j] - map[j - 1] - 1.0).abs() > 1e-6 {
            out.push((start, map[j - 1]));
            start = map[j] - 1.0;
        }
    }
    out.push((start, at(to)));
    out
}

/// Convert one project version into a mix. `tracks` are the imported library tracks;
/// `own_gain` gives ClubCraft's loudness gain (dB) the player applies to a track.
pub(crate) fn convert_project(root: &Path, version: &str, tracks: &LibTracks, own_gain: &dyn Fn(TrackId) -> f32) -> Result<(Mix, Vec<String>)> {
    let p = read_json(&root.join("projects-table").join(version)).with_context(|| format!("project {version}"))?;
    let mut notes = Vec::new();
    let mut mix = Mix { name: text(&p, "name").to_string(), tempo: TempoMode::Glide, keylock: true, ..Default::default() };
    if flag(&p, "mixBpmFixedMode", false) {
        if let Some(b) = num(&p, "mixBpmStart").filter(|b| *b > 0.0) {
            mix.tempo = TempoMode::Fixed(b);
            if num(&p, "mixBpmEnd").is_some_and(|e| e > 0.0 && (e - b).abs() > 1e-6) {
                notes.push("a tempo change across the mix is played at the start tempo".into());
            }
        }
    }
    // transitions belong to their outgoing track (§3.3)
    let mut effects: HashMap<&str, &Value> = HashMap::new();
    for e in list(&p, "autoEffects") {
        if !flag(e, "isTrackEffect", false) && !flag(e, "isDisabled", false) {
            effects.insert(text(e, "mixTrackKey"), e);
        }
    }
    struct Placed<'a> {
        key: &'a str,
        track: MixTrack,
        lib: &'a LibTrack,
        grid: Grid,
        /// The piece continues the previous one after a cut (joined by a one-beat hard cut).
        after_cut: bool,
        /// Track beats it plays from (aligned with the previous track's out) and to (§3.2).
        beats: (f64, f64),
        /// DJ.Studio's gain of the mix track (dB).
        output_gain: f64,
    }
    let mut placed: Vec<Placed> = Vec::new();
    let mut number = 0;
    for m in list(&p, "mixList") {
        number += 1;
        let key = text(m, "key");
        let lk = text(m, "libraryKey");
        let Some((id, lt)) = tracks.get(lk).map(|(id, t)| (*id, t)) else {
            notes.push(format!("track {number} ({lk}) is not in the library (or its file is missing) and was left out"));
            continue;
        };
        let md = read_json(&root.join("mix-data-table").join(version).join(local_key(key))).unwrap_or(Value::Null);
        let map = beat_map(list(&md, "mixMap"));
        let grid = match map_grid(list(&md, "mixMap")) {
            Some((g, dev)) => {
                if dev > 0.02 {
                    notes.push(format!("track {number} has a variable beat map (up to {:.0} ms off a straight grid); it plays on a straight grid", dev * 1000.0));
                }
                g
            }
            None => match lt.grid {
                Some(g) => g,
                None => {
                    notes.push(format!("track {number} ({lk}) has no beat grid and was left out"));
                    continue;
                }
            },
        };
        // cue beats count the beats the mix plays of the track (positions in its beat map, §3.2)
        let cues = md.get("cueData").map(|c| list(c, "systemCuePoints")).unwrap_or(&[]);
        let cue = |i: usize| cues.get(i).and_then(|c| num(c, "beat")).filter(|b| *b >= 0.0);
        // the first track plays from its first beat (§4.1), the others from their mix-in cue
        let in_pos = if placed.is_empty() { 0.0 } else { cue(0).unwrap_or(0.0) };
        let out_pos = cue(1).unwrap_or(map.len().saturating_sub(1) as f64).max(in_pos + 1.0);
        let auto = automation(root, version, local_key(key));
        if auto.get("bpm").is_some_and(|c| !c.is_empty()) {
            notes.push(format!("track {number}: tempo automation is not imported"));
        }
        let pieces = pieces(&map, in_pos, out_pos);
        if pieces.len() > 1 {
            notes.push(format!("track {number}: {} cut(s) played as hard cuts between parts of the track", pieces.len() - 1));
        }
        // after a tempo change the incoming track lands slightly late (§4.4)
        let lag = placed.last().map(|prev| TEMPO_CHANGE_LAG * (prev.grid.len / grid.len - 1.0)).unwrap_or(0.0);
        for (k, (b_in, b_out)) in pieces.into_iter().enumerate() {
            let mut t = MixTrack::new(id, grid.time_of(b_in) - if k == 0 { lag } else { 0.0 }, grid.time_of(b_out));
            t.key_shift = num(&md, "pitch").unwrap_or(0.0).round() as i32;
            // DJ.Studio's per-track gain replaces ClubCraft's own loudness gain (§6)
            let output_gain = num(&md, "trackOutputGain").unwrap_or(0.0);
            t.gain_db = output_gain as f32 - own_gain(id);
            t.stems = track_stems(&auto, &grid);
            placed.push(Placed { key, track: t, lib: lt, grid, after_cut: k > 0, beats: (b_in, b_out), output_gain });
        }
    }
    // track effects: stems switched off over a part of a track (§5.4)
    for e in list(&p, "autoEffects").iter().filter(|e| flag(e, "isTrackEffect", false) && !flag(e, "isDisabled", false)) {
        let (from, len) = (num(e, "effectOffset").unwrap_or(0.0), num(e, "duration").unwrap_or(0.0));
        for pl in placed.iter_mut().filter(|pl| pl.key == text(e, "mixTrackKey")) {
            for x in list(e, "effects") {
                if let Some((stem, _)) = stem_effect(text(x, "effectName")) {
                    if !flag(x.get("effectParams").unwrap_or(&Value::Null), "primaryEnabled", true) {
                        pl.track.set_stem_range(stem, pl.grid.time_of(from), pl.grid.time_of(from + len), 0.0);
                    }
                }
            }
        }
    }
    for (i, pl) in placed.iter().enumerate() {
        let g = pl.grid.beat_grid();
        match mix.grids.get(&pl.track.track_id) {
            Some(old) if *old != g => notes.push(format!("track {} appears twice with different grids; the first one is used", i + 1)),
            _ => {
                mix.grids.insert(pl.track.track_id, g);
            }
        }
    }
    for k in 0..placed.len().saturating_sub(1) {
        let t = if placed[k + 1].after_cut {
            Transition::new(TransitionStyle::Cut, 1)
        } else {
            match effects.get(placed[k].key) {
                Some(e) => {
                    let mut t = transition(root, version, e, &mut notes);
                    if let (Some(a), Some(c)) = (placed[k].lib.tempo_value, placed[k + 1].lib.tempo_value) {
                        t.tempo = Some([a, c]);
                    }
                    let (pos, len) = (num(e, "bpmLinePos").unwrap_or(0.75), num(e, "bpmLineLen").unwrap_or(0.5));
                    if (pos - 0.75).abs() > 1e-6 || (len - 0.5).abs() > 1e-6 {
                        notes.push(format!("transition {}: its tempo change is played over the second half", k + 1));
                    }
                    t
                }
                None => Transition { style: TransitionStyle::Custom, beats: 32, lanes: Vec::new(), locked: false, tempo: None },
            }
        };
        mix.transitions.push(t);
    }
    // sample-lane clips (§3.5): a track's own vocals playing in sync with it are merged into its
    // vocal lanes; anything else cannot be played and is reported
    let starts: Vec<f64> = {
        let mut v = vec![0.0];
        for pl in &placed {
            let s = *v.last().unwrap();
            v.push(s + (pl.beats.1 - pl.beats.0));
        }
        v
    };
    for clip in list(&p, "jingleList").iter().filter(|j| flag(j, "isClip", true)) {
        let title = text(clip, "title");
        let md = read_json(&root.join("mix-data-table").join(version).join(local_key(text(clip, "mixDataKey")))).unwrap_or(Value::Null);
        let map = beat_map(list(&md, "mixMap"));
        let link = text(&md, "linkMixRecordKey");
        let anchor = (|| {
            let li = placed.iter().position(|pl| pl.key == link)?;
            let lmd = read_json(&root.join("mix-data-table").join(version).join(local_key(link)))?;
            let cues = lmd.get("cueData").map(|c| list(c, "systemCuePoints")).unwrap_or(&[]);
            let key = text(&md, "linkMixRecordCueKey");
            let cue = cues.iter().find(|c| !key.is_empty() && text(c, "cueKey") == key).or_else(|| cues.get(num(&md, "linkMixRecordCue")? as usize))?;
            let pos = num(cue, "beat")?;
            let lmap = beat_map(list(&lmd, "mixMap"));
            let beat = lmap.get(pos.max(0.0) as usize).copied().unwrap_or(pos);
            // the piece of the linked track that plays that beat
            let k = (li..placed.len()).take_while(|&k| placed[k].key == link).find(|&k| beat <= placed[k].beats.1 + 1e-6).unwrap_or(li);
            Some(starts[k] + beat - placed[k].beats.0)
        })();
        let lk = text(clip, "libraryKey");
        let own = tracks.get(lk).map(|t| t.0);
        let (Some(at), Some(&c0), Some(&c1), Some(id)) = (anchor, map.first(), map.last(), own) else {
            notes.push(format!("clip \"{title}\" is not imported"));
            continue;
        };
        // relative to the track's own gain, which the player already applies
        let own_db = placed.iter().find(|pl| pl.track.track_id == id).map(|pl| pl.output_gain).unwrap_or(0.0);
        let gain = db_gain(num(&md, "trackOutputGain").unwrap_or(0.0) - own_db);
        let span = (at, at + (c1 - c0) + 1.0);
        let mut merged = 0.0;
        for k in 0..placed.len() {
            if placed[k].track.track_id != id {
                continue;
            }
            // in sync: this piece plays beat c0 at the clip's start
            let own_beat = placed[k].beats.0 + (at - starts[k]);
            if (own_beat - c0).abs() > 0.05 {
                continue;
            }
            // the mix beats this piece sounds: from its start to the end of its outgoing transition
            let trans_out = mix.transitions.get(k).map(|t| t.beats as f64).unwrap_or(f64::INFINITY);
            let (a, b) = (span.0.max(starts[k]), span.1.min(starts[k + 1] + trans_out));
            if b <= a {
                continue;
            }
            merged += b - a;
            let g = placed[k].grid;
            let b0 = placed[k].beats.0;
            let to_track = |m: f64| g.time_of(b0 + (m - starts[k])) as f32;
            let (ta, tb) = (to_track(a), to_track(b));
            add_stem(&mut placed[k].track.stems, StemKind::Vocals, ta, tb, gain as f32);
            // inside the transitions of this piece the transition's own vocal lane wins: add there too
            for (ti, side, x0) in [(k.wrapping_sub(1), Side::In, starts[k]), (k, Side::Out, starts[k + 1])] {
                if let Some(t) = mix.transitions.get_mut(ti) {
                    let len = t.beats as f64;
                    let (xa, xb) = ((a - x0).max(0.0), (b - x0).min(len));
                    if xb > xa {
                        if let Some(l) = t.lanes.iter_mut().find(|l| l.side == side && l.param == Param::Stem(StemKind::Vocals)) {
                            add_range(&mut l.points, xa as f32, xb as f32, gain as f32, 1.0);
                        }
                    }
                }
            }
        }
        let total = span.1 - span.0;
        if merged < total - 0.5 {
            notes.push(format!("clip \"{title}\": {:.0} of {:.0} beats are not imported (they do not play along with the clip's own track)", total - merged, total));
        }
    }
    mix.tracks = placed.into_iter().map(|pl| pl.track).collect();
    Ok((mix, notes))
}

/// Add `g` to a lane between `x0` and `x1` (`default` where it has no points).
fn add_range(pts: &mut Vec<(f32, f32)>, x0: f32, x1: f32, g: f32, default: f32) {
    let at = |x: f32| if pts.is_empty() { default } else { interp(pts, x) };
    let (before, start, end, after) = (at(x0 - 1e-3), at(x0), at(x1), at(x1 + 1e-3));
    let mut out: Vec<(f32, f32)> = pts.iter().copied().filter(|p| p.0 < x0 - 1e-3).collect();
    out.push((x0 - 1e-3, before));
    out.push((x0, start + g));
    out.extend(pts.iter().copied().filter(|p| p.0 > x0 && p.0 < x1).map(|p| (p.0, p.1 + g)));
    out.push((x1, end + g));
    out.push((x1 + 1e-3, after));
    out.extend(pts.iter().copied().filter(|p| p.0 > x1 + 1e-3));
    out.retain(|p| p.0 >= 0.0);
    *pts = out;
}

/// Piecewise-linear value of points at `x` (held outside; 1 without points).
fn interp(p: &[(f32, f32)], x: f32) -> f32 {
    match p {
        [] => 1.0,
        [a] => a.1,
        _ => {
            if x <= p[0].0 {
                return p[0].1;
            }
            for w in p.windows(2) {
                if x <= w[1].0 {
                    let t = if w[1].0 > w[0].0 { (x - w[0].0) / (w[1].0 - w[0].0) } else { 1.0 };
                    return w[0].1 + (w[1].1 - w[0].1) * t;
                }
            }
            p[p.len() - 1].1
        }
    }
}

/// Add `g` to a track's stem lane between track times `t0` and `t1` (s).
fn add_stem(stems: &mut Vec<StemLane>, stem: StemKind, t0: f32, t1: f32, g: f32) {
    let i = match stems.iter().position(|l| l.stem == stem) {
        Some(i) => i,
        None => {
            stems.push(StemLane { stem, points: Vec::new() });
            stems.len() - 1
        }
    };
    add_range(&mut stems[i].points, t0, t1, g, 1.0);
}

/// Stem lanes of a whole track from its automation (`virtual-stems-*`, times in track beats).
fn track_stems(auto: &HashMap<String, Curve>, g: &Grid) -> Vec<StemLane> {
    let mut out = Vec::new();
    for (name, stem) in STEM_CONTROLS {
        if let Some(c) = auto.get(name).filter(|c| c.iter().any(|p| (p.1 - 1.0).abs() > 1e-4)) {
            out.push(StemLane { stem, points: c.iter().map(|&(b, v)| (g.time_of(b) as f32, v.clamp(0.0, 1.0) as f32)).collect() });
        }
    }
    out
}

const STEM_CONTROLS: [(&str, StemKind); 4] =
    [("virtual-stems-drums", StemKind::Drums), ("virtual-stems-bass", StemKind::Bass), ("virtual-stems-melody", StemKind::Other), ("virtual-stems-vocals", StemKind::Vocals)];

// ------------------------------------------------------------------------- transitions (§5)

/// Stem and mode of a stem effect name (`AE_StemsBass_Full_Manual` → bass, `Full_Manual`).
fn stem_effect(name: &str) -> Option<(StemKind, &str)> {
    let rest = name.strip_prefix("AE_Stems")?;
    let (stem, mode) = rest.split_once('_').unwrap_or((rest, ""));
    let s = match stem {
        "Drums" => StemKind::Drums,
        "Bass" => StemKind::Bass,
        "Melody" => StemKind::Other,
        "Vocals" => StemKind::Vocals,
        _ => return None,
    };
    Some((s, mode))
}

const STEP: f32 = 0.01;
/// Points per ramp: amplitude ramps are drawn as polylines in the lane's own value scale.
const RAMP_STEPS: usize = 8;

/// Lane value for a volume gain (lane gain = value²).
fn vol_value(gain: f64) -> f32 {
    gain.clamp(0.0, 1.0).sqrt() as f32
}

/// EQ knob for a band gain (knob gain = (2·knob)² up to flat).
fn eq_value(gain: f64) -> f32 {
    (0.5 * gain.clamp(0.0, 1.0).sqrt()) as f32
}

fn db_gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Points of a gain ramp from `g0` at `x0` to `g1` at `x1`, linear in amplitude, held outside.
fn ramp(len: f32, x0: f32, x1: f32, g0: f64, g1: f64, value: fn(f64) -> f32) -> Vec<(f32, f32)> {
    let (x0, x1) = (x0.clamp(0.0, len), x1.clamp(0.0, len));
    let mut pts = vec![(0.0, value(g0))];
    if x1 - x0 < STEP {
        pts.push(((x0 - STEP).max(0.0), value(g0)));
        pts.push((x0, value(g1)));
    } else {
        for i in 0..=RAMP_STEPS {
            let f = i as f64 / RAMP_STEPS as f64;
            pts.push((x0 + (x1 - x0) * f as f32, value(g0 + (g1 - g0) * f)));
        }
    }
    pts.push((len, value(g1)));
    pts.dedup_by(|b, a| (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6);
    pts
}

/// An equal-power stem crossfade between `x0` and `x1`: sin (`rising`) or cos of a quarter turn.
fn power_ramp(len: f32, x0: f32, x1: f32, rising: bool) -> Vec<(f32, f32)> {
    let g = |f: f64| {
        let q = f * std::f64::consts::FRAC_PI_2;
        (if rising { q.sin() } else { q.cos() }) as f32
    };
    let (x0, x1) = (x0.clamp(0.0, len), x1.clamp(0.0, len));
    if x1 - x0 < STEP {
        return step(len, x0, g(0.0) as f64, g(1.0) as f64, |v| v as f32);
    }
    let mut pts = vec![(0.0, g(0.0))];
    for i in 0..=RAMP_STEPS * 2 {
        let f = i as f64 / (RAMP_STEPS * 2) as f64;
        pts.push((x0 + (x1 - x0) * f as f32, g(f)));
    }
    pts.push((len, g(1.0)));
    pts.dedup_by(|b, a| (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6);
    pts
}

/// Volume fade-out from full at `x0` to gain `g1` at `x1` following a quarter cosine (in
/// amplitude).
fn cos_ramp(len: f32, x0: f32, x1: f32, g1: f64) -> Vec<(f32, f32)> {
    let mut pts = vec![(0.0, 1.0)];
    for i in 0..=RAMP_STEPS * 2 {
        let f = i as f64 / (RAMP_STEPS * 2) as f64;
        pts.push((x0 + (x1 - x0) * f as f32, vol_value(g1 + (1.0 - g1) * (f * std::f64::consts::FRAC_PI_2).cos())));
    }
    pts.push((len, vol_value(g1)));
    pts
}

/// A step from `g0` to `g1` at `x`.
fn step(len: f32, x: f32, g0: f64, g1: f64, value: fn(f64) -> f32) -> Vec<(f32, f32)> {
    ramp(len, x, x, g0, g1, value)
}

fn set_lane(lanes: &mut Vec<Lane>, side: Side, param: Param, points: Vec<(f32, f32)>) {
    lanes.retain(|l| !(l.side == side && l.param == param));
    lanes.push(Lane { side, param, points });
}

/// A DJ.Studio transition (`autoEffects` entry) as ClubCraft lanes.
fn transition(root: &Path, project: &str, e: &Value, notes: &mut Vec<String>) -> Transition {
    let beats = num(e, "duration").unwrap_or(32.0).round().max(1.0) as u32;
    let len = beats as f32;
    let id = local_key(text(e, "key"));
    let autos = [automation(root, project, &format!("{id}_0")), automation(root, project, &format!("{id}_1"))];
    let curve = |side: usize, control: &str| -> Option<Vec<(f32, f32)>> {
        let c = autos[side].get(control).filter(|c| !c.is_empty())?;
        Some(c.iter().map(|&(x, v)| ((x as f32).clamp(0.0, len), v as f32)).collect())
    };
    let mut lanes: Vec<Lane> = Vec::new();
    let mut manual_volume = false;
    for x in list(e, "effects") {
        let name = text(x, "effectName");
        let pr = x.get("effectParams").cloned().unwrap_or(Value::Null);
        let p = |k: &str, d: f64| num(&pr, k).unwrap_or(d);
        match name {
            // §5.1: incoming from `level` dB to full over the first `range` of the window (linear
            // in amplitude), outgoing from full to `primaryLevel` dB over the last `range`
            "AE_CrossFade" => {
                let r = p("range", 0.5).clamp(0.0, 1.0) as f32;
                set_lane(&mut lanes, Side::In, Param::Volume, ramp(len, 0.0, r * len, db_gain(p("level", -20.0)), 1.0, vol_value));
                // overlapping fades (range > 0.5): the outgoing side fades slower, fitted as a
                // quarter cosine; separate halves: linear
                let end = db_gain(p("primaryLevel", -20.0));
                let out = if r > 0.5 { cos_ramp(len, (1.0 - r) * len, len, end) } else { ramp(len, (1.0 - r) * len, len, 1.0, end, vol_value) };
                set_lane(&mut lanes, Side::Out, Param::Volume, out);
            }
            // §5.2: the lows swap at `effectCenter`, `beats` beats left out around it
            "AE_Bass_Swap" => {
                let c = p("effectCenter", 0.5).clamp(0.0, 1.0) as f32 * len;
                let gap = p("beats", 0.0).max(0.0) as f32;
                let (a, b) = ((c - gap / 2.0).max(0.0), (c + gap / 2.0).min(len));
                set_lane(&mut lanes, Side::In, Param::EqLow, step(len, b, p("level", 0.04), 1.0, eq_value));
                set_lane(&mut lanes, Side::Out, Param::EqLow, step(len, a, 1.0, p("primaryLevel", 0.04), eq_value));
            }
            // §5.2: the lows cross over like the volume crossfade
            "AE_Bass_CrossFade" => {
                let r = p("range", 1.0).clamp(0.0, 1.0) as f32;
                set_lane(&mut lanes, Side::In, Param::EqLow, ramp(len, 0.0, r * len, p("level", 0.04), 1.0, eq_value));
                set_lane(&mut lanes, Side::Out, Param::EqLow, ramp(len, (1.0 - r) * len, len, 1.0, p("primaryLevel", 0.04), eq_value));
            }
            // §5.3: volume swap at `effectCenter`
            "AE_Swap" => {
                let c = p("effectCenter", 0.5).clamp(0.0, 1.0) as f32 * len;
                let gap = p("beats", 0.0).max(0.0) as f32;
                let (a, b) = ((c - gap / 2.0).max(0.0), (c + gap / 2.0).min(len));
                set_lane(&mut lanes, Side::In, Param::Volume, step(len, b, db_gain(p("level", -50.0)), 1.0, vol_value));
                set_lane(&mut lanes, Side::Out, Param::Volume, step(len, a, 1.0, db_gain(p("primaryLevel", -50.0)), vol_value));
            }
            "AE_ManualVolume" => manual_volume = true,
            _ => match stem_effect(name) {
                Some((stem, mode)) => {
                    let control = STEM_CONTROLS.iter().find(|c| c.1 == stem).map(|c| c.0).unwrap_or_default();
                    for (si, side) in [(0, Side::Out), (1, Side::In)] {
                        let enabled = flag(&pr, if si == 0 { "primaryEnabled" } else { "secondaryEnabled" }, true);
                        let pts = if !enabled {
                            // §5.4: a stem switched off for a side is silent for the whole window
                            Some(vec![(0.0, 0.0)])
                        } else if let Some(c) = curve(si, control) {
                            Some(c)
                        } else {
                            stem_mode_points(mode, side, &pr, len)
                        };
                        if let Some(pts) = pts {
                            set_lane(&mut lanes, side, Param::Stem(stem), pts);
                        }
                    }
                }
                None => notes.push(format!("effect {name} is not imported")),
            },
        }
    }
    // §5.5: hand-drawn curves of a side replace the generated ones
    for (si, side) in [(0, Side::Out), (1, Side::In)] {
        if let Some(c) = curve(si, "volume").filter(|_| manual_volume || lanes.iter().all(|l| !(l.side == side && l.param == Param::Volume))) {
            set_lane(&mut lanes, side, Param::Volume, c.into_iter().map(|(x, db)| (x, vol_value(db_gain(db as f64)))).collect());
        }
        for (control, param) in [("eq-low", Param::EqLow), ("eq-mid", Param::EqMid), ("eq-high", Param::EqHigh)] {
            if let Some(c) = curve(si, control) {
                set_lane(&mut lanes, side, param, c.into_iter().map(|(x, g)| (x, eq_value(g as f64))).collect());
            }
        }
    }
    Transition { style: TransitionStyle::Custom, beats, lanes, locked: flag(e, "locked", false), tempo: None }
}

/// Lane of a generated (not hand-drawn) stem effect for one side (§5.4). `None` = the stem plays
/// as the track's own stem lanes say.
fn stem_mode_points(mode: &str, side: Side, pr: &Value, len: f32) -> Option<Vec<(f32, f32)>> {
    let lin = |g: f64| g as f32;
    let c_in = (num(pr, "effectCenter").unwrap_or(0.0).clamp(0.0, 1.0) as f32) * len;
    let c_out = (num(pr, "primaryEffectCenter").unwrap_or(1.0).clamp(0.0, 1.0) as f32) * len;
    match mode.trim_end_matches("_Manual") {
        "Full" => match side {
            Side::In if c_in > STEP => Some(step(len, c_in, 0.0, 1.0, lin)),
            Side::Out if c_out < len - STEP => Some(step(len, c_out, 1.0, 0.0, lin)),
            _ => None,
        },
        "Crossfade" => {
            let (a, b) = (c_in.min(c_out), c_in.max(c_out));
            Some(match side {
                Side::In => power_ramp(len, a, b, true),
                Side::Out => power_ramp(len, a, b, false),
            })
        }
        "Swap" => Some(match side {
            Side::In => step(len, c_in, 0.0, 1.0, lin),
            Side::Out => step(len, c_out, 1.0, 0.0, lin),
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            let root = std::env::temp_dir().join(format!("cc-djs-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Fixture { root }
        }

        fn put(&self, rel: &str, v: &Value) {
            let p = self.root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, serde_json::to_vec(v).unwrap()).unwrap();
        }

        /// One second of silence as a 16-bit WAV file.
        fn wav(&self, name: &str) -> PathBuf {
            let p = self.root.join("audio").join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            let (sr, n) = (44100u32, 44100u32);
            let mut b = Vec::new();
            b.extend_from_slice(b"RIFF");
            b.extend_from_slice(&(36 + n * 2).to_le_bytes());
            b.extend_from_slice(b"WAVEfmt ");
            b.extend_from_slice(&16u32.to_le_bytes());
            b.extend_from_slice(&1u16.to_le_bytes());
            b.extend_from_slice(&1u16.to_le_bytes());
            b.extend_from_slice(&sr.to_le_bytes());
            b.extend_from_slice(&(sr * 2).to_le_bytes());
            b.extend_from_slice(&2u16.to_le_bytes());
            b.extend_from_slice(&16u16.to_le_bytes());
            b.extend_from_slice(b"data");
            b.extend_from_slice(&(n * 2).to_le_bytes());
            b.resize(b.len() + n as usize * 2, 0);
            std::fs::write(&p, b).unwrap();
            p
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn beat_map(t0: f64, len: f64, n: usize) -> Value {
        let mut v: Vec<Value> = (0..n).map(|i| json!({"time": t0 + i as f64 * len, "duration": len, "trackBeatIx": i})).collect();
        v.push(json!({"time": t0 + n as f64 * len - 0.1, "duration": 0, "trackBeatIx": n}));
        Value::Array(v)
    }

    fn points(p: &[(f64, f64)]) -> Value {
        Value::Array(p.iter().map(|&(t, v)| json!({"time": t, "value": v, "flags": 0, "skew": 0, "slope": 0})).collect())
    }

    /// A project of two tracks (120 → 125 BPM) joined by a 32-beat transition.
    fn fixture(name: &str) -> Fixture {
        let f = Fixture::new(name);
        let a = f.wav("a.wav");
        let b = f.wav("b.wav");
        for (key, path, bpm, value) in [("file_a", &a, 120.0, 120.2), ("file_b", &b, 125.0, 125.25)] {
            let len = 60.0 / bpm;
            f.put(
                &format!("audio-library-table/1{key}/{key}"),
                &json!({
                    "key": key, "kind": "file",
                    "externalRec": {"fileLocationPath": path.to_string_lossy(), "lastKnownFilePath": ""},
                    "bpmLine": [{"time": 0.1, "beatNr": 0, "value": value}, {"time": 0.1 + 400.0 * len, "beatNr": 400, "value": value}],
                    "cueData": {"systemCuePoints": [{"beat": 32, "time": 99}, {"beat": 160, "time": 99}],
                                "hotCuePoints": [{"beat": -1, "time": -1}, {"beat": 64, "time": -1, "name": "Drop"}],
                                "memCuePoints": []}
                }),
            );
        }
        f.put(
            "audio-library-table/zz/file_gone",
            &json!({"key": "file_gone", "kind": "file", "externalRec": {"fileLocationPath": "/nonexistent/gone.mp3"}}),
        );
        f.put("projects-meta-table/M", &json!({"key": "M", "name": "Test Mix", "isDeleted": false, "versions": [{"key": "V0"}, {"key": "V1"}]}));
        f.put("projects-table/V0", &json!({"key": "V0", "name": "old", "mixList": []}));
        f.put(
            "projects-table/V1",
            &json!({
                "key": "V1", "name": "Test Mix", "mixBpmFixedMode": false,
                "mixList": [{"key": "V1/ta", "libraryKey": "file_a"}, {"key": "V1/tgone", "libraryKey": "file_gone"}, {"key": "V1/tb", "libraryKey": "file_b"}],
                "autoEffects": [{
                    "key": "V1/ea", "mixTrackKey": "V1/ta", "duration": 32, "locked": true,
                    "bpmLinePos": 0.75, "bpmLineLen": 0.5, "isTrackEffect": false, "isDisabled": false,
                    "effects": [
                        {"effectName": "AE_CrossFade", "effectParams": {"level": -20, "primaryLevel": -20, "range": 0.5}},
                        {"effectName": "AE_Bass_Swap", "effectParams": {"level": 0.04, "primaryLevel": 0.04, "beats": 0, "effectCenter": 0.5}},
                        {"effectName": "AE_StemsVocals_Full_Manual", "effectParams": {"primaryEnabled": false, "secondaryEnabled": true}},
                        {"effectName": "AE_StemsDrums_Full_Manual", "effectParams": {"primaryEnabled": true, "secondaryEnabled": true}},
                        {"effectName": "AE_StemsMelody_Full_Manual", "effectParams": {"primaryEnabled": true, "secondaryEnabled": true}}
                    ]
                }],
                "jingleList": [{"key": "j", "isClip": true, "title": "lost"},
                               {"key": "k", "isClip": true, "title": "B vocals", "libraryKey": "file_b", "mixDataKey": "V1/clipb"}]
            }),
        );
        f.put("mix-data-table/V1/ta", &json!({"mixMap": beat_map(0.1, 0.5, 400), "cueData": {"systemCuePoints": [{"beat": 16}, {"beat": 160}]}, "trackOutputGain": -3.0, "pitch": 0}));
        f.put("mix-data-table/V1/tb", &json!({"mixMap": beat_map(-0.2, 0.48, 400), "cueData": {"systemCuePoints": [{"beat": 32, "cueKey": "c1"}, {"beat": 300}]}, "trackOutputGain": -1.5}));
        // track b's own vocals from its beat 32, placed on its own mix-in cue: in sync with it
        let clip: Vec<Value> = (32..=100).map(|i| json!({"time": -0.2 + i as f64 * 0.48, "duration": 0.48, "trackBeatIx": i})).collect();
        f.put("mix-data-table/V1/clipb", &json!({"mixMap": clip, "linkMixRecordKey": "V1/tb", "linkMixRecordCue": 0, "linkMixRecordCueKey": "c1", "trackOutputGain": -1.5}));
        // incoming melody drawn by hand, a track-level vocal mute
        f.put("automation-data-table/V1/ea_1/virtual-stems-melody", &json!({"controlName": "virtual-stems-melody", "points": points(&[(0.0, 0.0), (16.0, 0.0), (16.0, 1.0)])}));
        f.put("automation-data-table/V1/tb/virtual-stems-vocals", &json!({"controlName": "virtual-stems-vocals", "points": points(&[(0.0, 1.0), (100.0, 1.0), (100.0, 0.0), (120.0, 0.0), (120.0, 1.0)])}));
        f
    }

    #[test]
    fn imports_library_and_project() {
        let f = fixture("import");
        let lib = Library::in_memory().unwrap();
        let r = import_all(&lib, &f.root).unwrap();
        assert_eq!(r.tracks_imported, 2);
        assert_eq!(r.missing_files, vec![PathBuf::from("/nonexistent/gone.mp3")]);
        assert_eq!(r.mixes.len(), 1);
        assert!(r.notes.iter().any(|n| n.contains("clip \"lost\" is not imported")), "{:?}", r.notes);
        assert!(!r.notes.iter().any(|n| n.contains("B vocals")), "{:?}", r.notes);
        let mix = lib.mix(r.mixes[0].0).unwrap().unwrap();
        assert_eq!(mix.name, "Test Mix");
        // the missing track is left out
        assert_eq!(mix.tracks.len(), 2);
        assert_eq!(mix.transitions.len(), 1);
        // cues of the library records: mix-in / mix-out markers and the hot cue on pad 2
        let a = lib.track_by_path(&f.root.join("audio/a.wav")).unwrap().unwrap();
        let cues = lib.cues(a.id).unwrap();
        assert_eq!(cues.len(), 3);
        let hot = cues.iter().find(|c| c.slot == Some(1)).unwrap();
        assert!((hot.pos - (0.1 + 64.0 * 0.5)).abs() < 1e-9);
        assert_eq!(hot.name, "Drop");
        assert!(cues.iter().any(|c| c.kind == CueKind::MixOut && (c.pos - 80.1).abs() < 1e-9));
        // the first track plays from its first beat, the second from its mix-in cue
        let (ta, tb) = (&mix.tracks[0], &mix.tracks[1]);
        assert!((ta.mix_in - 0.1).abs() < 1e-9);
        assert!((ta.mix_out - (0.1 + 160.0 * 0.5)).abs() < 1e-9);
        // it lands 0.256 s × (125 / 120 − 1) late
        assert!((tb.mix_in - (-0.2 + 32.0 * 0.48 - 0.256 * (125.0 / 120.0 - 1.0))).abs() < 1e-9);
        assert!((tb.mix_out - (-0.2 + 300.0 * 0.48)).abs() < 1e-9);
        // DJ.Studio's gain replaces ClubCraft's (no analysis here: 0 dB)
        assert_eq!(ta.gain_db, -3.0);
        // grids and transition tempo
        assert!((mix.grids[&ta.track_id].bpm - 120.0).abs() < 1e-9);
        assert!((mix.grids[&tb.track_id].bpm - 125.0).abs() < 1e-9);
        let t = &mix.transitions[0];
        assert_eq!(t.beats, 32);
        assert!(t.locked);
        assert_eq!(t.tempo, Some([120.2, 125.25]));
        // crossfade: incoming from -20 dB to full over the first half, outgoing out over the second
        let g = |side, x| t.value(side, Param::Volume, x).powi(2) as f64;
        assert!((g(Side::In, 0.0) - 0.1).abs() < 1e-3);
        assert!((g(Side::In, 8.0) - 0.55).abs() < 1e-2);
        assert!((g(Side::In, 16.0) - 1.0).abs() < 1e-6);
        assert!((g(Side::Out, 16.0) - 1.0).abs() < 1e-6);
        assert!((g(Side::Out, 24.0) - 0.55).abs() < 1e-2);
        assert!((g(Side::Out, 32.0) - 0.1).abs() < 1e-3);
        // bass swap in the middle
        assert!((t.value(Side::In, Param::EqLow, 15.0) - 0.1).abs() < 1e-6);
        assert!((t.value(Side::In, Param::EqLow, 16.5) - 0.5).abs() < 1e-6);
        assert!((t.value(Side::Out, Param::EqLow, 15.0) - 0.5).abs() < 1e-6);
        assert!((t.value(Side::Out, Param::EqLow, 16.5) - 0.1).abs() < 1e-6);
        // stems: switched-off outgoing vocals are silent, the hand-drawn incoming melody follows
        // its points, the other stems are left to the tracks
        let v = |side, s| t.value(side, Param::Stem(s), 10.0);
        assert_eq!(v(Side::Out, StemKind::Vocals), 0.0);
        assert_eq!(v(Side::In, StemKind::Other), 0.0);
        assert_eq!(t.value(Side::In, Param::Stem(StemKind::Other), 20.0), 1.0);
        assert!(!t.lanes.iter().any(|l| l.param == Param::Stem(StemKind::Drums)));
        // track-level stem lane, in track seconds
        let sl = tb.stems.iter().find(|l| l.stem == StemKind::Vocals).unwrap();
        assert_eq!(tb.stem_at(StemKind::Vocals, -0.2 + 110.0 * 0.48), Some(0.0));
        assert_eq!(tb.stem_at(StemKind::Vocals, -0.2 + 130.0 * 0.48), Some(1.0));
        assert!(sl.points.len() >= 5);
        // the in-sync vocal clip doubles the vocals from beat 32 to its end (beat 101)
        assert_eq!(tb.stem_at(StemKind::Vocals, -0.2 + 50.0 * 0.48), Some(2.0));
        assert_eq!(tb.stem_at(StemKind::Vocals, -0.2 + 102.0 * 0.48), Some(0.0));
    }

    #[test]
    fn reimport_updates_the_same_mix_and_keeps_samples() {
        let f = fixture("reimport");
        let lib = Library::in_memory().unwrap();
        let r = import_all(&lib, &f.root).unwrap();
        let id = r.mixes[0].0;
        let mut mix = lib.mix(id).unwrap().unwrap();
        mix.samples.push(clubcraft_core::SampleClip { lane: 0, path: "/tmp/x.wav".into(), start_beat: 4.0, gain_db: 0.0 });
        mix.lanes[0].gain_db = -2.0;
        mix.tracks[0].mix_out = 1.0; // will be replaced
        lib.save_mix(&mut mix).unwrap();
        let r2 = import_all(&lib, &f.root).unwrap();
        assert_eq!(r2.mixes[0].0, id);
        assert_eq!(lib.mixes().unwrap().len(), 1);
        let m2 = lib.mix(id).unwrap().unwrap();
        assert_eq!(m2.samples.len(), 1);
        assert_eq!(m2.lanes[0].gain_db, -2.0);
        assert!((m2.tracks[0].mix_out - 80.1).abs() < 1e-9);
        // cues are not duplicated
        let a = lib.track_by_path(&f.root.join("audio/a.wav")).unwrap().unwrap();
        assert_eq!(lib.cues(a.id).unwrap().len(), 3);
    }

    #[test]
    fn user_cues_are_kept() {
        let f = fixture("usercues");
        let lib = Library::in_memory().unwrap();
        import_all(&lib, &f.root).unwrap();
        let a = lib.track_by_path(&f.root.join("audio/a.wav")).unwrap().unwrap();
        let mut cues = lib.cues(a.id).unwrap();
        cues.push(Cue { slot: Some(1), kind: CueKind::Hot, pos: 5.0, name: "mine".into(), color: [1, 2, 3], loop_beats: None, source: CueSource::User });
        cues.retain(|c| !(c.source == CueSource::Imported && c.slot == Some(1)));
        lib.set_cues(a.id, &cues).unwrap();
        import_all(&lib, &f.root).unwrap();
        let cues = lib.cues(a.id).unwrap();
        let pad: Vec<&Cue> = cues.iter().filter(|c| c.slot == Some(1)).collect();
        assert_eq!(pad.len(), 1);
        assert_eq!(pad[0].name, "mine");
    }

    #[test]
    fn ramps_and_steps() {
        let r = ramp(32.0, 0.0, 16.0, 0.1, 1.0, vol_value);
        assert_eq!(r.first().unwrap().0, 0.0);
        assert_eq!(r.last().unwrap(), &(32.0, 1.0));
        let l = Lane::new(Side::In, Param::Volume, &r);
        // linear in amplitude
        assert!(((l.value_at(4.0) as f64).powi(2) - 0.325).abs() < 0.01);
        let s = step(32.0, 16.0, 1.0, 0.0, |g| g as f32);
        let l = Lane::new(Side::Out, Param::Stem(StemKind::Bass), &s);
        assert_eq!(l.value_at(15.9), 1.0);
        assert_eq!(l.value_at(16.0), 0.0);
    }

    #[test]
    fn overlapping_crossfade_fades_out_slower() {
        let l = Lane::new(Side::Out, Param::Volume, &cos_ramp(32.0, 0.0, 32.0, 0.1));
        let g = |x: f32| (l.value_at(x) as f64).powi(2);
        assert!((g(0.0) - 1.0).abs() < 1e-6 && (g(32.0) - 0.1).abs() < 1e-6);
        // half way: 0.1 + 0.9 cos(pi/4) = 0.736 (linear would be 0.55)
        assert!((g(16.0) - 0.7364).abs() < 2e-3, "{}", g(16.0));
    }

    #[test]
    fn stem_modes() {
        let pr = json!({"effectCenter": 0.25, "primaryEffectCenter": 0.75});
        let cf = |side| Lane::new(side, Param::Stem(StemKind::Bass), &stem_mode_points("Crossfade", side, &pr, 32.0).unwrap());
        assert_eq!(cf(Side::In).value_at(8.0), 0.0);
        // equal power: both at 1/sqrt(2) in the middle
        assert!((cf(Side::In).value_at(16.0) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((cf(Side::Out).value_at(16.0) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!(cf(Side::Out).value_at(24.0).abs() < 1e-6);
        let full = json!({"effectCenter": 0, "primaryEffectCenter": 1});
        assert!(stem_mode_points("Full", Side::In, &full, 32.0).is_none());
        assert!(stem_mode_points("Full_Manual", Side::Out, &full, 32.0).is_none());
        let early = json!({"effectCenter": 0, "primaryEffectCenter": 0.5});
        let l = Lane::new(Side::Out, Param::Stem(StemKind::Drums), &stem_mode_points("Full", Side::Out, &early, 32.0).unwrap());
        assert_eq!(l.value_at(15.0), 1.0);
        assert_eq!(l.value_at(17.0), 0.0);
    }

    #[test]
    fn grid_from_beat_map() {
        let (g, dev) = map_grid(beat_map(-0.25, 0.5, 100).as_array().unwrap()).unwrap();
        assert!((g.len - 0.5).abs() < 1e-12 && (g.t0 + 0.25).abs() < 1e-9 && dev < 1e-9);
        let bg = g.beat_grid();
        assert!((bg.bpm - 120.0).abs() < 1e-9 && (bg.downbeat - 0.25).abs() < 1e-9);
    }

    #[test]
    fn adding_a_range_to_a_lane() {
        let mut p = vec![(0.0, 1.0), (10.0, 1.0), (10.0, 0.0)];
        add_range(&mut p, 12.0, 20.0, 1.0, 1.0);
        assert_eq!(interp(&p, 5.0), 1.0);
        assert_eq!(interp(&p, 11.0), 0.0);
        assert_eq!(interp(&p, 15.0), 1.0);
        assert_eq!(interp(&p, 25.0), 0.0);
        let mut e = Vec::new();
        add_range(&mut e, 2.0, 4.0, 1.0, 1.0);
        assert_eq!(interp(&e, 3.0), 2.0);
        assert_eq!(interp(&e, 5.0), 1.0);
    }

    #[test]
    fn cuts_split_a_track() {
        // beats 0..=9, then a jump to 20..=29
        let map: Vec<f64> = (0..10).chain(20..30).map(|b| b as f64).collect();
        assert_eq!(pieces(&map, 2.0, 15.0), vec![(2.0, 9.0), (19.0, 25.0)]);
        assert_eq!(pieces(&map, 2.0, 8.0), vec![(2.0, 8.0)]);
        // the out cue may lie past the end of the map
        assert_eq!(pieces(&map[..10], 0.0, 12.0), vec![(0.0, 12.0)]);
    }

    #[test]
    fn tolerant_json() {
        let f = Fixture::new("json");
        let p = f.root.join("x");
        std::fs::write(&p, b"{\"a\": 1}trailing").unwrap();
        assert_eq!(read_json(&p).unwrap()["a"], 1);
    }

    /// Imports the real DJ.Studio database into the library at `CLUBCRAFT_HOME` (never the
    /// user's own: set the variable) and prints the report.
    #[test]
    #[ignore]
    fn real_database() {
        assert!(std::env::var_os("CLUBCRAFT_HOME").is_some(), "set CLUBCRAFT_HOME to a test library");
        let lib = Library::open_default().unwrap();
        let r = import_all(&lib, &default_root()).unwrap();
        println!("tracks {} cues {} missing {:?}", r.tracks_imported, r.cues_imported, r.missing_files);
        for (id, name) in &r.mixes {
            let m = lib.mix(*id).unwrap().unwrap();
            println!("mix {id} {name}: {} tracks, {} transitions", m.tracks.len(), m.transitions.len());
        }
        for n in &r.notes {
            println!("note: {n}");
        }
        assert!(!r.mixes.is_empty());
    }

    /// Renders an imported mix of the library at `CLUBCRAFT_HOME` to a WAV file, for comparing
    /// with DJ.Studio's export: `DJS_MIX=<mix name> DJS_OUT=<file.wav>`.
    #[test]
    #[ignore]
    fn render_imported_mix() {
        assert!(std::env::var_os("CLUBCRAFT_HOME").is_some(), "set CLUBCRAFT_HOME to a test library");
        let name = std::env::var("DJS_MIX").expect("DJS_MIX");
        let out = PathBuf::from(std::env::var("DJS_OUT").expect("DJS_OUT"));
        let lib = Library::open_default().unwrap();
        let (id, _, _) = lib.mixes().unwrap().into_iter().find(|m| m.1 == name).expect("no such mix");
        let mix = lib.mix(id).unwrap().unwrap();
        let all = lib.all_analyses().unwrap();
        let mut opts = clubcraft_engine::RenderOptions { format: clubcraft_engine::WavFormat::Pcm16, ..Default::default() };
        // optional master settings: DJS_MASTER_DB (gain into the limiter), DJS_CEILING_DB
        if let Some(g) = std::env::var("DJS_MASTER_DB").ok().and_then(|v| v.parse().ok()) {
            opts.master_gain_db = g;
        }
        if let Some(c) = std::env::var("DJS_CEILING_DB").ok().and_then(|v| v.parse().ok()) {
            opts.ceiling_db = c;
        }
        let rep = clubcraft_engine::render_mix(&mix, &|id| all.get(&id).cloned(), &|id| crate::live::adapter::load_uncached(id).ok(), &out, &opts, &mut |_| true).unwrap();
        println!("rendered {:.2} s (mix {:.2} s), peak {:.3}", rep.seconds, rep.timeline.total_secs, rep.peak);
        for (i, t) in rep.timeline.tracks.iter().enumerate() {
            println!("track {i}: {:.3} .. {:.3} s", t.start_sec, t.end_sec);
        }
        for t in &rep.timeline.transitions {
            println!("transition {}: {:.3} .. {:.3} s", t.out, t.start_sec, t.end_sec);
        }
    }

    /// Separates (fast model) the stems of every track of an imported mix that has none yet, into
    /// the library at `CLUBCRAFT_HOME`: `DJS_MIX=<mix name>`. One track at a time.
    #[test]
    #[ignore]
    fn separate_mix_tracks() {
        assert!(std::env::var_os("CLUBCRAFT_HOME").is_some(), "set CLUBCRAFT_HOME to a test library");
        let name = std::env::var("DJS_MIX").expect("DJS_MIX");
        let lib = Library::open_default().unwrap();
        let (id, _, _) = lib.mixes().unwrap().into_iter().find(|m| m.1 == name).expect("no such mix");
        let mix = lib.mix(id).unwrap().unwrap();
        let sep = clubcraft_stems::Separator::new(clubcraft_stems::Quality::Fast).unwrap();
        let mut done = HashSet::new();
        for mt in &mix.tracks {
            let t = lib.track(mt.track_id).unwrap().unwrap();
            let path = clubcraft_core::library::stems_path(&t.hash);
            if !done.insert(t.id) || path.exists() {
                continue;
            }
            let audio = clubcraft_audio::decode_file(&t.path, clubcraft_core::SAMPLE_RATE).unwrap();
            let stems = sep.separate(&audio, &mut |_| true).unwrap();
            clubcraft_stems::save_stems_with_model(&path, &stems, sep.model_name()).unwrap();
            println!("stems: {}", t.path.display());
        }
    }

    /// Imports one saved version of a project (not necessarily the current one) as a mix named
    /// `DJS_NAME`, into the library at `CLUBCRAFT_HOME`: for comparing with an export that was
    /// made from that version (`DJS_VERSION=<projects-table key>`).
    #[test]
    #[ignore]
    fn import_one_version() {
        assert!(std::env::var_os("CLUBCRAFT_HOME").is_some(), "set CLUBCRAFT_HOME to a test library");
        let (version, name) = (std::env::var("DJS_VERSION").expect("DJS_VERSION"), std::env::var("DJS_NAME").expect("DJS_NAME"));
        let lib = Library::open_default().unwrap();
        let root = default_root();
        let mut report = ImportReport { tracks_imported: 0, cues_imported: 0, missing_files: Vec::new(), mixes: Vec::new(), notes: Vec::new() };
        let tracks = import_library(&lib, &root, &mut report).unwrap();
        let analyses = lib.all_analyses().unwrap();
        let (mut mix, notes) = convert_project(&root, &version, &tracks, &|id| analyses.get(&id).and_then(|a| a.gain_db).unwrap_or(0.0)).unwrap();
        mix.name = name;
        let id = save_mix(&lib, mix).unwrap();
        println!("mix {id}; notes: {notes:?}");
    }
}
