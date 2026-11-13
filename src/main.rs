#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod bundled;
mod crop;
mod ffmpeg;
mod timeline;

/// The program's name, spelled the one way it is spelled everywhere.
pub const APP_NAME: &str = "finnCrop";

/// `1.4`. Cargo only accepts three-part versions, so Cargo.toml carries
/// `1.4.0` and build.rs trims the trailing part once, here.
pub const VERSION: &str = env!("FINN_CROP_VERSION");

/// What the title bar and the About window say: `finnCrop v1.4`.
pub fn version_line() -> String {
    format!("{APP_NAME} v{VERSION}")
}

/// The window icon as raw RGBA, written by tools\make-icon.ps1.  Windows is
/// given this one bitmap and shrinks it for the title bar and the taskbar, so
/// it is the small mark - the tile and its big play triangle - at 64 px,
/// rather than the 64 px crop frame.
fn window_icon() -> eframe::egui::IconData {
    const SIZE: u32 = 64;
    let rgba = include_bytes!("icon-64.rgba").to_vec();
    debug_assert_eq!(rgba.len(), (SIZE * SIZE * 4) as usize);
    eframe::egui::IconData { rgba, width: SIZE, height: SIZE }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(version_line())
            .with_icon(window_icon())
            .with_inner_size([1100.0, 780.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(APP_NAME, options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
