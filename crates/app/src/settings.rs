//! Settings window: audio output, headphone routing, MIDI controllers (with learn mode) and
//! library maintenance.

use crate::theme;
use clubcraft_engine::midi::{self, MidiConnection, MidiMapper, MidiMapping};
use clubcraft_engine::{AudioConfig, DeviceInfo, EngineHandle, EngineState, HeadphoneRouting};
use eframe::egui::{self, RichText};
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

    pub fn ui(&mut self, ctx: &egui::Context, handle: Option<&EngineHandle>, actions: &mut Vec<SettingsAction>) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new("Settings").open(&mut open).default_width(560.0).resizable(true).show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(620.0).show(ui, |ui| {
                self.audio_ui(ui, handle);
                ui.separator();
                self.midi_ui(ui, handle);
                ui.separator();
                ui.heading("Library");
                ui.horizontal(|ui| {
                    if ui.button("Analyse + separate all tracks").clicked() {
                        actions.push(SettingsAction::ProcessAll);
                    }
                    if ui.button("Re-analyse all (keep stems)").clicked() {
                        actions.push(SettingsAction::ReanalyseAll);
                    }
                    if ui.button("Open data folder").clicked() {
                        let _ = std::process::Command::new("open").arg(clubcraft_core::paths::data_dir()).spawn();
                    }
                });
                ui.label(RichText::new(format!("Data: {}", clubcraft_core::paths::data_dir().display())).size(11.0).color(theme::TEXT_DIM));
                ui.label(RichText::new(format!("Models: {}", clubcraft_core::paths::models_dir().display())).size(11.0).color(theme::TEXT_DIM));
                if !self.message.is_empty() {
                    ui.separator();
                    ui.label(&self.message);
                }
            });
        });
        self.open = open;
    }

    fn audio_ui(&mut self, ui: &mut egui::Ui, handle: Option<&EngineHandle>) {
        ui.heading("Audio output");
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
        ui.heading("MIDI controller");
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
