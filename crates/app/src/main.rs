//! ClubCraft — open-source DJ studio + live performance app.

mod app;
mod devctl;
mod importer;
mod interop;
mod jobs;
mod library_view;
mod pipeline;
mod theme;
mod waveform;
mod widgets;

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("ClubCraft")
            .with_inner_size([1480.0, 920.0])
            .with_min_inner_size([1100.0, 700.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("ClubCraft", opts, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
