//! Adding audio files to the library (hash + tags), run on a background thread.

use anyhow::Result;
use clubcraft_core::library::content_hash;
use clubcraft_core::{Library, Track, TrackId};
use std::path::{Path, PathBuf};

pub const AUDIO_EXTS: &[&str] = &["mp3", "m4a", "aac", "mp4", "flac", "wav", "aif", "aiff", "ogg", "opus", "alac", "caf"];

pub fn is_audio(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).map(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str())).unwrap_or(false)
}

/// Expand files and folders (recursively) into audio file paths.
pub fn collect_audio(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            for e in walkdir::WalkDir::new(p).follow_links(true).into_iter().flatten() {
                if e.file_type().is_file() && is_audio(e.path()) && !e.file_name().to_string_lossy().starts_with("._") {
                    out.push(e.path().to_path_buf());
                }
            }
        } else if is_audio(p) {
            out.push(p.clone());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Add (or refresh) one file. Returns its id.
pub fn import_file(lib: &Library, path: &Path) -> Result<TrackId> {
    let md = std::fs::metadata(path)?;
    let mtime = md.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
    if let Some(t) = lib.track_by_path(path)? {
        if t.mtime == mtime && t.file_size == md.len() {
            return Ok(t.id);
        }
    }
    let meta = clubcraft_audio::read_metadata(path);
    let t = Track {
        path: path.to_path_buf(),
        hash: content_hash(path)?,
        title: meta.title,
        artist: meta.artist,
        album: meta.album,
        genre: meta.genre,
        year: meta.year,
        duration: meta.duration,
        bitrate: meta.bitrate,
        file_size: md.len(),
        mtime,
        comment: meta.comment,
        tag_bpm: meta.bpm,
        tag_key: meta.key,
        ..Default::default()
    };
    lib.upsert_track(&t)
}
