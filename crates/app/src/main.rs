//! ClubCraft — open-source DJ studio + live performance app.

// No console window for release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod devctl;
mod extract;
mod home;
mod importer;
mod interop;
mod jobs;
mod library_view;
mod live;
mod mashup;
mod mashup_view;
mod mcp;
mod menu;
mod pipeline;
mod platform;
mod prefs;
mod settings;
mod studio;
mod studio_timeline;
mod studio_transition;
mod theme;
mod waveform;
mod widgets;

fn main() -> eframe::Result<()> {
    // `clubcraft mcp`: Model Context Protocol server on stdio (no window).
    if std::env::args().nth(1).as_deref() == Some("mcp") {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).target(env_logger::Target::Stderr).init();
        if let Err(e) = mcp::run() {
            eprintln!("clubcraft mcp: {e:#}");
            std::process::exit(1);
        }
        return Ok(());
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("ClubCraft")
            .with_inner_size([1480.0, 920.0])
            .with_min_inner_size([1100.0, 700.0])
            .with_drag_and_drop(true)
            .with_icon(std::sync::Arc::new(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")).unwrap_or_default())),
        ..Default::default()
    };
    eframe::run_native("ClubCraft", opts, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
