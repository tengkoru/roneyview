//! Pilihan tema tampilan Roneyview.
//!
//! Empat tema yang bisa dipilih pengguna lewat menu Berkas > Tema:
//! "Dark Navy", "Neon", "Mono", dan "Aqua".

use eframe::egui::{Color32, Visuals};
use serde::{Deserialize, Serialize};

/// Tema tampilan yang tersedia.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Theme {
    /// Biru navy gelap (biru terang sebagai aksen).
    DarkNavy,
    /// Hitam dengan aksen hijau neon.
    Neon,
    /// Monokrom terang (hitam-putih).
    #[default]
    Mono,
    /// Terang kebiruan gaya macOS dengan aksen biru.
    Aqua,
}

impl Theme {
    /// Nama tampilan tema (dipakai di menu & pengaturan, tidak diterjemahkan).
    pub fn name(self) -> &'static str {
        match self {
            Theme::DarkNavy => "Dark Navy",
            Theme::Neon => "Neon",
            Theme::Mono => "Mono",
            Theme::Aqua => "Aqua",
        }
    }

    /// Semua tema, sesuai urutan tampil di menu.
    pub fn all() -> [Theme; 4] {
        [Theme::DarkNavy, Theme::Neon, Theme::Mono, Theme::Aqua]
    }

    /// Warna latar area gambar (panel tengah).
    pub fn bg(self) -> Color32 {
        match self {
            Theme::DarkNavy => Color32::from_rgb(10, 20, 40),
            Theme::Neon => Color32::from_rgb(6, 10, 8),
            Theme::Mono => Color32::from_rgb(250, 250, 250),
            Theme::Aqua => Color32::from_rgb(223, 231, 245),
        }
    }

    /// Warna aksen utama tema (untuk pratinjau & sorotan).
    pub fn accent(self) -> Color32 {
        match self {
            Theme::DarkNavy => Color32::from_rgb(77, 159, 255),
            Theme::Neon => Color32::from_rgb(0, 255, 157),
            Theme::Mono => Color32::from_rgb(17, 17, 17),
            Theme::Aqua => Color32::from_rgb(10, 132, 255),
        }
    }
}

/// Bangun `egui::Visuals` untuk tema yang diminta.
pub fn theme_visuals(theme: Theme) -> Visuals {
    match theme {
        Theme::DarkNavy => {
            let mut v = Visuals::dark();
            v.panel_fill = Color32::from_rgb(13, 26, 51);
            v.window_fill = Color32::from_rgb(10, 20, 40);
            v.extreme_bg_color = Color32::from_rgb(6, 13, 29);
            v.selection.bg_fill = Color32::from_rgb(30, 77, 158);
            v.selection.stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(77, 159, 255));
            v.widgets.hovered.bg_fill = Color32::from_rgb(20, 41, 77);
            v.widgets.active.bg_fill = Color32::from_rgb(30, 77, 158);
            v.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::WHITE);
            v.window_corner_radius = 8.0.into();
            for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
                w.corner_radius = 9.0.into();
            }
            v
        }
        Theme::Neon => {
            let mut v = Visuals::dark();
            v.panel_fill = Color32::from_rgb(8, 13, 10);
            v.window_fill = Color32::from_rgb(6, 10, 8);
            v.extreme_bg_color = Color32::from_rgb(4, 7, 5);
            // Seleksi: hijau gelap + teks neon (bukan neon menyilaukan full).
            v.selection.bg_fill = Color32::from_rgb(16, 46, 30);
            v.selection.stroke = eframe::egui::Stroke::new(1.5, Color32::from_rgb(0, 255, 157));
            v.widgets.hovered.bg_fill = Color32::from_rgb(15, 31, 22);
            v.widgets.hovered.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(0, 255, 157));
            v.widgets.active.bg_fill = Color32::from_rgb(20, 55, 35);
            v.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(0, 255, 157));
            v.window_corner_radius = 8.0.into();
            for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
                w.corner_radius = 8.0.into();
            }
            v
        }
        Theme::Mono => {
            let mut v = Visuals::light();
            v.panel_fill = Color32::WHITE;
            v.window_fill = Color32::from_rgb(250, 250, 250);
            v.extreme_bg_color = Color32::from_rgb(240, 240, 240);
            v.selection.bg_fill = Color32::from_rgb(17, 17, 17);
            v.selection.stroke = eframe::egui::Stroke::new(1.0, Color32::WHITE);
            v.widgets.hovered.bg_fill = Color32::from_rgb(235, 235, 235);
            v.widgets.active.bg_fill = Color32::from_rgb(17, 17, 17);
            v.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::WHITE);
            v.window_corner_radius = 2.0.into();
            for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
                w.corner_radius = 2.0.into();
            }
            v
        }
        Theme::Aqua => {
            let mut v = Visuals::light();
            v.panel_fill = Color32::from_rgb(246, 246, 248);
            v.window_fill = Color32::from_rgb(240, 242, 246);
            v.extreme_bg_color = Color32::from_rgb(223, 231, 245);
            v.selection.bg_fill = Color32::from_rgb(10, 132, 255);
            v.selection.stroke = eframe::egui::Stroke::new(1.0, Color32::WHITE);
            v.widgets.hovered.bg_fill = Color32::from_rgb(230, 235, 242);
            v.widgets.active.bg_fill = Color32::from_rgb(10, 132, 255);
            v.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::WHITE);
            v.window_corner_radius = 10.0.into();
            for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
                w.corner_radius = 10.0.into();
            }
            v
        }
    }
}

/// Terapkan tema ke context: visuals + ukuran font + padding tombol.
///
/// Ukuran font dibesarkan dari default egui agar sesuai prototype
/// (teks menu/panel yang terbaca, bukan kecil).
pub fn apply_theme(ctx: &eframe::egui::Context, theme: Theme) {
    ctx.set_theme(if matches!(theme, Theme::Mono | Theme::Aqua) {
        eframe::egui::Theme::Light
    } else {
        eframe::egui::Theme::Dark
    });
    ctx.set_visuals(theme_visuals(theme));
    ctx.all_styles_mut(|s| {
        use eframe::egui::{FontId, TextStyle};
        // Font lebih besar dari default egui.
        s.text_styles.insert(TextStyle::Body, FontId::proportional(16.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(15.0));
        s.text_styles.insert(TextStyle::Heading, FontId::proportional(22.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(15.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
        // Tombol lebih besar (seperti di prototype).
        s.spacing.button_padding = eframe::egui::vec2(12.0, 7.0);
        s.spacing.item_spacing = eframe::egui::vec2(8.0, 6.0);
        s.spacing.menu_margin = eframe::egui::Margin::same(8);
    });
}
/// Gambar pratinjau mini tema (mockup UI kecil) di dalam `rect`.
///
/// Menampilkan susunan v0.7.10 secara ringkas: menubar, toolbar dengan
/// satu tombol aktif (aksen), area gambar, dan statusbar — memakai
/// warna asli tema sehingga pengguna bisa melihat tampilannya.
pub fn draw_theme_preview(painter: &eframe::egui::Painter, rect: eframe::egui::Rect, theme: Theme) {
    let v = theme_visuals(theme);
    let accent = theme.accent();
    let r = 4.0;

    // Latar.
    painter.rect_filled(rect, r, v.window_fill);

    let bar_h = rect.height() * 0.14;
    let tool_h = rect.height() * 0.18;

    // Menubar.
    let menu = eframe::egui::Rect::from_min_size(rect.min, eframe::egui::vec2(rect.width(), bar_h));
    painter.rect_filled(menu, r, v.panel_fill);
    // Tiga "menu" kecil.
    for i in 0..3 {
        let w = rect.width() * 0.18;
        let x = rect.min.x + 6.0 + i as f32 * (w + 4.0);
        painter.rect_filled(
            eframe::egui::Rect::from_min_size(eframe::egui::pos2(x, menu.center().y - 3.0), eframe::egui::vec2(w, 6.0)),
            3.0,
            v.text_color().gamma_multiply(0.55),
        );
    }

    // Toolbar dengan satu tombol aktif beraksen.
    let tool = eframe::egui::Rect::from_min_size(
        eframe::egui::pos2(rect.min.x, rect.min.y + bar_h),
        eframe::egui::vec2(rect.width(), tool_h),
    );
    painter.rect_filled(tool, 0.0, v.extreme_bg_color);
    for i in 0..4 {
        let s = tool_h * 0.62;
        let x = rect.min.x + 6.0 + i as f32 * (s + 5.0);
        let y = tool.min.y + (tool_h - s) / 2.0;
        let b = eframe::egui::Rect::from_min_size(eframe::egui::pos2(x, y), eframe::egui::vec2(s, s));
        let fill = if i == 0 { accent } else { v.text_color().gamma_multiply(0.18) };
        painter.rect_filled(b, 3.0, fill);
    }

    // Area gambar (lebih gelap/terang mengikuti tema) + "foto" kecil.
    let content_top = rect.min.y + bar_h + tool_h;
    let content_h = rect.height() - (bar_h + tool_h) - bar_h;
    let content = eframe::egui::Rect::from_min_size(
        eframe::egui::pos2(rect.min.x, content_top),
        eframe::egui::vec2(rect.width(), content_h),
    );
    painter.rect_filled(content, 0.0, theme.bg());
    let pw = rect.width() * 0.44;
    let ph = content_h * 0.62;
    let photo = eframe::egui::Rect::from_center_size(content.center(), eframe::egui::vec2(pw, ph));
    // Gradasi sederhana sebagai pengganti foto.
    painter.rect_filled(photo, 2.0, accent.gamma_multiply(0.55));

    // Statusbar.
    let status = eframe::egui::Rect::from_min_size(
        eframe::egui::pos2(rect.min.x, rect.max.y - bar_h),
        eframe::egui::vec2(rect.width(), bar_h),
    );
    painter.rect_filled(status, r, v.panel_fill);
    painter.rect_filled(
        eframe::egui::Rect::from_min_size(
            eframe::egui::pos2(rect.min.x + 6.0, status.center().y - 2.5),
            eframe::egui::vec2(rect.width() * 0.4, 5.0),
        ),
        2.5,
        v.text_color().gamma_multiply(0.3),
    );
}
