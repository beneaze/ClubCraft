use std::time::Instant;
fn main() -> anyhow::Result<()> {
    let p = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    for _ in 0..3 {
        let t = Instant::now();
        let tr = clubcraft_stems::load_stem_track(&p)?;
        let a = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let st = clubcraft_stems::load_stems(&p)?;
        println!("load_stem_track {a:.3}s ({} frames), load_stems {:.3}s", tr.frames, t.elapsed().as_secs_f64());
        drop(st);
    }
    Ok(())
}
