//! Roneyview - penampil gambar cepat bergaya Honeyview untuk Linux.

mod app;
mod dialogs;
mod fileinfo;
mod loader;
mod natsort;
mod settings;
mod source;
mod wallpaper;

use std::io::Write;
use std::path::PathBuf;

const HELP: &str = "Roneyview - penampil gambar (folder, ZIP/CBZ, RAR/CBR)

Pemakaian:
  roneyview [BERKAS | FOLDER | ARSIP.zip/.cbz/.rar/.cbr]
  roneyview --set-wallpaper BERKAS [--style GAYA] [--color #RRGGBB|transparent]
  roneyview --restore-wallpaper

GAYA: none, centered, tiled, stretched, scaled, zoomed (bawaan: zoomed).
Tanpa argumen, Roneyview membuka jendela kosong (Ctrl+O untuk membuka gambar).
Tekan F1 di dalam aplikasi untuk daftar pintasan keyboard.";

/// `--set-wallpaper BERKAS [--style GAYA] [--color WARNA]` tanpa membuka jendela.
fn cli_set_wallpaper(rest: &[std::ffi::OsString]) -> Result<String, String> {
    use wallpaper::{ColorMode, WpStyle};
    let file = rest.first().ok_or("Sebutkan berkas gambar setelah --set-wallpaper")?;
    let mut style = WpStyle::default();
    let mut mode = ColorMode::Solid;
    let mut color = [0u8, 0, 0];
    let mut it = rest[1..].iter();
    while let Some(flag) = it.next() {
        let val = it
            .next()
            .and_then(|v| v.to_str())
            .ok_or_else(|| format!("Nilai untuk {} tidak ada", flag.to_string_lossy()))?;
        match flag.to_str() {
            Some("--style") => {
                style = WpStyle::parse(val).ok_or_else(|| format!("Gaya tidak dikenal: {val}"))?;
            }
            Some("--color") => {
                if val.eq_ignore_ascii_case("transparent") {
                    mode = ColorMode::Transparent;
                } else {
                    color = wallpaper::parse_hex(val).ok_or_else(|| format!("Warna tidak valid: {val}"))?;
                }
            }
            _ => return Err(format!("Opsi tidak dikenal: {}", flag.to_string_lossy())),
        }
    }
    let path = std::path::Path::new(file);
    let abs = std::fs::canonicalize(path).map_err(|e| format!("Tidak dapat membuka {}: {e}", path.display()))?;
    let bytes = std::fs::read(&abs).map_err(|e| format!("Gagal membaca {}: {e}", abs.display()))?;
    let img = loader::decode_rgba(&bytes, 8192, 40_000_000)?;
    wallpaper::apply(&wallpaper::detect(), &img, Some(&abs), style, mode, color)
}

fn cli_report(result: Result<String, String>) -> eframe::Result {
    match result {
        Ok(msg) => {
            let _ = writeln!(std::io::stdout(), "{msg}");
            Ok(())
        }
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "roneyview: {e}");
            std::process::exit(1);
        }
    }
}

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
            Some("--restore-wallpaper") => return cli_report(wallpaper::restore()),
            Some("--set-wallpaper") => {
                let rest: Vec<_> = args.collect();
                return cli_report(cli_set_wallpaper(&rest));
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
