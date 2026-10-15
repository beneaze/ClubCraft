#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // developer tool: failing loudly is fine
fn main() {
    let p = std::env::args().nth(1).unwrap();
    let t = std::time::Instant::now();
    let b = clubcraft_audio::decode_file(std::path::Path::new(&p), 44100).unwrap();
    println!("{} frames, {:.1}s, decoded in {:?}", b.frames(), b.duration(), t.elapsed());
    let m = clubcraft_audio::read_metadata(std::path::Path::new(&p));
    println!("{:?} / {:?} bpm={:?} key={:?} art={}", m.artist, m.title, m.bpm, m.key, m.artwork.map(|a| a.len()).unwrap_or(0));
}
