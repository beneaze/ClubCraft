use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub const BG: Color32 = Color32::from_rgb(14, 15, 18);
pub const PANEL: Color32 = Color32::from_rgb(24, 26, 31);
pub const PANEL_HI: Color32 = Color32::from_rgb(34, 37, 44);
/// App header and its view switcher.
pub const HEADER: Color32 = Color32::from_rgb(19, 20, 24);
pub const HEADER_LINE: Color32 = Color32::from_rgb(38, 40, 47);
pub const SEG_TRACK: Color32 = Color32::from_rgb(29, 31, 37);
pub const SEG_HOVER: Color32 = Color32::from_rgb(38, 41, 48);
pub const SEG_ON: Color32 = Color32::from_rgb(56, 60, 70);
pub const SEG_ON_LINE: Color32 = Color32::from_rgb(72, 77, 89);
pub const LINE: Color32 = Color32::from_rgb(48, 52, 60);
pub const TEXT: Color32 = Color32::from_rgb(220, 224, 232);
pub const TEXT_DIM: Color32 = Color32::from_rgb(130, 136, 148);
/// Fill of selected tabs, toggles and list entries: solid graphite, so the pink accent is kept
/// for primary actions and indicators (a translucent accent fill read as a muddy plum).
pub const SELECT: Color32 = Color32::from_rgb(66, 71, 83);
pub const OK: Color32 = Color32::from_rgb(90, 220, 130);
pub const WARN: Color32 = Color32::from_rgb(255, 190, 60);
/// Groups of tracks linked by locked transitions (playlist), one colour per group in turn.
pub const LINK_GROUPS: [Color32; 4] = [Color32::from_rgb(130, 170, 255), Color32::from_rgb(195, 145, 255), Color32::from_rgb(80, 210, 195), Color32::from_rgb(255, 145, 190)];
/// Stem rows of a selected transition: the waveform over the stem's colour where it plays, the
/// waveform and the hatching where the transition turns it down.
pub const STEM_WAVE_ON: Color32 = Color32::from_black_alpha(175);
pub const STEM_WAVE_OFF: Color32 = Color32::from_rgba_premultiplied(70, 70, 70, 70);
pub const STEM_HATCH: Color32 = Color32::from_rgba_premultiplied(16, 16, 16, 16);

/// Per-deck accent colours (A, B, C, D).
pub const DECK: [Color32; 4] = [
    Color32::from_rgb(58, 160, 255),
    Color32::from_rgb(255, 120, 50),
    Color32::from_rgb(170, 110, 255),
    Color32::from_rgb(60, 210, 170),
];

static ACCENT_RGB: AtomicU32 = AtomicU32::new(0xECEEF2);
static SOFT_STEMS: AtomicBool = AtomicBool::new(true);

/// The interface accent (chosen in Settings › General).
pub fn accent() -> Color32 {
    let v = ACCENT_RGB.load(Ordering::Relaxed);
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub fn set_accent(c: [u8; 3]) {
    ACCENT_RGB.store((c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32, Ordering::Relaxed);
}

pub fn set_soft_stems(soft: bool) {
    SOFT_STEMS.store(soft, Ordering::Relaxed);
}

/// Stem colour in the palette chosen in Settings › Waveforms ("soft" is easier on the eyes over
/// long sessions, "vivid" separates the stems more strongly).
pub fn stem_color(k: clubcraft_core::StemKind) -> Color32 {
    use clubcraft_core::StemKind as S;
    if SOFT_STEMS.load(Ordering::Relaxed) {
        match k {
            S::Drums => Color32::from_rgb(226, 172, 92),
            S::Bass => Color32::from_rgb(210, 104, 112),
            S::Other => Color32::from_rgb(104, 170, 216),
            S::Vocals => Color32::from_rgb(136, 202, 126),
        }
    } else {
        rgb(k.color())
    }
}

/// Opaque blend of `a` towards `b` (`t` = 0 → a, 1 → b).
pub fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// Colour of a transition style (strip chips and overlap markers).
pub fn style_color(s: clubcraft_core::TransitionStyle) -> Color32 {
    use clubcraft_core::TransitionStyle as T;
    match s {
        T::StemBlend => Color32::from_rgb(70, 196, 130),
        T::BassSwap => Color32::from_rgb(222, 176, 60),
        T::Crossfade => Color32::from_rgb(132, 150, 178),
        T::VocalOver => Color32::from_rgb(64, 190, 204),
        T::FilterOut => Color32::from_rgb(96, 150, 232),
        T::EchoOut => Color32::from_rgb(232, 116, 100),
        T::Cut => Color32::from_rgb(176, 178, 186),
        T::Mashup => Color32::from_rgb(170, 212, 84),
        T::Custom => Color32::from_rgb(210, 200, 120),
    }
}

pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

pub fn deck_color(i: usize) -> Color32 {
    DECK[i % 4]
}

pub fn apply(ctx: &egui::Context) {
    let mut v = Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = BG;
    v.faint_bg_color = PANEL_HI;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = SELECT;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = accent();
    let r = CornerRadius::same(4);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.inactive.weak_bg_fill = PANEL_HI;
    v.widgets.inactive.bg_fill = PANEL_HI;
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(46, 50, 60);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(60, 64, 76);
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        // label text isn't selectable: hovering a track name showed a text cursor, and dragging it
        // selected text instead of dragging the track
        s.interaction.selectable_labels = false;
        s.spacing.item_spacing = egui::vec2(6.0, 5.0);
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
        s.text_styles.insert(TextStyle::Body, FontId::new(13.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Button, FontId::new(13.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Small, FontId::new(10.5, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Heading, FontId::new(18.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace));
    });
}

pub fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0);
    let m = (s / 60.0).floor();
    let r = s - m * 60.0;
    format!("{}:{:04.1}", m as i64, r)
}

pub fn fmt_time_short(secs: f64) -> String {
    let s = secs.max(0.0).round() as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Colour of a key on the Camelot wheel (hue by number, brighter for major).
pub fn key_color(k: clubcraft_core::Key) -> Color32 {
    let n = k.camelot_number() as f32;
    let hue = ((n - 1.0) / 12.0 + 0.95) % 1.0;
    let (r, g, b) = hsv(hue, if k.minor { 0.55 } else { 0.4 }, if k.minor { 0.95 } else { 1.0 });
    Color32::from_rgb(r, g, b)
}

pub fn hsv(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match (i as i32).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

static KEY_NOTATION: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_key_notation(n: crate::prefs::KeyNotation) {
    KEY_NOTATION.store(n as u8, std::sync::atomic::Ordering::Relaxed);
}

/// A key in the notation chosen in the preferences (Camelot, Open Key or musical).
pub fn key_label(k: clubcraft_core::Key) -> String {
    match KEY_NOTATION.load(std::sync::atomic::Ordering::Relaxed) {
        1 => k.open_key(),
        2 => k.name(),
        _ => k.camelot(),
    }
}
