//! Peak-memory probe: `cargo run --release -p clubcraft-analysis --example mem_analyze -- <audio> <stems.flac> [rules]`
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // developer tool: failing loudly is fine
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let audio = clubcraft_audio::decode_file(a[1].as_ref(), 44100).unwrap();
    let st = clubcraft_stems::load_stem_track(a[2].as_ref()).unwrap();
    let owned = clubcraft_analysis::OwnedStems::from_stem_track_partial(&st);
    let mut o = clubcraft_analysis::AnalyzeOptions::default();
    if a.get(3).map(|s| s == "rules").unwrap_or(false) {
        o.structure = clubcraft_analysis::StructureSource::Rules;
    }
    let t = std::time::Instant::now();
    let an = clubcraft_analysis::analyze(&audio, Some(&owned.as_ref()), &o);
    println!("{:?} sections {} cues {} in {:?}", an.grid.map(|g| g.bpm), an.sections.len(), an.cues.len(), t.elapsed());
}
