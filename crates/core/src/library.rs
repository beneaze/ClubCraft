//! The ClubCraft music library: tracks, analysis results, user cues, playlists and mixes,
//! stored in a single SQLite database (`~/Music/ClubCraft/library.db`).

use crate::analysis::{Analysis, Cue, CueKind, CueSource, Waveform};
use crate::mix::Mix;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

pub type TrackId = i64;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub path: PathBuf,
    /// Content hash (stable across renames); used to name stem and waveform caches.
    pub hash: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub year: Option<i32>,
    pub duration: f64,
    pub bitrate: Option<u32>,
    pub file_size: u64,
    pub mtime: i64,
    pub added_at: i64,
    pub rating: u8,
    pub comment: String,
    /// BPM / key as found in the file tags (analysis results live in [`Analysis`]).
    pub tag_bpm: Option<f64>,
    pub tag_key: Option<String>,
    pub play_count: u32,
}

impl Track {
    pub fn display_name(&self) -> String {
        if self.artist.is_empty() {
            self.title.clone()
        } else {
            format!("{} - {}", self.artist, self.title)
        }
    }
}

/// Fast content hash: file size + first and last 256 KiB.
pub fn content_hash(path: &Path) -> Result<String> {
    use std::io::{Seek, SeekFrom};
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let len = f.metadata()?.len();
    let mut h = blake3::Hasher::new();
    h.update(&len.to_le_bytes());
    let mut buf = vec![0u8; 256 * 1024];
    let n = f.read(&mut buf)?;
    h.update(&buf[..n]);
    if len > 512 * 1024 {
        f.seek(SeekFrom::End(-(256 * 1024)))?;
        let n = f.read(&mut buf)?;
        h.update(&buf[..n]);
    }
    Ok(h.finalize().to_hex()[..32].to_string())
}

pub struct Library {
    conn: Connection,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tracks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    path TEXT NOT NULL UNIQUE,
    hash TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    artist TEXT NOT NULL DEFAULT '',
    album TEXT NOT NULL DEFAULT '',
    genre TEXT NOT NULL DEFAULT '',
    year INTEGER,
    duration REAL NOT NULL DEFAULT 0,
    bitrate INTEGER,
    file_size INTEGER NOT NULL DEFAULT 0,
    mtime INTEGER NOT NULL DEFAULT 0,
    added_at INTEGER NOT NULL DEFAULT 0,
    rating INTEGER NOT NULL DEFAULT 0,
    comment TEXT NOT NULL DEFAULT '',
    tag_bpm REAL,
    tag_key TEXT,
    play_count INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS tracks_hash ON tracks(hash);
CREATE TABLE IF NOT EXISTS analysis (
    track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    json TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS cues (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    slot INTEGER,
    kind TEXT NOT NULL,
    pos REAL NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    color INTEGER NOT NULL DEFAULT 0,
    loop_beats REAL,
    source TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS cues_track ON cues(track_id);
CREATE TABLE IF NOT EXISTS playlists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    sort INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS playlist_tracks (
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    sort INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS mixes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    modified_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS mix_savepoints (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    mix_id INTEGER NOT NULL REFERENCES mixes(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
"#;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn kind_str(k: CueKind) -> String {
    serde_json::to_value(k).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

fn parse_kind(s: &str) -> CueKind {
    serde_json::from_value(serde_json::Value::String(s.to_string())).unwrap_or(CueKind::Hot)
}

fn source_str(k: CueSource) -> &'static str {
    match k {
        CueSource::Auto => "Auto",
        CueSource::User => "User",
        CueSource::Imported => "Imported",
    }
}

fn color_int(c: [u8; 3]) -> i64 {
    ((c[0] as i64) << 16) | ((c[1] as i64) << 8) | c[2] as i64
}

fn int_color(v: i64) -> [u8; 3] {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

const TRACK_COLS: &str = "id, path, hash, title, artist, album, genre, year, duration, bitrate, file_size, mtime, \
     added_at, rating, comment, tag_bpm, tag_key, play_count";

fn row_track(r: &rusqlite::Row) -> rusqlite::Result<Track> {
    Ok(Track {
        id: r.get(0)?,
        path: PathBuf::from(r.get::<_, String>(1)?),
        hash: r.get(2)?,
        title: r.get(3)?,
        artist: r.get(4)?,
        album: r.get(5)?,
        genre: r.get(6)?,
        year: r.get(7)?,
        duration: r.get(8)?,
        bitrate: r.get(9)?,
        file_size: r.get::<_, i64>(10)? as u64,
        mtime: r.get(11)?,
        added_at: r.get(12)?,
        rating: r.get::<_, i64>(13)? as u8,
        comment: r.get(14)?,
        tag_bpm: r.get(15)?,
        tag_key: r.get(16)?,
        play_count: r.get::<_, i64>(17)? as u32,
    })
}

impl Library {
    pub fn open(path: &Path) -> Result<Library> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Library { conn })
    }

    pub fn open_default() -> Result<Library> {
        crate::paths::ensure_dirs()?;
        Library::open(&crate::paths::library_db())
    }

    pub fn in_memory() -> Result<Library> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Library { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    // ---------------------------------------------------------------- tracks

    /// Insert or update a track by path. Returns its id.
    pub fn upsert_track(&self, t: &Track) -> Result<TrackId> {
        let added = if t.added_at == 0 { now() } else { t.added_at };
        self.conn.execute(
            "INSERT INTO tracks (path, hash, title, artist, album, genre, year, duration, bitrate, file_size, mtime, \
             added_at, rating, comment, tag_bpm, tag_key, play_count) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17) \
             ON CONFLICT(path) DO UPDATE SET hash=excluded.hash, title=excluded.title, artist=excluded.artist, \
             album=excluded.album, genre=excluded.genre, year=excluded.year, duration=excluded.duration, \
             bitrate=excluded.bitrate, file_size=excluded.file_size, mtime=excluded.mtime, comment=excluded.comment, \
             tag_bpm=excluded.tag_bpm, tag_key=excluded.tag_key",
            params![
                t.path.to_string_lossy(),
                t.hash,
                t.title,
                t.artist,
                t.album,
                t.genre,
                t.year,
                t.duration,
                t.bitrate,
                t.file_size as i64,
                t.mtime,
                added,
                t.rating as i64,
                t.comment,
                t.tag_bpm,
                t.tag_key,
                t.play_count as i64
            ],
        )?;
        let id = self.conn.query_row(
            "SELECT id FROM tracks WHERE path = ?1",
            params![t.path.to_string_lossy()],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    pub fn track(&self, id: TrackId) -> Result<Option<Track>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {TRACK_COLS} FROM tracks WHERE id = ?1"), params![id], row_track)
            .optional()?)
    }

    pub fn track_by_path(&self, path: &Path) -> Result<Option<Track>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {TRACK_COLS} FROM tracks WHERE path = ?1"),
                params![path.to_string_lossy()],
                row_track,
            )
            .optional()?)
    }

    pub fn tracks(&self) -> Result<Vec<Track>> {
        let mut st = self.conn.prepare(&format!("SELECT {TRACK_COLS} FROM tracks ORDER BY artist, title"))?;
        let rows = st.query_map([], row_track)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn remove_track(&self, id: TrackId) -> Result<()> {
        self.conn.execute("DELETE FROM tracks WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn set_rating(&self, id: TrackId, rating: u8) -> Result<()> {
        self.conn.execute("UPDATE tracks SET rating = ?2 WHERE id = ?1", params![id, rating as i64])?;
        Ok(())
    }

    pub fn increment_play_count(&self, id: TrackId) -> Result<()> {
        self.conn.execute("UPDATE tracks SET play_count = play_count + 1 WHERE id = ?1", params![id])?;
        Ok(())
    }

    // -------------------------------------------------------------- analysis

    pub fn save_analysis(&self, id: TrackId, a: &Analysis) -> Result<()> {
        self.conn.execute(
            "INSERT INTO analysis (track_id, version, json, updated_at) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(track_id) DO UPDATE SET version=excluded.version, json=excluded.json, updated_at=excluded.updated_at",
            params![id, a.version, serde_json::to_string(a)?, now()],
        )?;
        Ok(())
    }

    pub fn analysis(&self, id: TrackId) -> Result<Option<Analysis>> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT json FROM analysis WHERE track_id = ?1", params![id], |r| r.get(0))
            .optional()?;
        Ok(match json {
            Some(j) => Some(serde_json::from_str(&j)?),
            None => None,
        })
    }

    /// All analyses keyed by track id (for list views).
    pub fn all_analyses(&self) -> Result<std::collections::HashMap<TrackId, Analysis>> {
        let mut st = self.conn.prepare("SELECT track_id, json FROM analysis")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = std::collections::HashMap::new();
        for r in rows {
            let (id, j) = r?;
            if let Ok(a) = serde_json::from_str(&j) {
                out.insert(id, a);
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------------ cues

    /// User and imported cues of a track (auto cues live inside [`Analysis`]).
    pub fn cues(&self, id: TrackId) -> Result<Vec<Cue>> {
        let mut st = self.conn.prepare(
            "SELECT slot, kind, pos, name, color, loop_beats, source FROM cues WHERE track_id = ?1 ORDER BY pos",
        )?;
        let rows = st.query_map(params![id], |r| {
            let source: String = r.get(6)?;
            Ok(Cue {
                slot: r.get::<_, Option<i64>>(0)?.map(|s| s as u8),
                kind: parse_kind(&r.get::<_, String>(1)?),
                pos: r.get(2)?,
                name: r.get(3)?,
                color: int_color(r.get(4)?),
                loop_beats: r.get(5)?,
                source: match source.as_str() {
                    "Imported" => CueSource::Imported,
                    "Auto" => CueSource::Auto,
                    _ => CueSource::User,
                },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replace all stored (user/imported) cues of a track.
    pub fn set_cues(&self, id: TrackId, cues: &[Cue]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM cues WHERE track_id = ?1", params![id])?;
        for c in cues {
            tx.execute(
                "INSERT INTO cues (track_id, slot, kind, pos, name, color, loop_beats, source) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    id,
                    c.slot.map(|s| s as i64),
                    kind_str(c.kind),
                    c.pos,
                    c.name,
                    color_int(c.color),
                    c.loop_beats,
                    source_str(c.source)
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // ------------------------------------------------------------- playlists

    pub fn create_playlist(&self, name: &str) -> Result<i64> {
        self.conn.execute("INSERT INTO playlists (name) VALUES (?1)", params![name])?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn playlists(&self) -> Result<Vec<(i64, String)>> {
        let mut st = self.conn.prepare("SELECT id, name FROM playlists ORDER BY sort, id")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn playlist_tracks(&self, playlist: i64) -> Result<Vec<TrackId>> {
        let mut st =
            self.conn.prepare("SELECT track_id FROM playlist_tracks WHERE playlist_id = ?1 ORDER BY sort")?;
        let rows = st.query_map(params![playlist], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_playlist_tracks(&self, playlist: i64, tracks: &[TrackId]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM playlist_tracks WHERE playlist_id = ?1", params![playlist])?;
        for (i, t) in tracks.iter().enumerate() {
            tx.execute(
                "INSERT INTO playlist_tracks (playlist_id, track_id, sort) VALUES (?1, ?2, ?3)",
                params![playlist, t, i as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn rename_playlist(&self, playlist: i64, name: &str) -> Result<()> {
        self.conn.execute("UPDATE playlists SET name = ?2 WHERE id = ?1", params![playlist, name])?;
        Ok(())
    }

    pub fn delete_playlist(&self, playlist: i64) -> Result<()> {
        self.conn.execute("DELETE FROM playlists WHERE id = ?1", params![playlist])?;
        Ok(())
    }

    // ----------------------------------------------------------------- mixes

    pub fn save_mix(&self, mix: &mut Mix) -> Result<i64> {
        let json = serde_json::to_string(mix)?;
        if mix.id > 0 {
            self.conn.execute(
                "UPDATE mixes SET name = ?2, json = ?3, modified_at = ?4 WHERE id = ?1",
                params![mix.id, mix.name, json, now()],
            )?;
        } else {
            self.conn.execute(
                "INSERT INTO mixes (name, json, created_at, modified_at) VALUES (?1, ?2, ?3, ?3)",
                params![mix.name, json, now()],
            )?;
            mix.id = self.conn.last_insert_rowid();
            // persist the id inside the JSON too
            self.conn.execute(
                "UPDATE mixes SET json = ?2 WHERE id = ?1",
                params![mix.id, serde_json::to_string(mix)?],
            )?;
        }
        Ok(mix.id)
    }

    pub fn mixes(&self) -> Result<Vec<(i64, String, i64)>> {
        let mut st = self.conn.prepare("SELECT id, name, modified_at FROM mixes ORDER BY modified_at DESC")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn mix(&self, id: i64) -> Result<Option<Mix>> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT json FROM mixes WHERE id = ?1", params![id], |r| r.get(0))
            .optional()?;
        Ok(match json {
            Some(j) => {
                let mut m: Mix = serde_json::from_str(&j)?;
                m.id = id;
                Some(m)
            }
            None => None,
        })
    }

    pub fn delete_mix(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM mix_savepoints WHERE mix_id = ?1", params![id])?;
        self.conn.execute("DELETE FROM mixes WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Copy mix `id` as a new mix named `name` (its savepoints stay with the original). Returns
    /// the new mix.
    pub fn duplicate_mix(&self, id: i64, name: &str) -> Result<Option<Mix>> {
        let Some(mut m) = self.mix(id)? else { return Ok(None) };
        m.id = 0;
        m.name = name.to_string();
        self.save_mix(&mut m)?;
        Ok(Some(m))
    }

    /// Keep the state `mix` of saved mix `mix_id` as a savepoint named `name`. Savepoints are
    /// never replaced: restoring one leaves all others, earlier and later, in place.
    pub fn add_savepoint(&self, mix_id: i64, name: &str, mix: &Mix) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO mix_savepoints (mix_id, name, json, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![mix_id, name, serde_json::to_string(mix)?, now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Savepoints of mix `mix_id`, oldest first: (id, name, created at).
    pub fn savepoints(&self, mix_id: i64) -> Result<Vec<(i64, String, i64)>> {
        let mut st = self.conn.prepare("SELECT id, name, created_at FROM mix_savepoints WHERE mix_id = ?1 ORDER BY created_at, id")?;
        let rows = st.query_map(params![mix_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The mix as it was at savepoint `id` (with its mix's id).
    pub fn savepoint(&self, id: i64) -> Result<Option<Mix>> {
        let row: Option<(i64, String)> = self
            .conn
            .query_row("SELECT mix_id, json FROM mix_savepoints WHERE id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        Ok(match row {
            Some((mix_id, j)) => {
                let mut m: Mix = serde_json::from_str(&j)?;
                m.id = mix_id;
                Some(m)
            }
            None => None,
        })
    }

    pub fn rename_savepoint(&self, id: i64, name: &str) -> Result<()> {
        self.conn.execute("UPDATE mix_savepoints SET name = ?2 WHERE id = ?1", params![id, name])?;
        Ok(())
    }

    pub fn delete_savepoint(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM mix_savepoints WHERE id = ?1", params![id])?;
        Ok(())
    }
}

// --------------------------------------------------------------- waveform cache

impl Waveform {
    const MAGIC: &'static [u8; 4] = b"CCWF";

    pub fn to_bytes(&self) -> Vec<u8> {
        let n = self.peak.len();
        let mut out = Vec::with_capacity(16 + n * 8);
        out.extend_from_slice(Self::MAGIC);
        out.extend_from_slice(&self.rate.to_le_bytes());
        out.extend_from_slice(&(n as u32).to_le_bytes());
        out.push(self.stems.is_some() as u8);
        for v in [&self.peak, &self.low, &self.mid, &self.high] {
            out.extend_from_slice(&v[..n.min(v.len())]);
            out.resize(out.len() + n.saturating_sub(v.len()), 0);
        }
        if let Some(st) = &self.stems {
            for v in st {
                out.extend_from_slice(&v[..n.min(v.len())]);
                out.resize(out.len() + n.saturating_sub(v.len()), 0);
            }
        }
        out
    }

    pub fn from_bytes(b: &[u8]) -> Option<Waveform> {
        if b.len() < 17 || &b[..4] != Self::MAGIC {
            return None;
        }
        let rate = f64::from_le_bytes(b[4..12].try_into().ok()?);
        let n = u32::from_le_bytes(b[12..16].try_into().ok()?) as usize;
        let has_stems = b[16] != 0;
        let mut off = 17;
        let take = |off: &mut usize| -> Option<Vec<u8>> {
            let v = b.get(*off..*off + n)?.to_vec();
            *off += n;
            Some(v)
        };
        let peak = take(&mut off)?;
        let low = take(&mut off)?;
        let mid = take(&mut off)?;
        let high = take(&mut off)?;
        let stems = if has_stems {
            Some([take(&mut off)?, take(&mut off)?, take(&mut off)?, take(&mut off)?])
        } else {
            None
        };
        Some(Waveform { rate, peak, low, mid, high, stems })
    }

    pub fn cache_path(hash: &str) -> PathBuf {
        crate::paths::waveform_dir().join(format!("{hash}.ccwf"))
    }

    pub fn save(&self, hash: &str) -> Result<()> {
        let p = Self::cache_path(hash);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(p, self.to_bytes())?;
        Ok(())
    }

    pub fn load(hash: &str) -> Option<Waveform> {
        std::fs::read(Self::cache_path(hash)).ok().and_then(|b| Waveform::from_bytes(&b))
    }
}

/// Path of the cached 8-channel stem file for a track hash.
pub fn stems_path(hash: &str) -> PathBuf {
    crate::paths::stems_dir().join(format!("{hash}.stems.flac"))
}

/// Path of the partial stem file for a track hash: stems for the parts of the track that mix
/// playback needs (its stem transitions), until complete stems replace them.
pub fn region_stems_path(hash: &str) -> PathBuf {
    crate::paths::stems_dir().join(format!("{hash}.regions.stems.flac"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::*;

    #[test]
    fn roundtrip() {
        let lib = Library::in_memory().unwrap();
        let t = Track { path: "/tmp/a.mp3".into(), hash: "h".into(), title: "A".into(), ..Default::default() };
        let id = lib.upsert_track(&t).unwrap();
        assert_eq!(lib.upsert_track(&t).unwrap(), id);
        let mut a = Analysis { version: 1, duration: 10.0, ..Default::default() };
        a.grid = Some(BeatGrid::new(124.0, 0.1));
        a.cues.push(Cue::auto(Some(0), CueKind::FirstBeat, 0.1));
        lib.save_analysis(id, &a).unwrap();
        assert_eq!(lib.analysis(id).unwrap().unwrap(), a);
        let mut c = Cue::auto(Some(2), CueKind::Drop, 60.0);
        c.source = CueSource::User;
        lib.set_cues(id, &[c.clone()]).unwrap();
        assert_eq!(lib.cues(id).unwrap(), vec![c]);
        let w = Waveform { rate: 100.0, peak: vec![1, 2], low: vec![3, 4], mid: vec![5, 6], high: vec![7, 8], stems: None };
        assert_eq!(Waveform::from_bytes(&w.to_bytes()).unwrap(), w);
    }

    #[test]
    fn savepoints_keep_every_state_and_duplicates_are_separate() {
        let lib = Library::in_memory().unwrap();
        let mut mix = crate::Mix { name: "Set".into(), ..Default::default() };
        lib.save_mix(&mut mix).unwrap();
        let a = lib.add_savepoint(mix.id, "A", &mix).unwrap();
        mix.keylock = !mix.keylock;
        let b = lib.add_savepoint(mix.id, "B", &mix).unwrap();
        // going back to A leaves B (the later state) in place
        let back = lib.savepoint(a).unwrap().unwrap();
        assert_eq!((back.id, back.keylock), (mix.id, !mix.keylock));
        assert_eq!(lib.savepoints(mix.id).unwrap().iter().map(|s| s.1.as_str()).collect::<Vec<_>>(), ["A", "B"]);
        assert_eq!(lib.savepoint(b).unwrap().unwrap().keylock, mix.keylock);
        lib.rename_savepoint(b, "B2").unwrap();
        lib.delete_savepoint(a).unwrap();
        assert_eq!(lib.savepoints(mix.id).unwrap().iter().map(|s| s.1.as_str()).collect::<Vec<_>>(), ["B2"]);
        // a duplicate is its own mix, without the savepoints
        let copy = lib.duplicate_mix(mix.id, "Set (copy)").unwrap().unwrap();
        assert_ne!(copy.id, mix.id);
        assert_eq!(lib.mix(copy.id).unwrap().unwrap().name, "Set (copy)");
        assert!(lib.savepoints(copy.id).unwrap().is_empty());
        lib.delete_mix(mix.id).unwrap();
        assert!(lib.savepoints(mix.id).unwrap().is_empty());
        assert!(lib.mix(copy.id).unwrap().is_some());
    }
}
