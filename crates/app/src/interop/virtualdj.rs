//! VirtualDJ `database.xml` interop: export ClubCraft beat grids, keys, hot cues and automix
//! points; import VirtualDJ hot cues. Existing files are merged (unknown elements and attributes
//! are preserved) and backed up first. VirtualDJ must be closed while its database is written.

use anyhow::{Context, Result};
use clubcraft_core::{Analysis, BeatGrid, Cue, CueKind, CueSource, Key, Track};
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, Event};
use quick_xml::{Reader, Writer};
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};

pub struct VdjSong<'a> {
    pub track: &'a Track,
    pub analysis: Option<&'a Analysis>,
    /// Effective cues (auto + user).
    pub cues: Vec<Cue>,
}

fn f6(v: f64) -> String {
    format!("{v:.6}")
}

/// `<Song>` children we own (replaced on export).
fn owned_poi(e: &BytesStart) -> bool {
    let ty = attr(e, "Type");
    matches!(ty.as_deref(), None | Some("cue") | Some("beatgrid") | Some("automix"))
}

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.as_ref() == name.as_bytes()).and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

fn write_song_children(w: &mut Writer<Cursor<Vec<u8>>>, s: &VdjSong, include_tags: bool) -> Result<()> {
    let a = s.analysis;
    if include_tags {
        let mut tags = BytesStart::new("Tags");
        tags.push_attribute(("Author", s.track.artist.as_str()));
        tags.push_attribute(("Title", s.track.title.as_str()));
        if !s.track.genre.is_empty() {
            tags.push_attribute(("Genre", s.track.genre.as_str()));
        }
        if !s.track.album.is_empty() {
            tags.push_attribute(("Album", s.track.album.as_str()));
        }
        w.write_event(Event::Empty(tags))?;
        let mut infos = BytesStart::new("Infos");
        let len = a.map(|a| a.duration).filter(|d| *d > 0.0).unwrap_or(s.track.duration);
        infos.push_attribute(("SongLength", f6(len).as_str()));
        w.write_event(Event::Empty(infos))?;
    }
    if let Some(a) = a {
        if let Some(g) = a.grid {
            let mut scan = BytesStart::new("Scan");
            scan.push_attribute(("Version", "801"));
            scan.push_attribute(("Bpm", f6(60.0 / g.bpm).as_str()));
            if let Some(k) = a.key {
                scan.push_attribute(("Key", vdj_key(k).as_str()));
            }
            w.write_event(Event::Empty(scan))?;
            let mut p = BytesStart::new("Poi");
            p.push_attribute(("Pos", f6(g.downbeat).as_str()));
            p.push_attribute(("Type", "beatgrid"));
            w.write_event(Event::Empty(p))?;
        }
    }
    for c in &s.cues {
        let Some(slot) = c.slot else { continue };
        let mut p = BytesStart::new("Poi");
        p.push_attribute(("Name", c.name.as_str()));
        p.push_attribute(("Pos", f6(c.pos).as_str()));
        p.push_attribute(("Num", (slot as u32 + 1).to_string().as_str()));
        w.write_event(Event::Empty(p))?;
    }
    // Automix: ClubCraft's mix-in / mix-out cues.
    let find = |k: CueKind| s.cues.iter().find(|c| c.kind == k).map(|c| c.pos);
    for (point, pos) in [("realStart", find(CueKind::FirstBeat)), ("fadeStart", find(CueKind::MixOut)), ("fadeEnd", find(CueKind::MixOut).zip(a.and_then(|a| a.grid)).map(|(p, g)| p + 32.0 * g.beat_len()))] {
        if let Some(pos) = pos {
            let mut e = BytesStart::new("Poi");
            e.push_attribute(("Pos", f6(pos).as_str()));
            e.push_attribute(("Type", "automix"));
            e.push_attribute(("Point", point));
            w.write_event(Event::Empty(e))?;
        }
    }
    Ok(())
}

/// VirtualDJ writes keys in musical notation ("Am", "F#", "Ebm").
pub fn vdj_key(k: Key) -> String {
    k.name()
}

/// Merge `songs` into the database at `path` (created if missing). Returns (updated, added).
pub fn export(path: &Path, songs: &[VdjSong]) -> Result<(usize, usize)> {
    let by_path: HashMap<String, usize> = songs.iter().enumerate().map(|(i, s)| (s.track.path.to_string_lossy().to_string(), i)).collect();
    let mut done: HashSet<usize> = HashSet::new();
    let mut w = Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 1);
    let existing = std::fs::read(path).ok();
    let mut updated = 0;
    if let Some(bytes) = &existing {
        std::fs::write(path.with_extension("xml.clubcraft-backup"), bytes).context("backup database.xml")?;
        let mut r = Reader::from_reader(bytes.as_slice());
        r.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut current: Option<usize> = None;
        let mut skip_depth = 0usize;
        loop {
            let ev = r.read_event_into(&mut buf)?;
            match &ev {
                Event::Eof => break,
                Event::Start(e) if e.name().as_ref() == b"Song" => {
                    current = attr(e, "FilePath").and_then(|p| by_path.get(&p).copied());
                    w.write_event(ev.borrow())?;
                }
                Event::End(e) if e.name().as_ref() == b"Song" => {
                    if let Some(i) = current.take() {
                        write_song_children(&mut w, &songs[i], false)?;
                        done.insert(i);
                        updated += 1;
                    }
                    w.write_event(ev.borrow())?;
                }
                Event::End(e) if e.name().as_ref() == b"VirtualDJ_Database" => {
                    append_new(&mut w, songs, &done)?;
                    w.write_event(ev.borrow())?;
                }
                Event::Empty(e) if current.is_some() && ((e.name().as_ref() == b"Poi" && owned_poi(e)) || e.name().as_ref() == b"Scan") => {}
                Event::Start(e) if current.is_some() && e.name().as_ref() == b"Poi" && owned_poi(e) => skip_depth += 1,
                Event::End(e) if skip_depth > 0 && e.name().as_ref() == b"Poi" => skip_depth -= 1,
                _ if skip_depth > 0 => {}
                _ => w.write_event(ev.borrow())?,
            }
            buf.clear();
        }
    } else {
        w.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;
        let mut root = BytesStart::new("VirtualDJ_Database");
        root.push_attribute(("Version", "8.5"));
        w.write_event(Event::Start(root))?;
        append_new(&mut w, songs, &done)?;
        w.write_event(Event::End(BytesEnd::new("VirtualDJ_Database")))?;
    }
    let added = songs.len() - updated;
    let out = w.into_inner().into_inner();
    let tmp = path.with_extension("xml.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, path)?;
    Ok((updated, added))
}

fn append_new(w: &mut Writer<Cursor<Vec<u8>>>, songs: &[VdjSong], done: &HashSet<usize>) -> Result<()> {
    for (i, s) in songs.iter().enumerate() {
        if done.contains(&i) {
            continue;
        }
        let mut e = BytesStart::new("Song");
        let p = s.track.path.to_string_lossy();
        e.push_attribute(("FilePath", p.as_ref()));
        e.push_attribute(("FileSize", s.track.file_size.to_string().as_str()));
        w.write_event(Event::Start(e))?;
        write_song_children(w, s, true)?;
        w.write_event(Event::End(BytesEnd::new("Song")))?;
    }
    Ok(())
}

/// Read hot cues and beat grids from a VirtualDJ database: file path → (cues, grid).
pub fn import(path: &Path) -> Result<HashMap<PathBuf, (Vec<Cue>, Option<BeatGrid>)>> {
    let bytes = std::fs::read(path)?;
    let mut r = Reader::from_reader(bytes.as_slice());
    let mut out = HashMap::new();
    let mut buf = Vec::new();
    let mut cur: Option<(PathBuf, Vec<Cue>, Option<f64>, Option<f64>)> = None;
    loop {
        let ev = r.read_event_into(&mut buf)?;
        match &ev {
            Event::Eof => break,
            Event::Start(e) if e.name().as_ref() == b"Song" => {
                cur = attr(e, "FilePath").map(|p| (PathBuf::from(p), vec![], None, None));
            }
            Event::End(e) if e.name().as_ref() == b"Song" => {
                if let Some((p, cues, spb, anchor)) = cur.take() {
                    let grid = spb.filter(|s| *s > 0.0).map(|s| BeatGrid::new(60.0 / s, anchor.unwrap_or(0.0)));
                    out.insert(p, (cues, grid));
                }
            }
            Event::Empty(e) | Event::Start(e) if cur.is_some() => {
                let c = cur.as_mut().unwrap();
                match e.name().as_ref() {
                    b"Scan" => c.2 = attr(e, "Bpm").and_then(|b| b.parse().ok()),
                    b"Poi" => {
                        let pos: Option<f64> = attr(e, "Pos").and_then(|p| p.parse().ok());
                        match (attr(e, "Type").as_deref(), pos) {
                            (Some("beatgrid"), Some(p)) => c.3 = c.3.or(Some(p)),
                            (None | Some("cue"), Some(p)) => {
                                let num: i32 = attr(e, "Num").and_then(|n| n.parse().ok()).unwrap_or(-1);
                                c.1.push(Cue {
                                    slot: if (1..=8).contains(&num) { Some(num as u8 - 1) } else { None },
                                    kind: CueKind::Hot,
                                    pos: p,
                                    name: attr(e, "Name").unwrap_or_else(|| format!("Cue {num}")),
                                    color: [230, 230, 230],
                                    loop_beats: None,
                                    source: CueSource::Imported,
                                });
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clubcraft_core::*;

    #[test]
    fn export_merge_import() {
        let dir = std::env::temp_dir().join(format!("ccvdj{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("database.xml");
        std::fs::write(
            &db,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<VirtualDJ_Database Version="8.5">
 <Song FilePath="/m/a.mp3" FileSize="10">
  <Tags Author="X" Title="A" />
  <Scan Version="801" Bpm="0.5" Key="Am" />
  <Poi Pos="1.0" Type="beatgrid" />
  <Poi Name="old" Pos="3.0" Num="1" />
  <Poi Pos="9.0" Type="loop" Size="4" Num="1" />
 </Song>
 <Song FilePath="/m/other.mp3" FileSize="11"><Tags Author="Y" Title="O" /></Song>
</VirtualDJ_Database>"#,
        )
        .unwrap();
        let ta = Track { path: "/m/a.mp3".into(), title: "A".into(), ..Default::default() };
        let tb = Track { path: "/m/b.mp3".into(), title: "B".into(), ..Default::default() };
        let mut an = Analysis { grid: Some(BeatGrid::new(124.0, 0.25)), key: Key::parse("Fm"), duration: 200.0, ..Default::default() };
        an.version = 1;
        let cues = vec![Cue::auto(Some(0), CueKind::FirstBeat, 0.25), Cue::auto(Some(4), CueKind::Drop, 62.2)];
        let songs = vec![VdjSong { track: &ta, analysis: Some(&an), cues: cues.clone() }, VdjSong { track: &tb, analysis: Some(&an), cues }];
        let (u, a) = export(&db, &songs).unwrap();
        assert_eq!((u, a), (1, 1));
        let text = std::fs::read_to_string(&db).unwrap();
        assert!(text.contains("other.mp3"), "{text}");
        assert!(text.contains(r#"Type="loop""#), "loops preserved: {text}");
        assert!(!text.contains("old"), "old cue replaced: {text}");
        let back = import(&db).unwrap();
        let (c, g) = &back[&PathBuf::from("/m/a.mp3")];
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].slot, Some(4));
        assert!((g.unwrap().bpm - 124.0).abs() < 1e-3);
        assert!(back.contains_key(&PathBuf::from("/m/b.mp3")));
    }
}
