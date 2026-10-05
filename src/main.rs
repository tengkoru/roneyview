//! Roneyview - penampil gambar cepat bergaya Honeyview untuk Linux.

mod app;
mod loader;
mod natsort;
mod settings;
mod source;

use std::io::Write;
use std::path::PathBuf;

const HELP: &str = "Roneyview - penampil gambar (folder, ZIP/CBZ, RAR/CBR)

Pemakaian:
  roneyview [BERKAS | FOLDER | ARSIP.zip/.cbz/.rar/.cbr]

Tanpa argumen, Roneyview melanjutkan dari tempat terakhir Anda berhenti.
Tekan F1 di dalam aplikasi untuk daftar pintasan keyboard.";

fn main() -> eframe::Result {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    if let Some(a) = &first {
        match a.to_str() {
            Some("-h" | "--help") => {
                let _ = writeln!(std::io::stdout(), "{HELP}");
                return Ok(());
            }
            Some("-V" | "--version") => {
                let _ = writeln!(std::io::stdout(), "roneyview {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => {}
        }
    }
    let arg: Option<PathBuf> = first.map(PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Roneyview")
            .with_app_id("roneyview")
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([360.0, 240.0])
            .with_drag_and_drop(true),
        renderer: eframe::Renderer::Glow,
        vsync: true,
        ..Default::default()
    };
    eframe::run_native(
        "Roneyview",
        options,
        Box::new(move |cc| Ok(Box::new(app::RoneyApp::new(cc, arg)?))),
    )
}
