//! Energy rating 1..=10 (Mixed-In-Key style scale) from loudness, groove density and
//! drum / bass presence.

use crate::structure::BarFeatures;
use clubcraft_core::{Section, SectionKind};

/// Rating from loudness only (used when no beat grid could be found).
pub fn rating_simple(lufs: f32) -> u8 {
    let loud = ((lufs + 16.0) / 10.0).clamp(0.0, 1.0);
    (1.0 + 8.0 * loud).round().clamp(1.0, 10.0) as u8
}

/// Full rating. `lufs` = integrated loudness, `bars` = bar features, `bpm` = tempo.
pub fn rating(lufs: Option<f32>, bars: &BarFeatures, sections: &[Section], bpm: f64) -> u8 {
    rating_from(lufs, bars, sections, bpm)
}

pub(crate) fn rating_from(lufs: Option<f32>, bars: &BarFeatures, sections: &[Section], bpm: f64) -> u8 {
    let loud = lufs.map(|l| ((l + 14.0) / 8.0).clamp(0.0, 1.0)).unwrap_or(0.5);
    let n = bars.len().max(1) as f32;
    let groove = bars.stems.iter().filter(|s| s[0] > 0.5 && s[1] > 0.5).count() as f32 / n;
    let drop_frac = sections.iter().filter(|s| s.kind == SectionKind::Drop).map(|s| s.bars as f32).sum::<f32>() / n;
    let tempo = ((bpm as f32 - 112.0) / 24.0).clamp(0.0, 1.0);
    let score = 0.4 * loud + 0.25 * groove + 0.1 * drop_frac + 0.25 * tempo;
    (1.0 + 9.0 * score).round().clamp(1.0, 10.0) as u8
}
