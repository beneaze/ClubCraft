use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::{Accessor, ItemKey};
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub year: Option<i32>,
    pub comment: String,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub duration: f64,
    pub bitrate: Option<u32>,
    pub artwork: Option<Vec<u8>>,
}

/// Read tags and properties. Missing titles fall back to the file name ("Artist - Title.mp3" is split).
pub fn read_metadata(path: &Path) -> Metadata {
    let mut m = Metadata::default();
    if let Ok(tf) = lofty::read_from_path(path) {
        let props = tf.properties();
        m.duration = props.duration().as_secs_f64();
        m.bitrate = props.audio_bitrate();
        if let Some(tag) = tf.primary_tag().or_else(|| tf.first_tag()) {
            m.title = tag.title().map(|s| s.to_string()).unwrap_or_default();
            m.artist = tag.artist().map(|s| s.to_string()).unwrap_or_default();
            m.album = tag.album().map(|s| s.to_string()).unwrap_or_default();
            m.genre = tag.genre().map(|s| s.to_string()).unwrap_or_default();
            m.comment = tag.comment().map(|s| s.to_string()).unwrap_or_default();
            m.year = tag.date().map(|d| d.year as i32);
            m.bpm = tag
                .get_string(ItemKey::Bpm)
                .or_else(|| tag.get_string(ItemKey::IntegerBpm))
                .and_then(|s| s.trim().parse::<f64>().ok())
                .filter(|b| *b > 20.0 && *b < 400.0);
            m.key = tag.get_string(ItemKey::InitialKey).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            m.artwork = tag.pictures().first().map(|p| p.data().to_vec());
        }
    }
    if m.title.is_empty() {
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if let Some((a, t)) = stem.split_once(" - ") {
            if m.artist.is_empty() {
                m.artist = a.trim().to_string();
            }
            m.title = t.trim().to_string();
        } else {
            m.title = stem;
        }
    }
    m
}
