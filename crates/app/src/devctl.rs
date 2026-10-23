//! Developer control file for automated checks. When `CLUBCRAFT_DEVCTL=<file>` is set, the app
//! polls that file; each line is a command: `shot <png path>`, `view <library|studio|live>`,
//! `drop <path>` (as if dropped from Finder at the pointer), `key <name>` (a key press, egui's key
//! names, e.g. `Backspace`), `quit`. The file is deleted after it has been read.

use crate::app::View;
use eframe::egui;
use std::path::PathBuf;

/// A file "dropped" by the `drop` command.
#[derive(Debug)]
struct DevDrop(PathBuf);

impl egui::DroppedFile for DevDrop {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

#[derive(Default)]
pub struct DevCtl {
    file: Option<PathBuf>,
    pending_shot: Option<PathBuf>,
    last_check: f64,
}

pub enum DevCmd {
    View(View),
    Quit,
    /// Free-form command for the current view (e.g. `load 0 <track id>`).
    Other(String),
}

impl DevCtl {
    pub fn from_env() -> DevCtl {
        DevCtl { file: std::env::var_os("CLUBCRAFT_DEVCTL").map(PathBuf::from), ..Default::default() }
    }

    pub fn poll(&mut self, ctx: &egui::Context) -> Vec<DevCmd> {
        let mut out = Vec::new();
        let Some(file) = &self.file else { return out };
        // Deliver screenshots requested earlier.
        if let Some(path) = &self.pending_shot {
            let img = ctx.input(|i| {
                i.raw.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(img) = img {
                let [w, h] = img.size;
                let mut buf = image::RgbaImage::new(w as u32, h as u32);
                for (i, px) in img.pixels.iter().enumerate() {
                    buf.put_pixel((i % w) as u32, (i / w) as u32, image::Rgba(px.to_array()));
                }
                if let Err(e) = buf.save(path) {
                    log::error!("screenshot: {e}");
                }
                self.pending_shot = None;
            }
        }
        let now = ctx.input(|i| i.time);
        ctx.request_repaint_after(std::time::Duration::from_millis(300));
        if now - self.last_check < 0.25 {
            return out;
        }
        self.last_check = now;
        let Ok(text) = std::fs::read_to_string(file) else { return out };
        let _ = std::fs::remove_file(file);
        for line in text.lines() {
            let line = line.trim();
            let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
            match cmd {
                "shot" => {
                    self.pending_shot = Some(PathBuf::from(arg.trim()));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                }
                "view" => out.push(DevCmd::View(match arg.trim() {
                    "studio" => View::Studio,
                    "home" => View::Home,
                    "live" => View::Live,
                    _ => View::Library,
                })),
                "drop" => {
                    let file: egui::DroppedFileHandle = std::sync::Arc::new(DevDrop(PathBuf::from(arg.trim())));
                    ctx.input_mut(|i| i.raw.dropped_files.push(file));
                }
                "key" => match egui::Key::from_name(arg.trim()) {
                    Some(key) => ctx.input_mut(|i| {
                        for pressed in [true, false] {
                            i.events.push(egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers::NONE });
                        }
                    }),
                    None => log::warn!("devctl: unknown key `{arg}`"),
                },
                "quit" => out.push(DevCmd::Quit),
                "" => {}
                _ => out.push(DevCmd::Other(line.to_string())),
            }
        }
        out
    }
}
