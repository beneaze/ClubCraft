use signalsmith_stretch::Stretch;
use std::time::Instant;
fn main() {
    let sr = 44100.0f32;
    for (name, block, interval) in [("default", 0.12f32, 0.03f32), ("mid", 0.09, 0.0225), ("cheaper", 0.1, 0.04), ("small", 0.07, 0.0175)] {
        let mut s = Stretch::new(2, (sr * block) as usize, (sr * interval) as usize);
        s.set_transpose_factor(1.0, None);
        let n = 256usize;
        let secs = 10.0;
        let blocks = (secs * sr) as usize / n;
        let input: Vec<f32> = (0..(n * 2 * 2)).map(|i| ((i as f32) * 0.01).sin() * 0.3).collect();
        let mut out = vec![0f32; n * 2];
        let t = Instant::now();
        for b in 0..blocks {
            let m = if b % 13 == 0 { 277 } else { 276 }; // ~1.08x
            s.process(&input[..m * 2], &mut out[..]);
        }
        let el = t.elapsed().as_secs_f64();
        println!("{name}: in_lat {} out_lat {} -> {:.2}% of realtime (10 s in {:.3} s)", s.input_latency(), s.output_latency(), el / secs as f64 * 100.0, el);
    }
}
