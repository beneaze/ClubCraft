//! `clubcraft mcp`: a Model Context Protocol server over stdio (JSON-RPC 2.0, one message per
//! line). It works on the same library as the app, so an assistant can search tracks, run
//! analysis and stem separation, build and edit mixes, set hot cues and export. Heavy work is
//! serialised with a running app through [`crate::jobs::heavy`]; after every change the app is
//! told to reload through [`notify_app`]. Logs go to stderr, stdout carries only protocol messages.

use anyhow::{anyhow, bail, Context, Result};
use clubcraft_core::planner::{self, OrderItem, PlanOptions};
use clubcraft_core::{Analysis, Cue, CueKind, CueSource, Key, Library, Mix, MixTimeline, TempoMode, Track, TrackId, Transition, TransitionStyle};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

const PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "ClubCraft is a DJ app: a library of analysed tracks (BPM, key in Camelot notation, energy 1-10, \
sections, eight automatic hot cues), cached stems (drums, bass, melody, vocals) and studio mixes (an ordered list of tracks \
joined by beat-matched transitions). Times are seconds in the original track unless a field says otherwise. Typical flow: \
search_tracks → create_mix (harmonize orders by key, tempo and energy) → get_mix → set_transition / set_mix_points → export_mix. \
separate_stems and export_mix take minutes; they run one at a time, also with the ClubCraft app. Changes show up in a running app.";

type Out = Arc<Mutex<std::io::Stdout>>;

fn send(out: &Out, v: &Value) {
    let mut o = out.lock();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

/// Serve MCP on stdin/stdout until stdin closes.
pub fn run() -> Result<()> {
    clubcraft_core::paths::ensure_dirs()?;
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let running: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>> = Default::default();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                send(&out, &json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}}));
                continue;
            }
        };
        let msgs = match msg {
            Value::Array(a) => a,
            v => vec![v],
        };
        for m in msgs {
            dispatch(m, &out, &running);
        }
    }
    Ok(())
}

fn dispatch(m: Value, out: &Out, running: &Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>) {
    let Some(method) = m.get("method").and_then(|v| v.as_str()).map(str::to_string) else { return }; // a response
    let params = m.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = m.get("id").cloned() else {
        if method == "notifications/cancelled" {
            if let Some(c) = params.get("requestId").and_then(|r| running.lock().get(&r.to_string()).cloned()) {
                c.store(true, Ordering::Relaxed);
            }
        }
        return;
    };
    let reply = |r: Result<Value, (i64, String)>| match r {
        Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
        Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    };
    match method.as_str() {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOLS[0]);
            let version = if PROTOCOLS.contains(&asked) { asked } else { PROTOCOLS[0] };
            send(
                out,
                &reply(Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "clubcraft", "title": "ClubCraft", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }))),
            );
        }
        "ping" => send(out, &reply(Ok(json!({})))),
        "tools/list" => send(out, &reply(Ok(json!({"tools": tools()})))),
        "resources/list" => send(out, &reply(Ok(json!({"resources": []})))),
        "prompts/list" => send(out, &reply(Ok(json!({"prompts": []})))),
        "tools/call" => {
            // Each call runs on its own thread so pings and cancellations are answered meanwhile.
            let key = id.to_string();
            let cancel = Arc::new(AtomicBool::new(false));
            running.lock().insert(key.clone(), cancel.clone());
            let (out, running) = (out.clone(), running.clone());
            std::thread::spawn(move || {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let token = params.get("_meta").and_then(|m| m.get("progressToken")).cloned();
                let call = Call { out: out.clone(), token, cancel: cancel.clone(), last: Mutex::new((-1.0, String::new())) };
                let result = match call_tool(&name, &args, &call) {
                    Ok(v) => json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}]}),
                    Err(e) => json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true}),
                };
                running.lock().remove(&key);
                if !cancel.load(Ordering::Relaxed) {
                    send(&out, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
                }
            });
        }
        _ => send(out, &reply(Err((-32601, format!("method not found: {method}"))))),
    }
}

/// One running tool call: progress notifications and cancellation.
struct Call {
    out: Out,
    token: Option<Value>,
    cancel: Arc<AtomicBool>,
    last: Mutex<(f64, String)>,
}

impl Call {
    /// Report `done` of `total` (throttled to 1 % steps or a new message).
    fn progress(&self, done: f64, total: f64, msg: &str) {
        let Some(token) = &self.token else { return };
        {
            let mut last = self.last.lock();
            if (done - last.0).abs() < total * 0.01 && last.1 == msg {
                return;
            }
            *last = (done, msg.to_string());
        }
        send(&self.out, &json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {"progressToken": token, "progress": done, "total": total, "message": msg}}));
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Tell a running app that the library changed (it polls this file's modification time).
pub fn notify_app() {
    let _ = std::fs::write(change_marker(), crate::platform::timestamp());
}

pub fn change_marker() -> std::path::PathBuf {
    clubcraft_core::paths::data_dir().join(".library-changed")
}

// ------------------------------------------------------------------ tool definitions

fn tools() -> Value {
    let ids = json!({"type": "array", "items": {"type": "integer"}, "description": "Track ids (from search_tracks)."});
    let style = json!({"type": "string", "enum": ["stem_blend", "bass_swap", "crossfade", "vocal_over", "filter_out", "echo_out", "cut", "mashup"], "description": "Transition style. stem_blend, vocal_over and mashup need stems on both tracks (bass_swap is played otherwise). mashup: the outgoing vocals sing over the incoming instrumental for the whole transition."});
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let rw = json!({"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false});
    let tool = |name: &str, title: &str, desc: &str, props: Value, required: &[&str], ann: &Value| {
        json!({"name": name, "title": title, "description": desc, "inputSchema": {"type": "object", "properties": props, "required": required}, "annotations": ann})
    };
    json!([
        tool("library_overview", "Library overview", "Counts of tracks, analysed tracks, tracks with stems and mixes, and where the library lives.", json!({}), &[], &ro),
        tool(
            "search_tracks",
            "Search tracks",
            "Find tracks by text (title, artist, album, genre; every word must match), tempo, key, energy or stems. Returns id, title, artist, duration, BPM, Camelot key, energy (1-10) and the stem model.",
            json!({
                "query": {"type": "string"},
                "bpm_min": {"type": "number"}, "bpm_max": {"type": "number"},
                "key": {"type": "string", "description": "Exact key, Camelot (8A) or musical (Am)."},
                "compatible_with": {"type": "string", "description": "Only keys that mix harmonically with this key (same, ±1, relative)."},
                "energy_min": {"type": "integer"}, "energy_max": {"type": "integer"},
                "has_stems": {"type": "boolean"},
                "sort": {"type": "string", "enum": ["title", "artist", "bpm", "key", "energy", "added"]},
                "limit": {"type": "integer", "description": "Default 50."}
            }),
            &[],
            &ro,
        ),
        tool("get_track", "Track details", "Everything known about one track: tags, path, beat grid, key, loudness, energy, sections, and the hot cues the live decks use.", json!({"track_id": {"type": "integer"}}), &["track_id"], &ro),
        tool(
            "import_tracks",
            "Import tracks",
            "Add audio files or folders (recursively) to the library and run the quick analysis (BPM, key, cues; ~1 s per track). Stems are not separated; use separate_stems.",
            json!({"paths": {"type": "array", "items": {"type": "string"}, "description": "Absolute file or folder paths."}, "analyze": {"type": "boolean", "description": "Default true."}}),
            &["paths"],
            &rw,
        ),
        tool(
            "analyze_tracks",
            "Analyse tracks",
            "Run the full analysis again (uses stems when present for better sections and cues). Hot cues you set yourself are kept.",
            json!({"track_ids": ids, "deep_structure": {"type": "boolean", "description": "Use the All-In-One structure model (needs stems, ~1 min per track)."}}),
            &["track_ids"],
            &rw,
        ),
        tool(
            "separate_stems",
            "Separate stems",
            "Separate tracks into drums, bass, melody and vocals (cached; tracks that already have stems of the requested quality are skipped), then re-analyse them. Takes ~1-4 min per track on a GPU.",
            json!({"track_ids": ids, "quality": {"type": "string", "enum": ["fast", "high", "ultra", "best"], "description": "Default: the app setting (best = Mel-Band RoFormer vocals + htdemucs_ft)."}}),
            &["track_ids"],
            &rw,
        ),
        tool(
            "set_hot_cue",
            "Set hot cue",
            "Set or clear hot cue pad 1-8 of a track. Positions snap to the beat grid unless snap is \"none\". Clearing a pad brings back its automatic cue, if any.",
            json!({
                "track_id": {"type": "integer"},
                "pad": {"type": "integer", "minimum": 1, "maximum": 8},
                "position_sec": {"type": "number"},
                "name": {"type": "string"},
                "snap": {"type": "string", "enum": ["beat", "bar", "none"], "description": "Default beat."},
                "clear": {"type": "boolean"}
            }),
            &["track_id", "pad"],
            &rw,
        ),
        tool("list_mixes", "List mixes", "All studio mixes with track count, length and last change.", json!({}), &[], &ro),
        tool(
            "get_mix",
            "Mix details",
            "A mix as the studio shows it: each track with its mix-in / mix-out points, key shift and when it plays in the mix, and each transition with style, length and how well the keys match.",
            json!({"mix_id": {"type": "integer"}}),
            &["mix_id"],
            &ro,
        ),
        tool(
            "create_mix",
            "Create mix",
            "Create a studio mix from tracks. With harmonize (default true) the order is optimised for key, tempo and a rising energy arc; mix points come from the cues and key shifts fix clashes.",
            json!({
                "name": {"type": "string"},
                "track_ids": ids,
                "harmonize": {"type": "boolean"},
                "transition_beats": {"type": "integer", "description": "Preferred transition length (16, 32, 64 ...). Default: chosen per track."},
                "style": style
            }),
            &["name", "track_ids"],
            &rw,
        ),
        tool(
            "edit_mix",
            "Edit mix",
            "Rename a mix, change tempo mode or keylock, or replace / reorder its tracks. Changing tracks or harmonize re-plans all mix points and transitions.",
            json!({
                "mix_id": {"type": "integer"},
                "name": {"type": "string"},
                "track_ids": ids,
                "harmonize": {"type": "boolean", "description": "Re-order the tracks harmonically."},
                "tempo": {"description": "\"glide\" (each track at its own BPM, gliding in transitions) or a fixed BPM number."},
                "keylock": {"type": "boolean"}
            }),
            &["mix_id"],
            &rw,
        ),
        tool(
            "set_transition",
            "Set transition",
            "Change the style and/or length of transition `index` (0 = between the first and second track).",
            json!({"mix_id": {"type": "integer"}, "index": {"type": "integer"}, "style": style, "beats": {"type": "integer", "description": "Length in beats, multiple of 4 (16 = 4 bars)."}}),
            &["mix_id", "index"],
            &rw,
        ),
        tool(
            "set_mix_points",
            "Set mix points",
            "Change where track `index` of a mix starts (mix_in_sec: aligned with the previous track's mix-out) or where the transition to the next track starts (mix_out_sec), its key shift in semitones or its gain.",
            json!({
                "mix_id": {"type": "integer"}, "index": {"type": "integer"},
                "mix_in_sec": {"type": "number"}, "mix_out_sec": {"type": "number"},
                "key_shift": {"type": "integer"}, "gain_db": {"type": "number"},
                "snap": {"type": "string", "enum": ["beat", "bar", "none"], "description": "Default bar."}
            }),
            &["mix_id", "index"],
            &rw,
        ),
        tool(
            "find_mashups",
            "Find mashups",
            "Find where the vocals of a track fit over the instrumental of other tracks (vocals of A over the instrumental of B, with key shift and tempo change). Scores every 16-bar placement bar by bar for harmony, masking and leftover vocals, then re-ranks the best with a neural network (COCOLA). Needs stems on both tracks. Bars are 1-based (bar 1 = first downbeat).",
            json!({
                "vocal_track_id": {"type": "integer"},
                "vocal_bar": {"type": "integer", "description": "Use the vocal phrase starting at this bar. Default: the strongest vocal phrases."},
                "bars": {"type": "integer", "enum": [8, 16, 32], "description": "Phrase length, default 16."},
                "key_range": {"type": "integer", "description": "Allowed key shift of the vocals in ± semitones, default 2."},
                "tempo_tolerance_pct": {"type": "number", "description": "Allowed tempo change of the vocals in ± %, default 6."},
                "instrumental_track_ids": ids,
                "rerank": {"type": "boolean", "description": "Neural re-ranking of the best 20 (a few seconds), default true."},
                "limit": {"type": "integer", "description": "Default 15."}
            }),
            &["vocal_track_id"],
            &ro,
        ),
        tool(
            "create_mashup_mix",
            "Create mashup mix",
            "Create a two-track mix from a find_mashups result: the vocal track plays 16 bars before its phrase, then its vocals sing over the instrumental, which continues to its end.",
            json!({
                "vocal_track_id": {"type": "integer"}, "instrumental_track_id": {"type": "integer"},
                "vocal_bar": {"type": "integer"}, "instrumental_bar": {"type": "integer"},
                "bars": {"type": "integer"}, "key_shift": {"type": "integer"},
                "name": {"type": "string"}
            }),
            &["vocal_track_id", "instrumental_track_id", "vocal_bar", "instrumental_bar"],
            &rw,
        ),
        tool(
            "export_mix",
            "Export mix",
            "Render a mix to a WAV file (plus a .cue track list if enabled in Settings). Takes about 1/20 of the mix length.",
            json!({"mix_id": {"type": "integer"}, "path": {"type": "string", "description": "Output .wav path. Default: Music/ClubCraft/Exports/<mix name> <date>.wav"}, "format": {"type": "string", "enum": ["wav16", "wav24", "wav32f"]}}),
            &["mix_id"],
            &rw,
        ),
        tool(
            "export_virtualdj",
            "Export to VirtualDJ",
            "Write ClubCraft's beat grids, keys and hot cues into a VirtualDJ database.xml (a backup is made first). VirtualDJ must be closed.",
            json!({"path": {"type": "string", "description": "database.xml path. Default: Documents/VirtualDJ/database.xml"}, "track_ids": ids}),
            &[],
            &json!({"readOnlyHint": false, "destructiveHint": false, "openWorldHint": true}),
        ),
    ])
}

// ------------------------------------------------------------------ argument helpers

fn arg_i64(a: &Value, k: &str) -> Result<i64> {
    a.get(k).and_then(|v| v.as_i64()).ok_or_else(|| anyhow!("missing integer argument `{k}`"))
}

fn opt_f64(a: &Value, k: &str) -> Option<f64> {
    a.get(k).and_then(|v| v.as_f64())
}

fn opt_str<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(|v| v.as_str())
}

fn opt_bool(a: &Value, k: &str) -> Option<bool> {
    a.get(k).and_then(|v| v.as_bool())
}

fn ids_arg(a: &Value, k: &str) -> Result<Vec<TrackId>> {
    let v = a.get(k).and_then(|v| v.as_array()).ok_or_else(|| anyhow!("missing array argument `{k}`"))?;
    v.iter().map(|x| x.as_i64().ok_or_else(|| anyhow!("`{k}` must contain integers"))).collect()
}

fn parse_key(s: &str) -> Result<Key> {
    Key::from_camelot(s.trim()).or_else(|| Key::parse(s.trim())).ok_or_else(|| anyhow!("unknown key `{s}` (use e.g. 8A or Am)"))
}

fn parse_style(s: &str) -> Result<TransitionStyle> {
    let norm = |x: &str| x.to_ascii_lowercase().replace([' ', '_', '-'], "");
    TransitionStyle::ALL
        .into_iter()
        .filter(|t| *t != TransitionStyle::Custom)
        .find(|t| norm(t.label()) == norm(s))
        .ok_or_else(|| anyhow!("unknown transition style `{s}`"))
}

fn style_id(t: TransitionStyle) -> String {
    t.label().to_ascii_lowercase().replace(' ', "_")
}

fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

// ------------------------------------------------------------------ views

fn track_brief(t: &Track, a: Option<&Analysis>) -> Value {
    json!({
        "id": t.id,
        "title": t.display_name(),
        "artist": t.artist,
        "duration_sec": r2(a.map(|a| a.duration).filter(|d| *d > 0.0).unwrap_or(t.duration)),
        "bpm": a.and_then(|a| a.bpm()).map(r2),
        "key": a.and_then(|a| a.key).map(|k| k.camelot()),
        "key_name": a.and_then(|a| a.key).map(|k| k.name()),
        "energy": a.and_then(|a| a.energy),
        "stems": a.and_then(|a| a.stems_model.clone()),
        "analysed": a.is_some_and(|a| a.version > 0),
    })
}

fn cue_view(c: &Cue) -> Value {
    json!({
        "pad": c.slot.map(|s| s + 1),
        "name": c.name,
        "kind": c.kind.label(),
        "position_sec": r2(c.pos),
        "loop_beats": c.loop_beats,
        "source": format!("{:?}", c.source).to_lowercase(),
    })
}

fn mix_view(lib: &Library, mix: &Mix, analyses: &HashMap<TrackId, Analysis>) -> Value {
    let tl = MixTimeline::build(mix, &|id| analyses.get(&id).cloned());
    let played_key = |i: usize| -> Option<Key> {
        let t = mix.tracks.get(i)?;
        analyses.get(&t.track_id)?.key.map(|k| k.shifted(t.key_shift))
    };
    let tracks: Vec<Value> = mix
        .tracks
        .iter()
        .enumerate()
        .map(|(i, mt)| {
            let t = lib.track(mt.track_id).ok().flatten();
            let a = analyses.get(&mt.track_id);
            json!({
                "index": i,
                "track_id": mt.track_id,
                "title": t.as_ref().map(|t| t.display_name()),
                "artist": t.as_ref().map(|t| t.artist.clone()),
                "bpm": a.and_then(|a| a.bpm()).map(r2),
                "key": a.and_then(|a| a.key).map(|k| k.camelot()),
                "key_shift": mt.key_shift,
                "played_key": played_key(i).map(|k| k.camelot()),
                "mix_in_sec": r2(mt.mix_in),
                "mix_out_sec": r2(mt.mix_out),
                "gain_db": mt.gain_db,
                "starts_at_sec": tl.tracks.get(i).map(|t| r2(t.start_sec)),
                "ends_at_sec": tl.tracks.get(i).map(|t| r2(t.end_sec)),
            })
        })
        .collect();
    let transitions: Vec<Value> = mix
        .transitions
        .iter()
        .enumerate()
        .map(|(i, tr)| {
            let at = tl.transitions.get(i);
            json!({
                "index": i,
                "from_track": i,
                "to_track": i + 1,
                "style": style_id(tr.style),
                "beats": tr.beats,
                "starts_at_sec": at.map(|t| r2(t.start_sec)),
                "ends_at_sec": at.map(|t| r2(t.end_sec)),
                "key_match": played_key(i).zip(played_key(i + 1)).map(|(a, b)| a.compatibility(b)),
            })
        })
        .collect();
    json!({
        "id": mix.id,
        "name": mix.name,
        "tempo": match mix.tempo { TempoMode::Glide => json!("glide"), TempoMode::Fixed(b) => json!(b) },
        "keylock": mix.keylock,
        "duration_sec": r2(tl.total_secs),
        "tracks": tracks,
        "transitions": transitions,
    })
}

fn load_mix(lib: &Library, id: i64) -> Result<Mix> {
    let mut m = lib.mix(id)?.ok_or_else(|| anyhow!("no mix with id {id}"))?;
    m.normalize();
    Ok(m)
}

fn save_and_view(lib: &Library, mix: &mut Mix) -> Result<Value> {
    mix.normalize();
    lib.save_mix(mix)?;
    notify_app();
    Ok(mix_view(lib, mix, &lib.all_analyses()?))
}

fn plan(name: &str, order: &[TrackId], analyses: &HashMap<TrackId, Analysis>, beats: Option<u32>, style: Option<TransitionStyle>) -> Mix {
    let o = PlanOptions { transition_beats: beats, style, ..Default::default() };
    planner::plan_mix(name, order, &|id| analyses.get(&id).cloned(), &o)
}

fn harmonic_order(ids: &[TrackId], analyses: &HashMap<TrackId, Analysis>) -> Vec<TrackId> {
    if ids.len() <= 2 {
        return ids.to_vec();
    }
    let items: Vec<OrderItem> = ids.iter().map(|id| OrderItem::from_analysis(*id, analyses.get(id))).collect();
    planner::order_tracks(&items, &Default::default())
}

fn job_settings() -> crate::pipeline::Settings {
    crate::app::prefs_job_settings(&crate::prefs::Prefs::load())
}

// ------------------------------------------------------------------ tools

fn call_tool(name: &str, a: &Value, call: &Call) -> Result<Value> {
    let lib = Library::open_default()?;
    match name {
        "library_overview" => {
            let tracks = lib.tracks()?;
            let an = lib.all_analyses()?;
            Ok(json!({
                "data_dir": clubcraft_core::paths::data_dir(),
                "tracks": tracks.len(),
                "analysed": an.values().filter(|a| a.version > 0).count(),
                "with_stems": an.values().filter(|a| a.stems_model.is_some()).count(),
                "mixes": lib.mixes()?.len(),
                "default_stem_quality": job_settings().quality,
            }))
        }
        "search_tracks" => search(&lib, a),
        "get_track" => {
            let id = arg_i64(a, "track_id")?;
            let t = lib.track(id)?.ok_or_else(|| anyhow!("no track with id {id}"))?;
            let an = lib.analysis(id)?;
            let mut v = track_brief(&t, an.as_ref());
            let cues = clubcraft_core::effective_cues(an.as_ref().map(|a| a.cues.as_slice()).unwrap_or(&[]), &lib.cues(id)?);
            let extra = json!({
                "album": t.album, "genre": t.genre, "year": t.year, "path": t.path,
                "tag_bpm": t.tag_bpm, "tag_key": t.tag_key, "rating": t.rating, "play_count": t.play_count,
                "downbeat_sec": an.as_ref().and_then(|a| a.grid).map(|g| r2(g.downbeat)),
                "loudness_lufs": an.as_ref().and_then(|a| a.loudness_lufs),
                "sections": an.as_ref().map(|a| a.sections.iter().map(|s| json!({"kind": s.kind.label(), "start_sec": r2(s.start), "end_sec": r2(s.end), "bars": s.bars})).collect::<Vec<_>>()),
                "cues": cues.iter().map(cue_view).collect::<Vec<_>>(),
            });
            if let (Some(o), Value::Object(e)) = (v.as_object_mut(), extra) {
                o.extend(e);
            }
            Ok(v)
        }
        "import_tracks" => import(&lib, a, call),
        "analyze_tracks" => {
            let ids = ids_arg(a, "track_ids")?;
            let mut s = job_settings();
            if let Some(d) = opt_bool(a, "deep_structure") {
                s.deep_structure = d;
            }
            let mut done = vec![];
            for (i, &id) in ids.iter().enumerate() {
                if call.cancelled() {
                    bail!("cancelled");
                }
                let t = lib.track(id)?.ok_or_else(|| anyhow!("no track with id {id}"))?;
                let n = ids.len() as f64;
                let has_stems = clubcraft_core::library::stems_path(&t.hash).exists();
                let _heavy = if has_stems || s.deep_structure { Some(crate::jobs::heavy()) } else { None };
                crate::pipeline::analyze(&lib, id, &s, &mut |p, label| call.progress(i as f64 + p as f64, n, &format!("{}: {label}", t.display_name())))?;
                done.push(track_brief(&t, lib.analysis(id)?.as_ref()));
                notify_app();
            }
            Ok(json!({"analysed": done}))
        }
        "separate_stems" => separate(&lib, a, call),
        "set_hot_cue" => {
            let id = arg_i64(a, "track_id")?;
            let pad = arg_i64(a, "pad")?;
            if !(1..=8).contains(&pad) {
                bail!("pad must be 1-8");
            }
            lib.track(id)?.ok_or_else(|| anyhow!("no track with id {id}"))?;
            let slot = (pad - 1) as u8;
            let mut cues = lib.cues(id)?;
            cues.retain(|c| c.slot != Some(slot));
            if !opt_bool(a, "clear").unwrap_or(false) {
                let mut pos = opt_f64(a, "position_sec").ok_or_else(|| anyhow!("missing `position_sec` (or set clear)"))?;
                if let Some(g) = lib.analysis(id)?.and_then(|a| a.grid) {
                    pos = match opt_str(a, "snap").unwrap_or("beat") {
                        "none" => pos,
                        "bar" => g.snap(pos, g.beats_per_bar as f64),
                        _ => g.snap(pos, 1.0),
                    };
                }
                cues.push(Cue {
                    slot: Some(slot),
                    kind: CueKind::Hot,
                    pos: pos.max(0.0),
                    name: opt_str(a, "name").map(str::to_string).unwrap_or_else(|| format!("Cue {pad}")),
                    color: CueKind::Hot.default_color(),
                    loop_beats: None,
                    source: CueSource::User,
                });
            }
            lib.set_cues(id, &cues)?;
            notify_app();
            let auto = lib.analysis(id)?.map(|a| a.cues).unwrap_or_default();
            Ok(json!({"cues": clubcraft_core::effective_cues(&auto, &cues).iter().map(cue_view).collect::<Vec<_>>()}))
        }
        "list_mixes" => {
            let analyses = lib.all_analyses()?;
            let mixes: Vec<Value> = lib
                .mixes()?
                .into_iter()
                .filter_map(|(id, name, modified)| {
                    let m = lib.mix(id).ok().flatten()?;
                    let tl = MixTimeline::build(&m, &|t| analyses.get(&t).cloned());
                    let when = chrono::DateTime::from_timestamp(modified, 0).map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string());
                    Some(json!({"id": id, "name": name, "tracks": m.tracks.len(), "duration_sec": r2(tl.total_secs), "modified": when}))
                })
                .collect();
            Ok(json!({"mixes": mixes}))
        }
        "get_mix" => {
            let m = load_mix(&lib, arg_i64(a, "mix_id")?)?;
            Ok(mix_view(&lib, &m, &lib.all_analyses()?))
        }
        "create_mix" => {
            let name = opt_str(a, "name").filter(|s| !s.trim().is_empty()).ok_or_else(|| anyhow!("missing `name`"))?;
            let ids = ids_arg(a, "track_ids")?;
            for &id in &ids {
                lib.track(id)?.ok_or_else(|| anyhow!("no track with id {id}"))?;
            }
            let analyses = lib.all_analyses()?;
            let order = if opt_bool(a, "harmonize").unwrap_or(true) { harmonic_order(&ids, &analyses) } else { ids };
            let style = opt_str(a, "style").map(parse_style).transpose()?;
            let beats = a.get("transition_beats").and_then(|v| v.as_u64()).map(|b| (b as u32 / 4 * 4).clamp(4, 256));
            let mut mix = plan(name, &order, &analyses, beats, style);
            mix.keylock = crate::prefs::Prefs::load().mix_keylock;
            save_and_view(&lib, &mut mix)
        }
        "edit_mix" => {
            let mut mix = load_mix(&lib, arg_i64(a, "mix_id")?)?;
            if let Some(n) = opt_str(a, "name").filter(|s| !s.trim().is_empty()) {
                mix.name = n.to_string();
            }
            if let Some(k) = opt_bool(a, "keylock") {
                mix.keylock = k;
            }
            match a.get("tempo") {
                Some(Value::String(s)) if s.eq_ignore_ascii_case("glide") => mix.tempo = TempoMode::Glide,
                Some(v) if v.is_number() => mix.tempo = TempoMode::Fixed(v.as_f64().unwrap_or(120.0).clamp(60.0, 200.0)),
                Some(v) if !v.is_null() => bail!("tempo must be \"glide\" or a BPM number"),
                _ => {}
            }
            let new_ids = if a.get("track_ids").is_some() { Some(ids_arg(a, "track_ids")?) } else { None };
            let harmonize = opt_bool(a, "harmonize").unwrap_or(false);
            if new_ids.is_some() || harmonize {
                let ids = new_ids.unwrap_or_else(|| mix.tracks.iter().map(|t| t.track_id).collect());
                for &id in &ids {
                    lib.track(id)?.ok_or_else(|| anyhow!("no track with id {id}"))?;
                }
                let analyses = lib.all_analyses()?;
                let order = if harmonize { harmonic_order(&ids, &analyses) } else { ids };
                let planned = plan(&mix.name, &order, &analyses, None, None);
                mix.tracks = planned.tracks;
                mix.transitions = planned.transitions;
            }
            save_and_view(&lib, &mut mix)
        }
        "set_transition" => {
            let mut mix = load_mix(&lib, arg_i64(a, "mix_id")?)?;
            let i = arg_i64(a, "index")? as usize;
            let n = mix.transitions.len();
            let tr = mix.transitions.get_mut(i).ok_or_else(|| anyhow!("transition index {i} out of range (mix has {n})"))?;
            let beats = a.get("beats").and_then(|v| v.as_u64()).map(|b| (b as u32 / 4 * 4).clamp(4, 256)).unwrap_or(tr.beats);
            let style = opt_str(a, "style").map(parse_style).transpose()?.unwrap_or(tr.style);
            if style == TransitionStyle::Custom {
                // keep hand-drawn curves, stretched to the new length
                let k = beats as f32 / tr.beats.max(1) as f32;
                for l in &mut tr.lanes {
                    for p in &mut l.points {
                        p.0 *= k;
                    }
                }
                tr.beats = beats;
            } else {
                *tr = Transition::new(style, beats);
            }
            save_and_view(&lib, &mut mix)
        }
        "set_mix_points" => {
            let mut mix = load_mix(&lib, arg_i64(a, "mix_id")?)?;
            let i = arg_i64(a, "index")? as usize;
            let n = mix.tracks.len();
            let mt = mix.tracks.get_mut(i).ok_or_else(|| anyhow!("track index {i} out of range (mix has {n})"))?;
            let an = lib.analysis(mt.track_id)?;
            let snap = |t: f64| match (an.as_ref().and_then(|a| a.grid), opt_str(a, "snap").unwrap_or("bar")) {
                (Some(g), "bar") => g.snap(t, g.beats_per_bar as f64),
                (Some(g), "beat") => g.snap(t, 1.0),
                _ => t,
            };
            let dur = an.as_ref().map(|a| a.duration).unwrap_or(f64::MAX);
            if let Some(t) = opt_f64(a, "mix_in_sec") {
                mt.mix_in = snap(t).clamp(0.0, dur);
            }
            if let Some(t) = opt_f64(a, "mix_out_sec") {
                mt.mix_out = snap(t).clamp(0.0, dur);
            }
            if mt.mix_out < mt.mix_in + 1.0 {
                bail!("mix_out ({:.1} s) must be after mix_in ({:.1} s)", mt.mix_out, mt.mix_in);
            }
            if let Some(k) = a.get("key_shift").and_then(|v| v.as_i64()) {
                mt.key_shift = k.clamp(-6, 6) as i32;
            }
            if let Some(g) = opt_f64(a, "gain_db") {
                mt.gain_db = g.clamp(-24.0, 12.0) as f32;
            }
            save_and_view(&lib, &mut mix)
        }
        "find_mashups" => find_mashups(&lib, a, call),
        "create_mashup_mix" => {
            let analyses = lib.all_analyses()?;
            let (v, i) = (arg_i64(a, "vocal_track_id")?, arg_i64(a, "instrumental_track_id")?);
            for id in [v, i] {
                if analyses.get(&id).and_then(|x| x.grid).is_none() {
                    bail!("track {id} is not analysed");
                }
            }
            let m = clubcraft_analysis::mashup::Match {
                vocal_track: v,
                vocal_bar: arg_i64(a, "vocal_bar")? as i32 - 1,
                inst_track: i,
                inst_bar: arg_i64(a, "instrumental_bar")? as i32 - 1,
                bars: a.get("bars").and_then(|x| x.as_u64()).unwrap_or(16).clamp(4, 64) as usize,
                key_shift: a.get("key_shift").and_then(|x| x.as_i64()).unwrap_or(0).clamp(-6, 6) as i32,
                tempo_ratio: 1.0,
                score: 0.0,
                harmony: 0.0,
                harmony_min: 0.0,
                masking: 0.0,
                residual_vocals: 0.0,
                per_bar: vec![],
                learned: None,
            };
            let title = |id: TrackId| lib.track(id).ok().flatten().map(|t| if t.title.is_empty() { t.display_name() } else { t.title }).unwrap_or_default();
            let name = opt_str(a, "name").map(str::to_string).unwrap_or_else(|| format!("{} × {}", title(v), title(i)));
            let mut mix = crate::mashup::mashup_mix(&name, &m, &analyses, 16).ok_or_else(|| anyhow!("could not build the mix"))?;
            mix.keylock = crate::prefs::Prefs::load().mix_keylock;
            save_and_view(&lib, &mut mix)
        }
        "export_mix" => export_mix(&lib, a, call),
        "export_virtualdj" => export_virtualdj(&lib, a),
        _ => bail!("unknown tool `{name}`"),
    }
}

fn search(lib: &Library, a: &Value) -> Result<Value> {
    let analyses = lib.all_analyses()?;
    let words: Vec<String> = opt_str(a, "query").unwrap_or_default().to_lowercase().split_whitespace().map(str::to_string).collect();
    let key = opt_str(a, "key").map(parse_key).transpose()?;
    let compat = opt_str(a, "compatible_with").map(parse_key).transpose()?;
    let (bmin, bmax) = (opt_f64(a, "bpm_min"), opt_f64(a, "bpm_max"));
    let (emin, emax) = (a.get("energy_min").and_then(|v| v.as_u64()), a.get("energy_max").and_then(|v| v.as_u64()));
    let stems = opt_bool(a, "has_stems");
    let mut hits: Vec<(Track, Option<&Analysis>)> = lib
        .tracks()?
        .into_iter()
        .map(|t| {
            let an = analyses.get(&t.id);
            (t, an)
        })
        .filter(|(t, an)| {
            let text = format!("{} {} {} {} {}", t.title, t.artist, t.album, t.genre, t.path.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default()).to_lowercase();
            let bpm = an.and_then(|a| a.bpm());
            let k = an.and_then(|a| a.key);
            let e = an.and_then(|a| a.energy).map(|e| e as u64);
            words.iter().all(|w| text.contains(w))
                && bmin.is_none_or(|m| bpm.is_some_and(|b| b >= m))
                && bmax.is_none_or(|m| bpm.is_some_and(|b| b <= m))
                && key.is_none_or(|q| k == Some(q))
                && compat.is_none_or(|q| k.is_some_and(|k| q.compatibility(k) >= 0.9))
                && emin.is_none_or(|m| e.is_some_and(|e| e >= m))
                && emax.is_none_or(|m| e.is_some_and(|e| e <= m))
                && stems.is_none_or(|s| an.is_some_and(|a| a.stems_model.is_some()) == s)
        })
        .collect();
    match opt_str(a, "sort").unwrap_or("title") {
        "artist" => hits.sort_by_key(|(t, _)| t.artist.to_lowercase()),
        "bpm" => hits.sort_by(|x, y| x.1.and_then(|a| a.bpm()).unwrap_or(0.0).total_cmp(&y.1.and_then(|a| a.bpm()).unwrap_or(0.0))),
        "key" => hits.sort_by_key(|(_, an)| an.and_then(|a| a.key).map(|k| (k.camelot_number(), !k.minor)).unwrap_or((99, false))),
        "energy" => hits.sort_by_key(|(_, an)| an.and_then(|a| a.energy).unwrap_or(0)),
        "added" => hits.sort_by_key(|(t, _)| std::cmp::Reverse(t.added_at)),
        _ => hits.sort_by_key(|(t, _)| t.display_name().to_lowercase()),
    }
    let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
    Ok(json!({
        "total": hits.len(),
        "tracks": hits.iter().take(limit).map(|(t, an)| track_brief(t, *an)).collect::<Vec<_>>(),
    }))
}

fn import(lib: &Library, a: &Value, call: &Call) -> Result<Value> {
    let paths: Vec<std::path::PathBuf> = a.get("paths").and_then(|v| v.as_array()).ok_or_else(|| anyhow!("missing `paths`"))?.iter().filter_map(|p| p.as_str()).map(std::path::PathBuf::from).collect();
    for p in &paths {
        if !p.exists() {
            bail!("{} does not exist", p.display());
        }
    }
    let analyze = opt_bool(a, "analyze").unwrap_or(true);
    let files = crate::importer::collect_audio(&paths);
    if files.is_empty() {
        bail!("no audio files found");
    }
    let (mut ids, mut errors) = (vec![], vec![]);
    let n = files.len() as f64;
    for (i, f) in files.iter().enumerate() {
        if call.cancelled() {
            break;
        }
        call.progress(i as f64, n, &format!("Importing {}", f.file_name().map(|s| s.to_string_lossy()).unwrap_or_default()));
        match crate::importer::import_file(lib, f) {
            Ok(id) => {
                let needs = lib.analysis(id)?.is_none_or(|a| a.version < Analysis::CURRENT_VERSION);
                if analyze && needs {
                    if let Err(e) = crate::pipeline::analyze_quick(lib, id) {
                        errors.push(format!("{}: analysis: {e:#}", f.display()));
                    }
                }
                ids.push(id);
            }
            Err(e) => errors.push(format!("{}: {e:#}", f.display())),
        }
        if i % 10 == 9 {
            notify_app();
        }
    }
    notify_app();
    let analyses = lib.all_analyses()?;
    let tracks: Vec<Value> = ids.iter().filter_map(|id| lib.track(*id).ok().flatten()).take(100).map(|t| track_brief(&t, analyses.get(&t.id))).collect();
    Ok(json!({"imported": ids.len(), "tracks": tracks, "errors": errors}))
}

fn separate(lib: &Library, a: &Value, call: &Call) -> Result<Value> {
    let ids = ids_arg(a, "track_ids")?;
    let mut s = job_settings();
    if let Some(q) = opt_str(a, "quality") {
        clubcraft_stems::Quality::parse(q).ok_or_else(|| anyhow!("unknown quality `{q}`"))?;
        s.quality = q.to_string();
    }
    let mut ctx = crate::pipeline::WorkerContext::default();
    let mut results = vec![];
    let n = ids.len() as f64;
    for (i, &id) in ids.iter().enumerate() {
        let t = lib.track(id)?.ok_or_else(|| anyhow!("no track with id {id}"))?;
        let start = Instant::now();
        call.progress(i as f64, n, &format!("{}: waiting for other stem jobs", t.display_name()));
        let _heavy = crate::jobs::heavy();
        let name = t.display_name();
        let r = crate::pipeline::separate(lib, &mut ctx, id, &s, &call.cancel, &mut |p, label| call.progress(i as f64 + p as f64 * 0.9, n, &format!("{name}: {label}")))
            .and_then(|_| crate::pipeline::analyze(lib, id, &s, &mut |p, label| call.progress(i as f64 + 0.9 + p as f64 * 0.1, n, &format!("{name}: {label}"))));
        drop(_heavy);
        notify_app();
        if call.cancelled() {
            bail!("cancelled");
        }
        let model = crate::pipeline::cached_stems_model(&t.hash);
        results.push(match r {
            Ok(()) => json!({"track_id": id, "title": name, "stems": model, "seconds": start.elapsed().as_secs()}),
            Err(e) => json!({"track_id": id, "title": name, "error": format!("{e:#}")}),
        });
    }
    ctx.release();
    Ok(json!({"results": results}))
}

fn export_mix(lib: &Library, a: &Value, call: &Call) -> Result<Value> {
    use crate::prefs::AudioFormat;
    let mix = load_mix(lib, arg_i64(a, "mix_id")?)?;
    if mix.tracks.is_empty() {
        bail!("the mix has no tracks");
    }
    let prefs = crate::prefs::Prefs::load();
    let format = match opt_str(a, "format") {
        Some("wav16") => AudioFormat::Wav16,
        Some("wav24") => AudioFormat::Wav24,
        Some("wav32f") => AudioFormat::Wav32Float,
        Some(f) => bail!("unknown format `{f}`"),
        None => prefs.export_format,
    };
    let out = match opt_str(a, "path") {
        Some(p) => std::path::PathBuf::from(p).with_extension("wav"),
        None => {
            let safe: String = mix.name.chars().map(|c| if c.is_alphanumeric() || " -_.()".contains(c) { c } else { '_' }).collect();
            clubcraft_core::paths::exports_dir().join(format!("{} {}.wav", safe.trim(), crate::platform::timestamp()))
        }
    };
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let all = lib.all_analyses()?;
    let analyses: HashMap<TrackId, Analysis> = mix.tracks.iter().filter_map(|t| all.get(&t.track_id).map(|a| (t.track_id, a.clone()))).collect();
    call.progress(0.0, 1.0, "Waiting for other heavy jobs");
    let _heavy = crate::jobs::heavy();
    let opts = clubcraft_engine::RenderOptions { format: format.wav(), tail_secs: prefs.export_tail_secs, ..Default::default() };
    let report = clubcraft_engine::render_mix(&mix, &|id| analyses.get(&id).cloned(), &|id| crate::live::adapter::load_uncached(id).ok(), &out, &opts, &mut |p| {
        call.progress(p as f64, 1.0, "Rendering");
        !call.cancelled()
    })?;
    if call.cancelled() {
        let _ = std::fs::remove_file(&out);
        bail!("cancelled");
    }
    if prefs.export_cue_sheet {
        crate::live::adapter::write_cue_sheet(&mix, &analyses, &out)?;
    }
    Ok(json!({
        "path": out,
        "duration_sec": r2(report.seconds),
        "peak_dbfs": r2(20.0 * (report.peak.max(1e-9) as f64).log10()),
        "format": format.label(),
        "realtime_factor": r2(report.speed),
    }))
}

fn find_mashups(lib: &Library, a: &Value, call: &Call) -> Result<Value> {
    let vocal = arg_i64(a, "vocal_track_id")?;
    let opts = clubcraft_analysis::mashup::MatchOptions {
        bars: a.get("bars").and_then(|v| v.as_u64()).unwrap_or(16).clamp(4, 64) as usize,
        key_range: a.get("key_range").and_then(|v| v.as_i64()).unwrap_or(2).clamp(0, 6) as i32,
        tempo_tolerance: opt_f64(a, "tempo_tolerance_pct").unwrap_or(6.0).clamp(0.0, 20.0) / 100.0,
        max_results: a.get("limit").and_then(|v| v.as_u64()).unwrap_or(15).clamp(1, 100) as usize,
        ..Default::default()
    };
    let q = crate::mashup::Query {
        vocal,
        vocal_bar: a.get("vocal_bar").and_then(|v| v.as_i64()).map(|b| b as i32 - 1),
        candidates: if a.get("instrumental_track_ids").is_some() { Some(ids_arg(a, "instrumental_track_ids")?) } else { None },
        opts,
        rerank: opt_bool(a, "rerank").unwrap_or(true),
    };
    let found = crate::mashup::run(&q, &call.cancel, &mut |f, l| call.progress(f as f64, 1.0, l))?;
    let analyses = lib.all_analyses()?;
    let title = |id: TrackId| lib.track(id).ok().flatten().map(|t| t.display_name()).unwrap_or_default();
    let matches: Vec<Value> = found
        .iter()
        .map(|m| {
            let (tv, ti) = crate::mashup::match_times(m, &analyses).unwrap_or((0.0, 0.0));
            json!({
                "instrumental_track_id": m.inst_track,
                "instrumental": title(m.inst_track),
                "instrumental_bar": m.inst_bar + 1,
                "instrumental_start_sec": r2(ti),
                "vocal_bar": m.vocal_bar + 1,
                "vocal_start_sec": r2(tv),
                "bars": m.bars,
                "key_shift": m.key_shift,
                "tempo_change_pct": r2((m.tempo_ratio - 1.0) * 100.0),
                "score": r2(m.score as f64),
                "harmony": r2(m.harmony as f64),
                "weakest_4_bars": r2(m.harmony_min as f64),
                "masking": r2(m.masking as f64),
                "leftover_vocals": r2(m.residual_vocals as f64),
                "ai_score": m.learned.map(|l| r2(l as f64)),
            })
        })
        .collect();
    Ok(json!({"vocal_track_id": vocal, "vocals": title(vocal), "matches": matches}))
}

fn virtualdj_running() -> bool {
    let probe = if cfg!(windows) {
        std::process::Command::new("tasklist").args(["/FI", "IMAGENAME eq virtualdj.exe", "/NH"]).output().map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains("virtualdj"))
    } else {
        std::process::Command::new("pgrep").args(["-i", "-x", "virtualdj"]).output().map(|o| o.status.success())
    };
    probe.unwrap_or(false)
}

fn export_virtualdj(lib: &Library, a: &Value) -> Result<Value> {
    let path = match opt_str(a, "path") {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let p = dirs::document_dir().context("no Documents folder")?.join("VirtualDJ").join("database.xml");
            if !p.exists() {
                bail!("{} not found; pass `path`", p.display());
            }
            p
        }
    };
    if virtualdj_running() {
        bail!("VirtualDJ is running; close it first, otherwise it overwrites database.xml when it quits");
    }
    let only: Option<Vec<TrackId>> = if a.get("track_ids").is_some() { Some(ids_arg(a, "track_ids")?) } else { None };
    let analyses = lib.all_analyses()?;
    let tracks: Vec<Track> = lib.tracks()?.into_iter().filter(|t| analyses.get(&t.id).is_some_and(|a| a.version > 0) && only.as_ref().is_none_or(|o| o.contains(&t.id))).collect();
    let mut cues: HashMap<TrackId, Vec<Cue>> = HashMap::new();
    for t in &tracks {
        let auto = analyses.get(&t.id).map(|a| a.cues.clone()).unwrap_or_default();
        cues.insert(t.id, clubcraft_core::effective_cues(&auto, &lib.cues(t.id)?));
    }
    let songs: Vec<crate::interop::virtualdj::VdjSong> = tracks.iter().map(|t| crate::interop::virtualdj::VdjSong { track: t, analysis: analyses.get(&t.id), cues: cues.remove(&t.id).unwrap_or_default() }).collect();
    let (updated, added) = crate::interop::virtualdj::export(&path, &songs)?;
    Ok(json!({"path": path, "updated": updated, "added": added, "backup": path.with_extension("xml.clubcraft-backup")}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_and_keys_parse() {
        for t in TransitionStyle::ALL.into_iter().filter(|t| *t != TransitionStyle::Custom) {
            assert_eq!(parse_style(&style_id(t)).unwrap(), t);
        }
        assert_eq!(parse_style("Bass swap").unwrap(), TransitionStyle::BassSwap);
        assert!(parse_style("custom").is_err());
        assert_eq!(parse_key("8A").unwrap(), parse_key("Am").unwrap());
    }

    #[test]
    fn tool_schemas_are_objects() {
        let t = tools();
        let names: Vec<&str> = t.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names.len(), 17);
        for tool in t.as_array().unwrap() {
            assert_eq!(tool["inputSchema"]["type"], "object");
            for r in tool["inputSchema"]["required"].as_array().unwrap() {
                assert!(tool["inputSchema"]["properties"].get(r.as_str().unwrap()).is_some(), "{}", tool["name"]);
            }
        }
    }
}
