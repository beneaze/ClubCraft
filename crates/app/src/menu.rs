//! Native menu bar (macOS): ClubCraft ▸ About / Settings… (⌘,) / Quit, File, Edit, View,
//! Playback, Window and Help. Other platforms keep the in-window toolbar.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuCmd {
    Settings,
    NewMix,
    AddMusic,
    ImportDjStudio,
    ExportMix,
    ExportVirtualDj,
    SaveMix,
    ViewHome,
    ViewLibrary,
    ViewStudio,
    ViewLive,
    PlayPause,
    OpenDataFolder,
    Help,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::MenuCmd;
    use muda::accelerator::{Accelerator, Code, Modifiers};
    use muda::{AboutMetadata, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};

    pub struct AppMenu {
        _menu: Menu,
    }

    const ITEMS: &[(&str, MenuCmd)] = &[
        ("settings", MenuCmd::Settings),
        ("new-mix", MenuCmd::NewMix),
        ("add-music", MenuCmd::AddMusic),
        ("import-djs", MenuCmd::ImportDjStudio),
        ("export-mix", MenuCmd::ExportMix),
        ("export-vdj", MenuCmd::ExportVirtualDj),
        ("save-mix", MenuCmd::SaveMix),
        ("view-home", MenuCmd::ViewHome),
        ("view-library", MenuCmd::ViewLibrary),
        ("view-studio", MenuCmd::ViewStudio),
        ("view-live", MenuCmd::ViewLive),
        ("play-pause", MenuCmd::PlayPause),
        ("data-folder", MenuCmd::OpenDataFolder),
        ("help", MenuCmd::Help),
    ];

    fn item(id: &str, text: &str, acc: Option<(Modifiers, Code)>) -> MenuItem {
        MenuItem::with_id(id, text, true, acc.map(|(m, c)| Accelerator::new(m, c)))
    }

    impl AppMenu {
        /// Build and install the menu bar; menu clicks wake the UI through `ctx`.
        pub fn install(ctx: &eframe::egui::Context) -> Option<AppMenu> {
            let cmd = Modifiers::META;
            let about = AboutMetadata {
                name: Some("ClubCraft".into()),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                comments: Some("Open-source DJ studio and live performance app with stem separation.".into()),
                ..Default::default()
            };
            let app_menu = Submenu::with_items(
                "ClubCraft",
                true,
                &[
                    &PredefinedMenuItem::about(Some("About ClubCraft"), Some(about)),
                    &PredefinedMenuItem::separator(),
                    &item("settings", "Settings…", Some((cmd, Code::Comma))),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::services(None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::hide(None),
                    &PredefinedMenuItem::hide_others(None),
                    &PredefinedMenuItem::show_all(None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::quit(None),
                ],
            )
            .ok()?;
            let file = Submenu::with_items(
                "File",
                true,
                &[
                    &item("new-mix", "New Mix", Some((cmd, Code::KeyN))),
                    &item("add-music", "Add Music Folder…", Some((cmd, Code::KeyO))),
                    &item("save-mix", "Save Mix", Some((cmd, Code::KeyS))),
                    &PredefinedMenuItem::separator(),
                    &item("import-djs", "Import DJ.Studio Projects", None),
                    &item("export-mix", "Export Mix as Audio…", Some((cmd, Code::KeyE))),
                    &item("export-vdj", "Export Cues to VirtualDJ…", None),
                    &PredefinedMenuItem::separator(),
                    &item("data-folder", "Show Data Folder", None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::close_window(None),
                ],
            )
            .ok()?;
            let edit = Submenu::with_items(
                "Edit",
                true,
                &[
                    &PredefinedMenuItem::undo(None),
                    &PredefinedMenuItem::redo(None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::cut(None),
                    &PredefinedMenuItem::copy(None),
                    &PredefinedMenuItem::paste(None),
                    &PredefinedMenuItem::select_all(None),
                ],
            )
            .ok()?;
            let view = Submenu::with_items(
                "View",
                true,
                &[
                    &item("view-home", "Home", Some((cmd, Code::Digit1))),
                    &item("view-library", "Library", Some((cmd, Code::Digit2))),
                    &item("view-studio", "Studio", Some((cmd, Code::Digit3))),
                    &item("view-live", "Live", Some((cmd, Code::Digit4))),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::fullscreen(None),
                ],
            )
            .ok()?;
            let playback = Submenu::with_items("Playback", true, &[&item("play-pause", "Play / Pause Mix", None)]).ok()?;
            let window = Submenu::with_items("Window", true, &[&PredefinedMenuItem::minimize(None), &PredefinedMenuItem::fullscreen(None)]).ok()?;
            let help = Submenu::with_items("Help", true, &[&item("help", "ClubCraft Help", None)]).ok()?;
            let menu = Menu::with_items(&[&app_menu, &file, &edit, &view, &playback, &window, &help]).ok()?;
            menu.init_for_nsapp();
            log::info!("native menu bar installed");
            window.set_as_windows_menu_for_nsapp();
            let ctx = ctx.clone();
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                PENDING.lock().push(e.id.0.clone());
                ctx.request_repaint();
            }));
            Some(AppMenu { _menu: menu })
        }

        pub fn poll(&self) -> Vec<MenuCmd> {
            let ids: Vec<String> = std::mem::take(&mut *PENDING.lock());
            ids.iter().filter_map(|id| ITEMS.iter().find(|(k, _)| k == id).map(|(_, c)| *c)).collect()
        }
    }

    static PENDING: parking_lot::Mutex<Vec<String>> = parking_lot::const_mutex(Vec::new());
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::MenuCmd;
    pub struct AppMenu;
    impl AppMenu {
        pub fn install(_ctx: &eframe::egui::Context) -> Option<AppMenu> {
            None
        }
        pub fn poll(&self) -> Vec<MenuCmd> {
            Vec::new()
        }
    }
}

pub use imp::AppMenu;
