//! Settings window: audio output, headphone routing, MIDI controllers (with learn mode) and
//! library maintenance.

use crate::prefs::*;
use crate::theme;
use clubcraft_core::TransitionStyle;
use clubcraft_engine::midi::{self, MidiConnection, MidiMapper, MidiMapping};
use clubcraft_engine::{AudioConfig, DeviceInfo, EngineHandle, EngineState, HeadphoneRouting};
use eframe::egui::{self, RichText, Ui};
use parking_lot::Mutex;
use std::sync::Arc;

pub fn mapping_path() -> std::path::PathBuf {
    clubcraft_core::paths::data_dir().join("midi-mapping.json")
}

/// Learnable controls: (label, action, per-deck).
const LEARN: &[(&str, &str, bool)] = &[
    ("Play / pause", "play_pause", true),
    ("Cue", "cue", true),
    ("Sync", "sync", true),
    ("Hot cue 1", "hot_cue 1", true),
    ("Hot cue 2", "hot_cue 2", true),
    ("Hot cue 3", "hot_cue 3", true),
    ("Hot cue 4", "hot_cue 4", true),
    ("Hot cue 5", "hot_cue 5", true),
    ("Hot cue 6", "hot_cue 6", true),
    ("Hot cue 7", "hot_cue 7", true),
    ("Hot cue 8", "hot_cue 8", true),
    ("Pitch fader", "pitch", true),
    ("Jog wheel", "jog", true),
    ("Jog touch", "jog_touch", true),
    ("Keylock", "keylock", true),
    ("Auto loop 4", "loop 4", true),
    ("Loop halve", "loop_half", true),
    ("Loop double", "loop_double", true),
    ("Beat jump −4", "beatjump -4", true),
    ("Beat jump +4", "beatjump 4", true),
    ("Vocals mute", "stem vocal mute", true),
    ("Melody mute", "stem melody mute", true),
    ("Bass mute", "stem bass mute", true),
    ("Drums mute", "stem drums mute", true),
    ("Acapella", "acapella", true),
    ("Instrumental", "instrumental", true),
    ("EQ high", "eq_high", true),
    ("EQ mid", "eq_mid", true),
    ("EQ low", "eq_low", true),
    ("Filter", "filter", true),
    ("Gain", "gain", true),
    ("Channel fader", "volume", true),
    ("Headphone cue", "pfl", true),
    ("FX 1 on", "fx 1", true),
    ("Crossfader", "crossfader", false),
    ("Master volume", "master_volume", false),
    ("Headphone mix", "headphone_mix", false),
    ("Headphone volume", "headphone_volume", false),
];

pub struct Settings {
    pub open: bool,
    pub audio: AudioConfig,
    devices: Vec<DeviceInfo>,
    midi_ports: Vec<String>,
    pub midi_port: Option<String>,
    pub mapper: Arc<Mutex<MidiMapper>>,
    connection: Option<MidiConnection>,
    learn_deck: usize,
    pub message: String,
    pub tab: Tab,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    General,
    Decks,
    Mixer,
    Audio,
    Waveforms,
    Analysis,
    Studio,
    Export,
    Controllers,
}

impl Tab {
    const ALL: [Tab; 9] = [Tab::General, Tab::Decks, Tab::Mixer, Tab::Audio, Tab::Waveforms, Tab::Analysis, Tab::Studio, Tab::Export, Tab::Controllers];
    fn label(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Decks => "Decks",
            Tab::Mixer => "Mixer",
            Tab::Audio => "Audio output",
            Tab::Waveforms => "Waveforms",
            Tab::Analysis => "Analysis & stems",
            Tab::Studio => "Studio",
            Tab::Export => "Export & recording",
            Tab::Controllers => "Controllers (MIDI)",
        }
    }
}

impl Settings {
    pub fn new(audio: AudioConfig, midi_port: Option<String>) -> Settings {
        let mapping = MidiMapping::load(&mapping_path()).unwrap_or_else(|_| MidiMapping { name: "My controller".into(), device: None, bindings: vec![] });
        let mapper = MidiMapper::new(mapping).unwrap_or_else(|_| MidiMapper::new(MidiMapping::default()).unwrap());
        Settings {
            open: false,
            audio,
            devices: vec![],
            midi_ports: vec![],
            midi_port,
            mapper: Arc::new(Mutex::new(mapper)),
            connection: None,
            learn_deck: 0,
            message: String::new(),
            tab: Tab::General,
        }
    }

    pub fn connect_midi(&mut self, handle: Option<&EngineHandle>) {
        self.connection = None;
        let (Some(port), Some(h)) = (self.midi_port.clone(), handle) else { return };
        match midi::connect(&port, self.mapper.clone(), h.sender()) {
            Ok(c) => {
                self.connection = Some(c);
                self.message = format!("MIDI: connected to {port}");
            }
            Err(e) => self.message = format!("MIDI: {e:#}"),
        }
    }

    pub fn midi_connected(&self) -> bool {
        self.connection.is_some()
    }

    /// Per-frame: feed engine state to the MIDI mapper (toggle feedback) and save learned bindings.
    pub fn tick(&mut self, st: &EngineState) {
        if self.connection.is_none() {
            return;
        }
        let mut m = self.mapper.lock();
        m.update_state(st);
        if m.take_learned().is_some() {
            let _ = m.mapping.save(&mapping_path());
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, handle: Option<&EngineHandle>, actions: &mut Vec<SettingsAction>, prefs: &mut Prefs) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new("Settings")
            .open(&mut open)
            .default_size([760.0, 560.0])
            .min_width(640.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(150.0);
                        for t in Tab::ALL {
                            if ui.add(egui::Button::new(RichText::new(t.label()).size(13.0)).selected(self.tab == t).min_size(egui::vec2(146.0, 26.0))).clicked() {
                                self.tab = t;
                            }
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        egui::ScrollArea::vertical().id_salt("settings-body").show(ui, |ui| {
                            ui.set_min_width(520.0);
                            match self.tab {
                                Tab::General => general_ui(ui, prefs),
                                Tab::Decks => decks_ui(ui, prefs),
                                Tab::Mixer => mixer_ui(ui, prefs),
                                Tab::Audio => self.audio_ui(ui, handle),
                                Tab::Waveforms => waveforms_ui(ui, prefs),
                                Tab::Analysis => analysis_ui(ui, prefs, actions),
                                Tab::Studio => studio_ui(ui, prefs),
                                Tab::Export => export_ui(ui, prefs),
                                Tab::Controllers => self.midi_ui(ui, handle),
                            }
                            if !self.message.is_empty() {
                                ui.add_space(8.0);
                                ui.label(RichText::new(&self.message).color(theme::TEXT_DIM));
                            }
                        });
                    });
                });
            });
        self.open = open;
    }

    fn audio_ui(&mut self, ui: &mut egui::Ui, handle: Option<&EngineHandle>) {
        section(ui, "Audio output");
        if self.devices.is_empty() {
            self.devices = clubcraft_engine::devices().unwrap_or_default();
        }
        let mut cfg = self.audio.clone();
        egui::Grid::new("audio").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Device");
            egui::ComboBox::from_id_salt("dev").width(300.0).selected_text(cfg.device.clone().unwrap_or("System default".into())).show_ui(ui, |ui| {
                ui.selectable_value(&mut cfg.device, None, "System default");
                for d in &self.devices {
                    ui.selectable_value(&mut cfg.device, Some(d.name.clone()), format!("{} ({} ch)", d.name, d.max_channels));
                }
            });
            if ui.small_button("↻").on_hover_text("Rescan devices").clicked() {
                self.devices = clubcraft_engine::devices().unwrap_or_default();
            }
            ui.end_row();
            ui.label("Sample rate");
            egui::ComboBox::from_id_salt("sr").selected_text(cfg.sample_rate.map(|r| format!("{r} Hz")).unwrap_or("Auto".into())).show_ui(ui, |ui| {
                ui.selectable_value(&mut cfg.sample_rate, None, "Auto");
                ui.selectable_value(&mut cfg.sample_rate, Some(44100), "44100 Hz");
                ui.selectable_value(&mut cfg.sample_rate, Some(48000), "48000 Hz");
            });
            ui.end_row();
            ui.label("Buffer");
            egui::ComboBox::from_id_salt("buf").selected_text(cfg.buffer_frames.map(|b| format!("{b} frames")).unwrap_or("Device default".into())).show_ui(ui, |ui| {
                for b in [128u32, 256, 512, 1024] {
                    ui.selectable_value(&mut cfg.buffer_frames, Some(b), format!("{b} frames ({:.1} ms)", b as f32 / 44.1));
                }
                ui.selectable_value(&mut cfg.buffer_frames, None, "Device default");
            });
            ui.end_row();
            ui.label("Headphones");
            let label = match cfg.headphones {
                HeadphoneRouting::Auto => "Auto (outputs 3/4 if available)".to_string(),
                HeadphoneRouting::Off => "Off".into(),
                HeadphoneRouting::Split => "Split cable (master L / cue R)".into(),
                HeadphoneRouting::Channels { master, cue } => format!("Master {}/{} · Cue {}/{}", master[0] + 1, master[1] + 1, cue[0] + 1, cue[1] + 1),
            };
            egui::ComboBox::from_id_salt("hp").width(300.0).selected_text(label).show_ui(ui, |ui| {
                ui.selectable_value(&mut cfg.headphones, HeadphoneRouting::Auto, "Auto (outputs 3/4 if available)");
                ui.selectable_value(&mut cfg.headphones, HeadphoneRouting::Off, "Off");
                ui.selectable_value(&mut cfg.headphones, HeadphoneRouting::Split, "Split cable (master L / cue R)");
                ui.selectable_value(&mut cfg.headphones, HeadphoneRouting::Channels { master: [0, 1], cue: [2, 3] }, "Master 1/2 · Cue 3/4");
                ui.selectable_value(&mut cfg.headphones, HeadphoneRouting::Channels { master: [2, 3], cue: [0, 1] }, "Master 3/4 · Cue 1/2");
            });
            ui.end_row();
        });
        if let Some(h) = handle {
            let i = h.stream_info();
            ui.label(RichText::new(format!("Running: {} · {} Hz · {} ch · buffer {}{}", i.device, i.sample_rate, i.channels, i.buffer_frames.map(|b| b.to_string()).unwrap_or("default".into()), if i.cue_channels.is_some() { " · headphone cue on" } else { "" })).size(11.0).color(theme::TEXT_DIM));
            let a = h.state().audio;
            let txt = format!("Audio callback: {} frames · peak load {:.0} % · dropouts {}", a.buffer_frames, a.peak_load * 100.0, a.overruns + a.late);
            let col = if a.overruns + a.late > 0 { theme::WARN } else { theme::TEXT_DIM };
            ui.label(RichText::new(txt).size(11.0).color(col)).on_hover_text("Dropouts = callbacks that took longer than their buffer or started late. If they keep rising while playing, pick a larger buffer.");
        }
        if cfg != self.audio {
            self.audio = cfg.clone();
            if let Some(h) = handle {
                match h.set_audio_config(cfg) {
                    Ok(()) => self.message = "Audio output changed".into(),
                    Err(e) => self.message = format!("Audio: {e:#}"),
                }
            }
        }
    }

    fn midi_ui(&mut self, ui: &mut egui::Ui, handle: Option<&EngineHandle>) {
        section(ui, "MIDI controller");
        ui.horizontal(|ui| {
            if self.midi_ports.is_empty() {
                self.midi_ports = midi::input_ports().unwrap_or_default();
            }
            let before = self.midi_port.clone();
            egui::ComboBox::from_id_salt("midiport").width(260.0).selected_text(self.midi_port.clone().unwrap_or("None".into())).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.midi_port, None, "None");
                for p in &self.midi_ports {
                    ui.selectable_value(&mut self.midi_port, Some(p.clone()), p);
                }
            });
            if ui.small_button("↻").clicked() {
                self.midi_ports = midi::input_ports().unwrap_or_default();
            }
            if before != self.midi_port {
                self.connect_midi(handle);
            }
            ui.label(if self.midi_connected() { RichText::new("connected").color(theme::OK) } else { RichText::new("not connected").color(theme::TEXT_DIM) });
        });
        if !self.midi_connected() {
            ui.label(RichText::new("Connect a controller to map its buttons and knobs (MIDI learn).").color(theme::TEXT_DIM));
            return;
        }
        ui.horizontal(|ui| {
            ui.label("Learn for deck");
            for (i, n) in ["A", "B", "C", "D"].iter().enumerate() {
                ui.selectable_value(&mut self.learn_deck, i, *n);
            }
            let mut m = self.mapper.lock();
            if m.is_learning() {
                ui.label(RichText::new("Move a control on your controller…").color(theme::WARN));
                if ui.button("Cancel").clicked() {
                    m.cancel_learn();
                }
            }
            if ui.button("Clear mapping").clicked() {
                m.mapping.bindings.clear();
                let _ = m.mapping.save(&mapping_path());
                if let Ok(nm) = MidiMapper::new(m.mapping.clone()) {
                    *m = nm;
                }
            }
        });
        let deck = self.learn_deck;
        egui::Grid::new("learn").num_columns(3).striped(true).show(ui, |ui| {
            for (label, action, per_deck) in LEARN {
                let d = if *per_deck { Some(deck) } else { None };
                ui.label(*label);
                let m = self.mapper.lock();
                let bound = m.mapping.bindings.iter().find(|b| b.action == *action && b.deck == d).map(|b| format!("{:?}", b.control));
                drop(m);
                ui.label(RichText::new(bound.unwrap_or_else(|| "—".into())).size(11.0).color(theme::TEXT_DIM));
                if ui.small_button("Learn").clicked() {
                    let _ = self.mapper.lock().learn(action, d);
                }
                ui.end_row();
            }
        });
    }
}

pub enum SettingsAction {
    ProcessAll,
    ReanalyseAll,
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(2.0);
    ui.label(RichText::new(title).size(16.0).strong());
    ui.add_space(4.0);
}

fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(theme::TEXT_DIM));
}

fn row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(egui::vec2(190.0, 20.0), egui::Label::new(label));
        add(ui);
    });
}

fn general_ui(ui: &mut Ui, p: &mut Prefs) {
    section(ui, "General");
    row(ui, "Key notation", |ui| {
        ui.selectable_value(&mut p.key_notation, KeyNotation::Camelot, "Camelot (8A)");
        ui.selectable_value(&mut p.key_notation, KeyNotation::OpenKey, "Open Key (1m)");
        ui.selectable_value(&mut p.key_notation, KeyNotation::Musical, "Musical (Am)");
    });
    row(ui, "Accent colour", |ui| {
        for a in crate::prefs::Accent::ALL {
            let c = theme::rgb(a.rgb());
            let sel = p.accent == a;
            let (r, resp) = ui.allocate_exact_size(egui::vec2(74.0, 22.0), egui::Sense::click());
            let pnt = ui.painter();
            pnt.rect_filled(r, 4.0, if sel { theme::SELECT } else { theme::PANEL_HI });
            pnt.rect_stroke(r, 4.0, egui::Stroke::new(1.0, if sel { c } else { theme::LINE }), egui::StrokeKind::Inside);
            pnt.circle_filled(egui::pos2(r.left() + 12.0, r.center().y), 5.0, c);
            pnt.text(egui::pos2(r.left() + 23.0, r.center().y), egui::Align2::LEFT_CENTER, a.label(), egui::FontId::proportional(12.5), theme::TEXT);
            if resp.clicked() {
                p.accent = a;
            }
        }
    });
    row(ui, "Interface size", |ui| {
        ui.add(egui::Slider::new(&mut p.ui_scale, 0.8..=1.5).step_by(0.05).custom_formatter(|v, _| format!("{:.0} %", v * 100.0)));
    });
    row(ui, "On start", |ui| {
        ui.checkbox(&mut p.start_on_home, "Open the home page");
    });
    ui.add_space(8.0);
    hint(ui, &format!("Library, stems and exports are stored in {}", clubcraft_core::paths::data_dir().display()));
}

fn decks_ui(ui: &mut Ui, p: &mut Prefs) {
    section(ui, "Decks");
    row(ui, "Cue button", |ui| {
        ui.selectable_value(&mut p.cue_button, CueButton::Cdj, "CDJ").on_hover_text("Stopped: set / preview the cue. Playing: back to the cue and stop.");
        ui.selectable_value(&mut p.cue_button, CueButton::Hold, "Hold").on_hover_text("Plays while held, returns on release");
        ui.selectable_value(&mut p.cue_button, CueButton::Cup, "Cue-up-play").on_hover_text("Release starts playback from the cue");
    });
    row(ui, "Hot cue while stopped", |ui| {
        ui.selectable_value(&mut p.hot_cue_press, HotCuePress::Preview, "Preview while held");
        ui.selectable_value(&mut p.hot_cue_press, HotCuePress::Play, "Start playing");
    });
    row(ui, "Park loaded track at", |ui| {
        ui.selectable_value(&mut p.load_position, LoadPosition::FirstBeat, "First beat");
        ui.selectable_value(&mut p.load_position, LoadPosition::MixIn, "Mix-in cue");
        ui.selectable_value(&mut p.load_position, LoadPosition::FileStart, "Start of file");
    });
    row(ui, "Default pitch range", |ui| {
        for r in [6u32, 8, 10, 16, 25, 50, 100] {
            ui.selectable_value(&mut p.pitch_range, r, format!("±{r}%"));
        }
    });
    row(ui, "On load", |ui| {
        ui.checkbox(&mut p.keylock, "Keylock on");
        ui.checkbox(&mut p.quantize, "Quantize on");
        ui.checkbox(&mut p.sync_on_load, "Sync to master");
    });
    row(ui, "Reset on load", |ui| {
        ui.checkbox(&mut p.reset_eq_on_load, "EQ & filter");
        ui.checkbox(&mut p.reset_stems_on_load, "Stems");
        ui.checkbox(&mut p.reset_key_on_load, "Key shift");
        ui.checkbox(&mut p.reset_pitch_on_load, "Pitch");
    });
}

fn mixer_ui(ui: &mut Ui, p: &mut Prefs) {
    section(ui, "Mixer");
    row(ui, "Crossfader curve", |ui| {
        ui.selectable_value(&mut p.crossfader_curve, XfCurve::Smooth, "Smooth");
        ui.selectable_value(&mut p.crossfader_curve, XfCurve::Full, "Full");
        ui.selectable_value(&mut p.crossfader_curve, XfCurve::Cut, "Cut");
        ui.selectable_value(&mut p.crossfader_curve, XfCurve::Scratch, "Scratch");
    });
    row(ui, "Channel fader curve", |ui| {
        ui.selectable_value(&mut p.fader_curve, FaderLaw::Linear, "Linear");
        ui.selectable_value(&mut p.fader_curve, FaderLaw::Smooth, "Smooth");
        ui.selectable_value(&mut p.fader_curve, FaderLaw::Fast, "Fast");
    });
    row(ui, "Auto gain", |ui| {
        ui.checkbox(&mut p.auto_gain, "Match the loudness of every track");
    });
    row(ui, "Master limiter", |ui| {
        ui.checkbox(&mut p.limiter, "On");
        ui.add_enabled(p.limiter, egui::Slider::new(&mut p.limiter_ceiling_db, -6.0..=0.0).suffix(" dB").text("ceiling"));
    });
}

fn waveforms_ui(ui: &mut Ui, p: &mut Prefs) {
    section(ui, "Waveforms");
    row(ui, "Colours", |ui| {
        ui.selectable_value(&mut p.stem_waveforms, true, "By stem");
        ui.selectable_value(&mut p.stem_waveforms, false, "By frequency band");
    });
    row(ui, "Stem palette", |ui| {
        ui.selectable_value(&mut p.soft_stem_colors, true, "Soft");
        ui.selectable_value(&mut p.soft_stem_colors, false, "Vivid");
        for k in [clubcraft_core::StemKind::Drums, clubcraft_core::StemKind::Bass, clubcraft_core::StemKind::Other, clubcraft_core::StemKind::Vocals] {
            ui.label(RichText::new("■").size(14.0).color(theme::stem_color(k)));
        }
    });
    row(ui, "Live waveform zoom", |ui| {
        ui.add(egui::Slider::new(&mut p.wave_zoom_secs, 2.0..=30.0).suffix(" s visible"));
    });
}

fn analysis_ui(ui: &mut Ui, p: &mut Prefs, actions: &mut Vec<SettingsAction>) {
    section(ui, "Analysis");
    row(ui, "BPM range", |ui| {
        ui.add(egui::DragValue::new(&mut p.bpm_min).range(50.0..=p.bpm_max - 10.0).speed(1.0));
        ui.label("to");
        ui.add(egui::DragValue::new(&mut p.bpm_max).range(p.bpm_min + 10.0..=250.0).speed(1.0));
        ui.label("BPM");
    });
    hint(ui, "Detected tempos are folded into this range (e.g. 70–180 for house and techno, 85–175 for drum & bass at half time).");
    row(ui, "Structure", |ui| {
        ui.checkbox(&mut p.deep_structure, "Deep structure analysis (All-In-One model)");
    });
    hint(ui, "Slightly better sections and cues; needs stems, about 1 min and 1.5 GB extra per track.");
    ui.add_space(6.0);
    section(ui, "Stems");
    row(ui, "Separate automatically", |ui| {
        ui.checkbox(&mut p.separate_stems, "For every imported track (in the background)");
    });
    let qualities = [
        ("best", "Best", "Mel-Band RoFormer vocals + htdemucs_ft: cleanest vocals, ~2.5 min per track"),
        ("high", "High", "htdemucs_ft, ~80 s per track"),
        ("fast", "Fast", "htdemucs, ~17 s per track"),
    ];
    row(ui, "Quality for playback", |ui| {
        for (q, l, h) in qualities {
            ui.selectable_value(&mut p.stem_quality, q.to_string(), l).on_hover_text(h);
        }
    });
    hint(ui, "Used for the decks, the studio and background separation. A deck plays a track while its stems are separated: with Fast the part under the playhead has stems within a second, with Best after about 15 s.");
    row(ui, "Quality for export", |ui| {
        for (q, l, h) in qualities {
            ui.selectable_value(&mut p.export_stem_quality, q.to_string(), l).on_hover_text(h);
        }
    });
    hint(ui, "Before a mix is rendered, tracks whose stems are weaker than this are separated again. The better stems are kept for playback too.");
    ui.add_space(6.0);
    section(ui, "Library");
    ui.horizontal(|ui| {
        if ui.button("Analyse + separate all tracks").clicked() {
            actions.push(SettingsAction::ProcessAll);
        }
        if ui.button("Re-analyse all (keep stems)").clicked() {
            actions.push(SettingsAction::ReanalyseAll);
        }
        if ui.button("Open data folder").clicked() {
            crate::platform::open(&clubcraft_core::paths::data_dir());
        }
    });
}

fn studio_ui(ui: &mut Ui, p: &mut Prefs) {
    section(ui, "Studio (new transitions)");
    row(ui, "Default transition", |ui| {
        egui::ComboBox::from_id_salt("pref-tstyle").selected_text(p.transition_style.label()).show_ui(ui, |ui| {
            for s in TransitionStyle::ALL.into_iter().filter(|s| *s != TransitionStyle::Custom) {
                ui.selectable_value(&mut p.transition_style, s, s.label());
            }
        });
    });
    row(ui, "Transition length", |ui| {
        ui.selectable_value(&mut p.transition_bars, None, "Auto");
        for b in [4u32, 8, 16, 32] {
            ui.selectable_value(&mut p.transition_bars, Some(b), format!("{b} bars"));
        }
    });
    hint(ui, "Auto picks the length from the outro of the outgoing and the intro of the incoming track. Stem transitions fall back to a bass swap for tracks without stems.");
    row(ui, "Harmonic mixing", |ui| {
        ui.checkbox(&mut p.allow_key_shift, "Shift keys by ±1 semitone to avoid clashes");
    });
    row(ui, "Keylock in mixes", |ui| {
        ui.checkbox(&mut p.mix_keylock, "Keep the original key when the tempo changes");
    });
}

fn export_ui(ui: &mut Ui, p: &mut Prefs) {
    section(ui, "Mix export");
    row(ui, "Format", |ui| {
        for f in [AudioFormat::Wav16, AudioFormat::Wav24, AudioFormat::Wav32Float] {
            ui.selectable_value(&mut p.export_format, f, f.label());
        }
    });
    row(ui, "Cue sheet", |ui| {
        ui.checkbox(&mut p.export_cue_sheet, "Write a .cue track list next to the mix");
    });
    row(ui, "Tail", |ui| {
        ui.add(egui::Slider::new(&mut p.export_tail_secs, 0.0..=10.0).suffix(" s").text("after the last track"));
    });
    ui.add_space(6.0);
    section(ui, "Recording (live)");
    row(ui, "Format", |ui| {
        for f in [AudioFormat::Wav16, AudioFormat::Wav24, AudioFormat::Wav32Float] {
            ui.selectable_value(&mut p.record_format, f, f.label());
        }
    });
    hint(ui, &format!("Recordings go to {}", clubcraft_core::paths::recordings_dir().display()));
}
