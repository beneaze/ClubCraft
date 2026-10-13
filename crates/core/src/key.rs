use serde::{Deserialize, Serialize};
use std::fmt;

const NAMES: [&str; 12] = ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];

/// A musical key. `tonic` is a pitch class (0 = C .. 11 = B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Key {
    pub tonic: u8,
    pub minor: bool,
}

impl Key {
    pub fn new(tonic: u8, minor: bool) -> Self {
        Key { tonic: tonic % 12, minor }
    }

    /// Camelot wheel number (1..=12). Minor keys are "A", major keys are "B".
    pub fn camelot_number(self) -> u8 {
        // Each step of a fifth (+7 semitones) moves one position on the wheel.
        // 8B = C major, 5A = C minor.
        let base = if self.minor { 5 } else { 8 };
        let fifths = (self.tonic as u32 * 7) % 12; // position of tonic on circle of fifths from C
        ((base - 1 + fifths) % 12 + 1) as u8
    }

    pub fn camelot(self) -> String {
        format!("{}{}", self.camelot_number(), if self.minor { 'A' } else { 'B' })
    }

    /// Open Key notation (1m..12m / 1d..12d). C major = 1d, A minor = 1m.
    pub fn open_key(self) -> String {
        let n = (self.camelot_number() + 4) % 12 + 1;
        format!("{}{}", n, if self.minor { 'm' } else { 'd' })
    }

    pub fn name(self) -> String {
        format!("{}{}", NAMES[self.tonic as usize], if self.minor { "m" } else { "" })
    }

    pub fn from_camelot(s: &str) -> Option<Key> {
        let s = s.trim();
        let (num, letter) = s.split_at(s.len().checked_sub(1)?);
        let n: u32 = num.parse().ok()?;
        if !(1..=12).contains(&n) {
            return None;
        }
        let minor = match letter {
            "A" | "a" => true,
            "B" | "b" => false,
            _ => return None,
        };
        (0..12u8).map(|t| Key::new(t, minor)).find(|k| k.camelot_number() as u32 == n)
    }

    /// Parse "Am", "F#", "Bbm", "C# minor", "8A", "1d" ...
    pub fn parse(s: &str) -> Option<Key> {
        if let Some(k) = Key::from_camelot(s) {
            return Some(k);
        }
        let t = s.trim();
        let lower = t.to_ascii_lowercase();
        if let Some(rest) = lower.strip_suffix('m').or_else(|| lower.strip_suffix('d')) {
            if let Ok(n) = rest.parse::<u8>() {
                if (1..=12).contains(&n) {
                    let camelot = (n + 6) % 12 + 1;
                    let minor = lower.ends_with('m');
                    return Key::from_camelot(&format!("{}{}", camelot, if minor { 'A' } else { 'B' }));
                }
            }
        }
        let mut chars = t.chars();
        let letter = chars.next()?.to_ascii_uppercase();
        let mut tonic: i32 = match letter {
            'C' => 0,
            'D' => 2,
            'E' => 4,
            'F' => 5,
            'G' => 7,
            'A' => 9,
            'B' => 11,
            _ => return None,
        };
        let rest: String = chars.collect();
        let mut rest = rest.as_str();
        if let Some(r) = rest.strip_prefix('#').or_else(|| rest.strip_prefix('♯')) {
            tonic += 1;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('b').or_else(|| rest.strip_prefix('♭')) {
            tonic -= 1;
            rest = r;
        }
        let rest = rest.trim().to_ascii_lowercase();
        let minor = rest.starts_with('m') && !rest.starts_with("maj");
        Some(Key::new(tonic.rem_euclid(12) as u8, minor))
    }

    /// Transpose by semitones.
    pub fn shifted(self, semitones: i32) -> Key {
        Key::new((self.tonic as i32 + semitones).rem_euclid(12) as u8, self.minor)
    }

    /// Harmonic compatibility score in 0..=1 following Camelot rules:
    /// same key 1.0, relative major/minor and ±1 on the wheel 0.9, "energy boost" (+2) / diagonal 0.6.
    pub fn compatibility(self, other: Key) -> f32 {
        let a = self.camelot_number() as i32;
        let b = other.camelot_number() as i32;
        let d = (b - a).rem_euclid(12);
        let dist = d.min(12 - d);
        let same_mode = self.minor == other.minor;
        if same_mode && (d == 7 || d == 2) {
            // +1 semitone / +2 semitones "energy boost" modulations
            return 0.6;
        }
        match (same_mode, dist) {
            (true, 0) => 1.0,
            (false, 0) => 0.9,
            (true, 1) => 0.9,
            (false, 1) => 0.5,
            (true, 2) => 0.4,
            _ => 0.0,
        }
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.camelot())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn camelot() {
        assert_eq!(Key::new(0, false).camelot(), "8B");
        assert_eq!(Key::new(9, true).camelot(), "8A");
        assert_eq!(Key::new(0, true).camelot(), "5A");
        assert_eq!(Key::new(7, false).camelot(), "9B");
        assert_eq!(Key::new(6, false).camelot(), "2B");
        assert_eq!(Key::new(8, true).camelot(), "1A");
        assert_eq!(Key::new(0, false).open_key(), "1d");
        assert_eq!(Key::new(9, true).open_key(), "1m");
        for t in 0..12 {
            for m in [false, true] {
                let k = Key::new(t, m);
                assert_eq!(Key::from_camelot(&k.camelot()), Some(k));
                assert_eq!(Key::parse(&k.name()), Some(k));
                assert_eq!(Key::parse(&k.open_key()), Some(k));
            }
        }
        assert_eq!(Key::parse("F# minor"), Some(Key::new(6, true)));
        assert_eq!(Key::parse("Bb"), Some(Key::new(10, false)));
    }
}
