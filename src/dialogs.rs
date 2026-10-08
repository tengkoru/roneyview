//! Jendela dialog (Properties, Set as wallpaper), kotak konfirmasi, dan aksi berkas
//! (buka di dalam folder, pindahkan ke sampah).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, pos2, vec2, Align2, Color32, Context, Rect, Sense, TextureId, Ui};

use crate::loader;
use crate::source::Listing;
use crate::wallpaper::{self, Backend, ColorMode, WpPrefs, WpStyle};

// ------------------------------------------------------------------ kotak modal

pub enum Modal {
    /// File berada di dalam arsip: Roneyview tidak melakukan tindakan apa pun.
    ArchiveWarning { archive: String },
    ConfirmTrash { index: usize, path: PathBuf, name: String },
    Error(String),
}

pub enum ModalResult {
    Keep,
    Close,
    ConfirmTrash,
}

pub fn draw_modal(ctx: &Context, modal: &Modal) -> ModalResult {
    let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let mut result = ModalResult::Keep;
    let (title, width) = match modal {
        Modal::ArchiveWarning { .. } => ("Peringatan", 420.0),
        Modal::ConfirmTrash { .. } => ("Pindahkan ke sampah", 420.0),
        Modal::Error(_) => ("Terjadi kesalahan", 420.0),
    };
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .default_width(width)
        .show(ctx, |ui| {
            ui.set_max_width(width);
            match modal {
                Modal::ArchiveWarning { archive } => {
                    ui.label("Gambar ini berada di dalam arsip:");
                    ui.add(egui::Label::new(egui::RichText::new(archive).monospace()).wrap());
                    ui.add_space(6.0);
                    ui.label("Roneyview tidak melakukan tindakan lain.");
                    ui.add_space(8.0);
                    if ui.button("OK").clicked() || esc {
                        result = ModalResult::Close;
                    }
                }
                Modal::ConfirmTrash { name, path, .. } => {
                    ui.label(format!("Pindahkan \"{name}\" ke Tempat Sampah?"));
                    ui.add(
                        egui::Label::new(egui::RichText::new(path.display().to_string()).monospace().weak())
                            .wrap(),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Pindahkan ke sampah").clicked() {
                            result = ModalResult::ConfirmTrash;
                        }
                        if ui.button("Batal").clicked() || esc {
                            result = ModalResult::Close;
                        }
                    });
                }
                Modal::Error(msg) => {
                    ui.add(egui::Label::new(msg.as_str()).wrap());
                    ui.add_space(8.0);
                    if ui.button("OK").clicked() || esc {
                        result = ModalResult::Close;
                    }
                }
            }
        });
    result
}

// ------------------------------------------------------------- jendela terpisah

/// Tampilkan dialog sebagai jendela OS terpisah (atau jendela mengambang bila backend
/// tidak mendukung banyak jendela).
pub fn show_dialog(
    ctx: &Context,
    id: &str,
    title: &str,
    size: [f32; 2],
    open: &mut bool,
    mut add: impl FnMut(&Context, &mut Ui),
) {
    let vid = egui::ViewportId::from_hash_of(id);
    let builder = egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size(size)
        .with_min_inner_size([320.0, 200.0])
        .with_app_id("roneyview");
    let mut close = false;
    ctx.show_viewport_immediate(vid, builder, |ctx, class| {
        if class == egui::ViewportClass::Embedded {
            let mut o = true;
            egui::Window::new(title).open(&mut o).show(ctx, |ui| add(ctx, ui));
            if !o {
                close = true;
            }
        } else {
            egui::CentralPanel::default().show(ctx, |ui| add(ctx, ui));
            if ctx.input(|i| i.viewport().close_requested() || i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
        }
    });
    if close {
        *open = false;
    }
}

// ------------------------------------------------------------------- Properties

pub struct PropsDialog {
    pub rows: Vec<(String, String)>,
}

pub fn draw_properties(ui: &mut Ui, dlg: &PropsDialog) -> bool {
    let mut close = false;
    egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
        egui::Grid::new("props_grid")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .striped(true)
            .show(ui, |ui| {
                for (k, v) in &dlg.rows {
                    ui.label(egui::RichText::new(k).strong());
                    ui.add(egui::Label::new(v.as_str()).selectable(true).wrap());
                    ui.end_row();
                }
            });
    });
    ui.add_space(8.0);
    if ui.button("Tutup").clicked() {
        close = true;
    }
    close
}

// --------------------------------------------------------------- Set as wallpaper

pub enum WpStatus {
    Idle,
    Working,
    Done(String),
    Failed(String),
}

pub struct WallpaperDialog {
    pub index: usize,
    pub name: String,
    pub tex: TextureId,
    pub img_size: [f32; 2],
    pub screen: [f32; 2],
    pub prefs: WpPrefs,
    pub hex: String,
    pub backend: Backend,
    pub listing: Arc<Listing>,
    pub status: WpStatus,
    rx: Option<Receiver<Result<String, String>>>,
}

impl WallpaperDialog {
    pub fn new(
        listing: Arc<Listing>,
        index: usize,
        tex: TextureId,
        img_size: [u32; 2],
        screen: [f32; 2],
        prefs: WpPrefs,
    ) -> Self {
        let name = listing.names.get(index).cloned().unwrap_or_default();
        WallpaperDialog {
            index,
            name,
            tex,
            img_size: [img_size[0] as f32, img_size[1] as f32],
            screen,
            hex: wallpaper::color_hex(prefs.color),
            prefs,
            backend: wallpaper::detect(),
            listing,
            status: WpStatus::Idle,
            rx: None,
        }
    }

    fn start_apply(&mut self, ctx: &Context) {
        let (tx, rx): (Sender<Result<String, String>>, _) = mpsc::channel();
        self.rx = Some(rx);
        self.status = WpStatus::Working;
        let (listing, index, backend, prefs) = (
            self.listing.clone(),
            self.index,
            self.backend.clone(),
            self.prefs.clone(),
        );
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("roneyview-wallpaper".into())
            .spawn(move || {
                let work = || -> Result<String, String> {
                    let mut reader = listing.open_reader()?;
                    let bytes = reader.read(index)?;
                    let img = loader::decode_rgba(&bytes, 8192, 40_000_000)?;
                    let original = if listing.is_archive() {
                        None
                    } else {
                        listing.names.get(index).map(|n| listing.origin.join(n))
                    };
                    let original = original.and_then(|p| std::fs::canonicalize(p).ok());
                    wallpaper::apply(&backend, &img, original.as_deref(), prefs.style, prefs.mode, prefs.color)
                };
                let res = catch_unwind(AssertUnwindSafe(work))
                    .unwrap_or_else(|_| Err("Kesalahan internal saat menerapkan wallpaper".into()));
                let _ = tx.send(res);
                ctx.request_repaint();
            });
        if spawned.is_err() {
            self.status = WpStatus::Failed("Tidak dapat memulai proses penerapan".into());
            self.rx = None;
        }
    }
}

fn checkerboard(painter: &egui::Painter, rect: Rect) {
    let cell = 8.0;
    let (a, b) = (Color32::from_gray(200), Color32::from_gray(150));
    let cols = (rect.width() / cell).ceil() as i32;
    let rows = (rect.height() / cell).ceil() as i32;
    for r in 0..rows {
        for c in 0..cols {
            let min = rect.min + vec2(c as f32 * cell, r as f32 * cell);
            let cell_rect = Rect::from_min_size(min, vec2(cell, cell)).intersect(rect);
            painter.rect_filled(cell_rect, 0.0, if (r + c) % 2 == 0 { a } else { b });
        }
    }
}

/// Pratinjau monitor mini; memakai `wallpaper::place` yang sama dengan renderer.
fn draw_preview(ui: &mut Ui, dlg: &WallpaperDialog) {
    let [sw, sh] = dlg.screen;
    let max_w = ui.available_width().clamp(200.0, 380.0);
    let (mut pw, mut ph) = (max_w, max_w * sh / sw);
    if ph > 230.0 {
        ph = 230.0;
        pw = ph * sw / sh;
    }
    let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width(), ph + 8.0), Sense::hover());
    let rect = Rect::from_center_size(outer.center(), vec2(pw, ph));
    let painter = ui.painter_at(rect.expand(2.0));
    match dlg.prefs.mode {
        ColorMode::Transparent => checkerboard(&painter, rect),
        ColorMode::Solid => {
            let c = dlg.prefs.color;
            painter.rect_filled(rect, 0.0, Color32::from_rgb(c[0], c[1], c[2]));
        }
    }
    let k = pw / sw;
    let clip = ui.painter_at(rect);
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    if let Some(p) = wallpaper::place(dlg.prefs.style, dlg.img_size[0], dlg.img_size[1], sw, sh) {
        if p.tiled {
            let (tw, th) = (p.w * k, p.h * k);
            if tw >= 2.0 && th >= 2.0 && (pw / tw).ceil() * (ph / th).ceil() <= 4096.0 {
                let mut y = 0.0;
                while y < ph {
                    let mut x = 0.0;
                    while x < pw {
                        let r = Rect::from_min_size(rect.min + vec2(x, y), vec2(tw, th));
                        clip.image(dlg.tex, r, uv, Color32::WHITE);
                        x += tw;
                    }
                    y += th;
                }
            } else {
                clip.image(dlg.tex, rect, uv, Color32::WHITE); // ubin terlalu kecil: aproksimasi
            }
        } else {
            let r = Rect::from_min_size(rect.min + vec2(p.x, p.y) * k, vec2(p.w, p.h) * k);
            clip.image(dlg.tex, r, uv, Color32::WHITE);
        }
    }
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(2.0_f32, Color32::from_gray(90)),
        egui::StrokeKind::Outside,
    );
}

/// Isi jendela "Set as wallpaper". Mengembalikan true bila harus ditutup.
pub fn draw_wallpaper(ctx: &Context, ui: &mut Ui, dlg: &mut WallpaperDialog) -> bool {
    if let Some(rx) = &dlg.rx {
        if let Ok(res) = rx.try_recv() {
            dlg.status = match res {
                Ok(m) => WpStatus::Done(m),
                Err(e) => WpStatus::Failed(e),
            };
            dlg.rx = None;
        }
    }
    let mut close = false;
    ui.label(egui::RichText::new(&dlg.name).strong());
    ui.add_space(4.0);
    draw_preview(ui, dlg);
    ui.add_space(6.0);

    egui::Grid::new("wp_grid").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
        ui.label("Color");
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("wp_color_mode")
                .selected_text(dlg.prefs.mode.label())
                .width(130.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut dlg.prefs.mode, ColorMode::Solid, ColorMode::Solid.label());
                    ui.selectable_value(
                        &mut dlg.prefs.mode,
                        ColorMode::Transparent,
                        ColorMode::Transparent.label(),
                    );
                });
            if dlg.prefs.mode == ColorMode::Solid {
                if egui::color_picker::color_edit_button_srgb(ui, &mut dlg.prefs.color).changed() {
                    dlg.hex = wallpaper::color_hex(dlg.prefs.color);
                }
                let r = ui.add(egui::TextEdit::singleline(&mut dlg.hex).desired_width(76.0));
                if r.changed() {
                    if let Some(c) = wallpaper::parse_hex(&dlg.hex) {
                        dlg.prefs.color = c;
                    }
                }
                if r.lost_focus() {
                    dlg.hex = wallpaper::color_hex(dlg.prefs.color); // rapikan input
                }
            }
        });
        ui.end_row();

        ui.label("Style");
        egui::ComboBox::from_id_salt("wp_style")
            .selected_text(dlg.prefs.style.label())
            .width(130.0)
            .show_ui(ui, |ui| {
                for s in WpStyle::ALL {
                    ui.selectable_value(&mut dlg.prefs.style, s, s.label());
                }
            });
        ui.end_row();
    });

    ui.add_space(6.0);
    ui.add(egui::Label::new(egui::RichText::new(format!("Metode: {}", dlg.backend.label())).weak()).wrap());
    if dlg.prefs.mode == ColorMode::Transparent && dlg.backend.supported() && !dlg.backend.has_transparent() {
        ui.add(
            egui::Label::new(
                egui::RichText::new("Transparent hanya tersedia di Xfce; di desktop ini dipakai warna hitam.")
                    .color(Color32::from_rgb(230, 190, 90)),
            )
            .wrap(),
        );
    }
    match &dlg.status {
        WpStatus::Idle => {}
        WpStatus::Working => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Menerapkan...");
            });
        }
        WpStatus::Done(m) => {
            ui.add(egui::Label::new(egui::RichText::new(m.as_str()).color(Color32::from_rgb(110, 200, 120))).wrap());
        }
        WpStatus::Failed(e) => {
            ui.add(egui::Label::new(egui::RichText::new(e.as_str()).color(Color32::from_rgb(235, 110, 110))).wrap());
        }
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let can = dlg.backend.supported() && !matches!(dlg.status, WpStatus::Working);
        if ui.add_enabled(can, egui::Button::new("Apply")).clicked() {
            dlg.start_apply(ctx);
        }
        if ui.button("Tutup").clicked() {
            close = true;
        }
    });
    close
}

// -------------------------------------------------------------------- aksi berkas

/// Buka folder tempat berkas berada di file manager, dan pilih berkasnya bila bisa
/// (org.freedesktop.FileManager1); bila tidak, buka foldernya dengan xdg-open.
/// Hasil gagal dikirim lewat `notes`.
pub fn open_in_folder(path: &Path, notes: Sender<String>, ctx: Context) {
    let path = path.to_path_buf();
    let _ = std::thread::Builder::new().name("roneyview-open-folder".into()).spawn(move || {
        let timeout = Duration::from_secs(10);
        let uri = wallpaper::file_uri(&path.to_string_lossy());
        let selected = wallpaper::which("gdbus")
            && wallpaper::run(
                &[
                    "gdbus".to_string(),
                    "call".into(),
                    "--session".into(),
                    "--dest".into(),
                    "org.freedesktop.FileManager1".into(),
                    "--object-path".into(),
                    "/org/freedesktop/FileManager1".into(),
                    "--method".into(),
                    "org.freedesktop.FileManager1.ShowItems".into(),
                    format!("['{uri}']"),
                    String::new(),
                ],
                timeout,
            )
            .is_ok();
        if !selected {
            let dir = path.parent().unwrap_or(Path::new("/")).to_string_lossy().into_owned();
            if let Err(e) = wallpaper::run(&["xdg-open".to_string(), dir], timeout) {
                let _ = notes.send(format!("Tidak dapat membuka folder: {e}"));
                ctx.request_repaint();
            }
        }
    });
}

/// Pindahkan ke Tempat Sampah (spesifikasi freedesktop.org Trash).
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|e| format!("Gagal memindahkan ke sampah: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn berkas_dipindahkan_ke_sampah_dan_hilang_dari_folder() {
        let base = std::env::temp_dir().join(format!("roneyview-trash-{}", std::process::id()));
        let data = base.join("share");
        std::fs::create_dir_all(&data).unwrap();
        // Tempat sampah satu perangkat dengan berkas: paksa lewat XDG_DATA_HOME.
        let file = base.join("gambar.png");
        std::fs::write(&file, b"x").unwrap();
        // set_var aman karena hanya tes ini yang menyentuh variabel tersebut.
        std::env::set_var("XDG_DATA_HOME", &data);
        let res = move_to_trash(&file);
        assert!(res.is_ok(), "{res:?}");
        assert!(!file.exists());
        assert!(data.join("Trash/files/gambar.png").exists());
        assert!(move_to_trash(&base.join("tidak-ada.png")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
