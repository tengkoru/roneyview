//! Pilihan tema tampilan Roneyview.
//!
//! Bagian bertanda THEME-STUDIO digenerate oleh tool `theme-studio`
//! (src/bin/theme-studio). Jangan edit manual — edit lewat tool tersebut.
//! Bagian lain (panel_heading, draw_theme_preview) adalah kode tulis tangan.

use eframe::egui::{Color32, Visuals};
use serde::{Deserialize, Serialize};

// ===== THEME-STUDIO BEGIN theme_enum =====
/// Tema tampilan yang tersedia.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Theme {
    /// Biru navy gelap (biru terang sebagai aksen).
    DarkNavy,
    /// Hitam dengan aksen hijau neon.
    #[default]
    Neon,
    /// Monokrom terang (hitam-putih).
    Mono,
    /// Terang kebiruan gaya macOS dengan aksen biru.
    Aqua,
    // ===== TEMA BARU 0.8.29 (port palet dari salinan eksperimen) =====
    /// Terang hangat bergaya kertas (krem, cokelat tua, aksen oranye bata).
    Kertas,
    /// Gelap hangat ungu-jingga senja.
    Senja,
    /// Retro fosfor hijau di atas hitam.
    Terminal,
    /// Aksesibilitas: hitam-putih pekat, aksen kuning.
    KontrasTinggi,
    /// Pastel lembut pink/lavender muda.
    Sakura,
    /// Gelap kebiruan kehijauan (teal dalam).
    Samudra,
}
// ===== THEME-STUDIO END theme_enum =====

// ===== THEME-STUDIO BEGIN theme_impl =====
impl Theme {
    /// Nama tampilan tema (dipakai di menu & pengaturan, tidak diterjemahkan).
    pub fn name(self) -> &'static str {
        match self {
            Theme::DarkNavy => "Dark Navy",
            Theme::Neon => "Neon",
            Theme::Mono => "Mono",
            Theme::Aqua => "Aqua",
            Theme::Kertas => "Kertas",
            Theme::Senja => "Senja",
            Theme::Terminal => "Terminal",
            Theme::KontrasTinggi => "Kontras Tinggi",
            Theme::Sakura => "Sakura",
            Theme::Samudra => "Samudra",
        }
    }

    /// Semua tema, sesuai urutan tampil di menu.
    pub fn all() -> [Theme; 10] {
        [
            Theme::DarkNavy,
            Theme::Neon,
            Theme::Mono,
            Theme::Aqua,
            Theme::Kertas,
            Theme::Senja,
            Theme::Terminal,
            Theme::KontrasTinggi,
            Theme::Sakura,
            Theme::Samudra,
        ]
    }

    /// Warna latar area gambar (panel tengah).
    pub fn bg(self) -> Color32 {
        match self {
            Theme::DarkNavy => Color32::from_rgb(10, 20, 40),
            Theme::Neon => Color32::from_rgb(6, 10, 8),
            Theme::Mono => Color32::from_rgb(250, 250, 250),
            Theme::Aqua => Color32::from_rgb(206, 219, 240),
            Theme::Kertas => Color32::from_rgb(245, 238, 221),
            Theme::Senja => Color32::from_rgb(30, 21, 51),
            Theme::Terminal => Color32::from_rgb(1, 10, 3),
            Theme::KontrasTinggi => Color32::from_rgb(0, 0, 0),
            Theme::Sakura => Color32::from_rgb(253, 242, 244),
            Theme::Samudra => Color32::from_rgb(4, 38, 42),
        }
    }

    /// Warna aksen utama tema (untuk pratinjau & sorotan).
    pub fn accent(self) -> Color32 {
        match self {
            Theme::DarkNavy => Color32::from_rgb(77, 159, 255),
            Theme::Neon => Color32::from_rgb(0, 255, 157),
            Theme::Mono => Color32::from_rgb(17, 17, 17),
            Theme::Aqua => Color32::from_rgb(10, 132, 255),
            Theme::Kertas => Color32::from_rgb(194, 65, 12),
            Theme::Senja => Color32::from_rgb(251, 146, 60),
            Theme::Terminal => Color32::from_rgb(51, 255, 102),
            Theme::KontrasTinggi => Color32::from_rgb(255, 255, 0),
            Theme::Sakura => Color32::from_rgb(236, 72, 153),
            Theme::Samudra => Color32::from_rgb(45, 212, 191),
        }
    }

    /// Warna ikon toolbar: gelap di tema terang, terang di tema gelap.
    pub fn toolbar_icon(self) -> Color32 {
        match self {
            Theme::DarkNavy => Color32::from_rgb(225, 232, 245),
            Theme::Neon => Color32::from_rgb(225, 232, 245),
            Theme::Mono => Color32::from_rgb(35, 35, 35),
            Theme::Aqua => Color32::from_rgb(35, 35, 35),
            Theme::Kertas => Color32::from_rgb(62, 39, 35),
            Theme::Senja => Color32::from_rgb(245, 240, 235),
            Theme::Terminal => Color32::from_rgb(51, 255, 102),
            Theme::KontrasTinggi => Color32::from_rgb(255, 255, 255),
            Theme::Sakura => Color32::from_rgb(74, 43, 58),
            Theme::Samudra => Color32::from_rgb(204, 251, 241),
        }
    }
}
// ===== THEME-STUDIO END theme_impl =====

/// Campur dua warna: `mix(a, b, t)` bergerak `t` bagian dari `a` menuju `b`
/// (interpolasi per kanal di ruang gamma; cukup untuk derivasi warna UI).
/// Dipakai `theme_visuals` untuk menurunkan field Visuals yang belum bertema
/// dari palet tiap tema — helper ini TIDAK menambah nilai warna baru.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
}

// ===== THEME-STUDIO BEGIN theme_visuals =====
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
            v.widgets.active.bg_fill = Color32::from_rgb(30, 77, 158);
            v.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(255, 255, 255));
            v.window_corner_radius = 8.0.into();
            v.widgets.inactive.corner_radius = 9.0.into();
            v.widgets.hovered.corner_radius = 9.0.into();
            v.widgets.active.corner_radius = 9.0.into();
            // Pelengkap cakupan tema (0.8.27, revisi v2): field yang
            // sebelumnya masih default bawaan egui kini diturunkan dari
            // palet tema ini. v2 menaikkan campuran karena v1 (diam 5%)
            // terlalu dekat dengan latar panel di mata user — hanya Aqua
            // yang dinilai bagus. Ramp v2 (satu aturan untuk semua tema):
            // tombol diam = panel + aksen 20%; tombol terbuka = 30%;
            // hover = 35%; active Neon = 50% (agar hover<active tetap
            // terjaga; active tiga tema lain sudah lebih kuat dari hover
            // baru sehingga dipertahankan). Teks = warna ikon toolbar.
            // pemisah/garis = panel + teks 25%/30%; hyperlink & kursor = aksen.
            let panel = Color32::from_rgb(13, 26, 51);
            let window = Color32::from_rgb(10, 20, 40);
            let extreme = Color32::from_rgb(6, 13, 29);
            let accent = Color32::from_rgb(77, 159, 255);
            let text = Color32::from_rgb(225, 232, 245);
            v.widgets.noninteractive.bg_fill = window;
            v.widgets.noninteractive.weak_bg_fill = panel;
            v.widgets.noninteractive.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(panel, text, 0.25));
            v.widgets.noninteractive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.noninteractive.corner_radius = 8.0.into();
            v.widgets.inactive.weak_bg_fill = mix(panel, accent, 0.20);
            v.widgets.inactive.bg_fill = mix(panel, accent, 0.20);
            v.widgets.inactive.bg_stroke = eframe::egui::Stroke::NONE;
            v.widgets.inactive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.hovered.bg_fill = mix(panel, accent, 0.35);
            v.widgets.hovered.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(accent, panel, 0.5));
            v.widgets.hovered.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.active.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.weak_bg_fill = Color32::from_rgb(30, 77, 158);
            v.widgets.open.bg_fill = mix(panel, accent, 0.30);
            v.widgets.open.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.open.corner_radius = 8.0.into();
            v.hyperlink_color = accent;
            v.faint_bg_color = mix(window, panel, 0.5);
            v.text_edit_bg_color = Some(extreme);
            v.code_bg_color = mix(extreme, panel, 0.5);
            v.warn_fg_color = Color32::from_rgb(255, 176, 0);
            v.error_fg_color = Color32::from_rgb(255, 90, 90);
            v.window_stroke = eframe::egui::Stroke::new(1.0, mix(panel, text, 0.30));
            v.window_shadow = eframe::egui::Shadow {
                offset: [10, 20],
                blur: 15,
                spread: 0,
                color: Color32::from_black_alpha(110),
            };
            v.popup_shadow = eframe::egui::Shadow {
                offset: [6, 10],
                blur: 8,
                spread: 0,
                color: Color32::from_black_alpha(110),
            };
            v.menu_corner_radius = 8.0.into();
            v.text_cursor.stroke = eframe::egui::Stroke::new(2.0, accent);
            v
        }
        Theme::Neon => {
            let mut v = Visuals::dark();
            v.panel_fill = Color32::from_rgb(8, 13, 10);
            v.window_fill = Color32::from_rgb(6, 10, 8);
            v.extreme_bg_color = Color32::from_rgb(4, 7, 5);
            v.selection.bg_fill = Color32::from_rgb(16, 46, 30);
            v.selection.stroke = eframe::egui::Stroke::new(1.5, Color32::from_rgb(0, 255, 157));
            v.widgets.hovered.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(0, 255, 157));
            v.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(0, 255, 157));
            v.window_corner_radius = 8.0.into();
            v.widgets.inactive.corner_radius = 8.0.into();
            v.widgets.hovered.corner_radius = 8.0.into();
            v.widgets.active.corner_radius = 8.0.into();
            // Pelengkap cakupan tema (0.8.27): lihat aturan derivasi di
            // lengan DarkNavy — palet yang dipakai di sini milik Neon.
            let panel = Color32::from_rgb(8, 13, 10);
            let window = Color32::from_rgb(6, 10, 8);
            let extreme = Color32::from_rgb(4, 7, 5);
            let accent = Color32::from_rgb(0, 255, 157);
            let text = Color32::from_rgb(225, 232, 245);
            v.widgets.noninteractive.bg_fill = window;
            v.widgets.noninteractive.weak_bg_fill = panel;
            v.widgets.noninteractive.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(panel, text, 0.25));
            v.widgets.noninteractive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.noninteractive.corner_radius = 8.0.into();
            v.widgets.inactive.weak_bg_fill = mix(panel, accent, 0.20);
            v.widgets.inactive.bg_fill = mix(panel, accent, 0.20);
            v.widgets.inactive.bg_stroke = eframe::egui::Stroke::NONE;
            v.widgets.inactive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.hovered.bg_fill = mix(panel, accent, 0.35);
            v.widgets.hovered.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(accent, panel, 0.5));
            // fg_stroke hovered Neon sudah aksen sejak awal; dipertahankan.
            v.widgets.active.bg_fill = mix(panel, accent, 0.50);
            v.widgets.active.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.weak_bg_fill = mix(panel, accent, 0.50);
            v.widgets.open.bg_fill = mix(panel, accent, 0.30);
            v.widgets.open.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.open.corner_radius = 8.0.into();
            v.hyperlink_color = accent;
            v.faint_bg_color = mix(window, panel, 0.5);
            v.text_edit_bg_color = Some(extreme);
            v.code_bg_color = mix(extreme, panel, 0.5);
            v.warn_fg_color = Color32::from_rgb(255, 176, 0);
            v.error_fg_color = Color32::from_rgb(255, 90, 90);
            v.window_stroke = eframe::egui::Stroke::new(1.0, mix(panel, text, 0.30));
            v.window_shadow = eframe::egui::Shadow {
                offset: [10, 20],
                blur: 15,
                spread: 0,
                color: Color32::from_black_alpha(110),
            };
            v.popup_shadow = eframe::egui::Shadow {
                offset: [6, 10],
                blur: 8,
                spread: 0,
                color: Color32::from_black_alpha(110),
            };
            v.menu_corner_radius = 8.0.into();
            v.text_cursor.stroke = eframe::egui::Stroke::new(2.0, accent);
            v
        }
        Theme::Mono => {
            let mut v = Visuals::light();
            v.panel_fill = Color32::from_rgb(255, 255, 255);
            v.window_fill = Color32::from_rgb(250, 250, 250);
            v.extreme_bg_color = Color32::from_rgb(240, 240, 240);
            v.selection.bg_fill = Color32::from_rgb(17, 17, 17);
            v.selection.stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(255, 255, 255));
            v.widgets.active.bg_fill = Color32::from_rgb(17, 17, 17);
            v.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(255, 255, 255));
            v.window_corner_radius = 2.0.into();
            v.widgets.inactive.corner_radius = 2.0.into();
            v.widgets.hovered.corner_radius = 2.0.into();
            v.widgets.active.corner_radius = 2.0.into();
            // Pelengkap cakupan tema (0.8.27): lihat aturan derivasi di
            // lengan DarkNavy — palet yang dipakai di sini milik Mono.
            let panel = Color32::from_rgb(255, 255, 255);
            let window = Color32::from_rgb(250, 250, 250);
            let extreme = Color32::from_rgb(240, 240, 240);
            let accent = Color32::from_rgb(17, 17, 17);
            let text = Color32::from_rgb(35, 35, 35);
            v.widgets.noninteractive.bg_fill = window;
            v.widgets.noninteractive.weak_bg_fill = panel;
            v.widgets.noninteractive.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(panel, text, 0.25));
            v.widgets.noninteractive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.noninteractive.corner_radius = 2.0.into();
            v.widgets.inactive.weak_bg_fill = mix(panel, accent, 0.20);
            v.widgets.inactive.bg_fill = mix(panel, accent, 0.20);
            v.widgets.inactive.bg_stroke = eframe::egui::Stroke::NONE;
            v.widgets.inactive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.hovered.bg_fill = mix(panel, accent, 0.35);
            v.widgets.hovered.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(accent, panel, 0.5));
            v.widgets.hovered.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.active.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.weak_bg_fill = Color32::from_rgb(17, 17, 17);
            v.widgets.open.bg_fill = mix(panel, accent, 0.30);
            v.widgets.open.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.open.corner_radius = 2.0.into();
            v.hyperlink_color = accent;
            v.faint_bg_color = mix(window, panel, 0.5);
            v.text_edit_bg_color = Some(extreme);
            v.code_bg_color = mix(extreme, panel, 0.5);
            v.warn_fg_color = Color32::from_rgb(200, 110, 0);
            v.error_fg_color = Color32::from_rgb(200, 0, 0);
            v.window_stroke = eframe::egui::Stroke::new(1.0, mix(panel, text, 0.30));
            v.window_shadow = eframe::egui::Shadow {
                offset: [10, 20],
                blur: 15,
                spread: 0,
                color: Color32::from_black_alpha(25),
            };
            v.popup_shadow = eframe::egui::Shadow {
                offset: [6, 10],
                blur: 8,
                spread: 0,
                color: Color32::from_black_alpha(25),
            };
            v.menu_corner_radius = 2.0.into();
            v.text_cursor.stroke = eframe::egui::Stroke::new(2.0, accent);
            v
        }
        Theme::Aqua => {
            let mut v = Visuals::light();
            v.panel_fill = Color32::from_rgb(255, 255, 255);
            v.window_fill = Color32::from_rgb(240, 242, 246);
            v.extreme_bg_color = Color32::from_rgb(206, 219, 240);
            v.selection.bg_fill = Color32::from_rgb(10, 132, 255);
            v.selection.stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(255, 255, 255));
            v.widgets.active.bg_fill = Color32::from_rgb(10, 132, 255);
            v.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(255, 255, 255));
            v.window_corner_radius = 10.0.into();
            v.widgets.inactive.corner_radius = 10.0.into();
            v.widgets.hovered.corner_radius = 10.0.into();
            v.widgets.active.corner_radius = 10.0.into();
            // Pelengkap cakupan tema (0.8.27): lihat aturan derivasi di
            // lengan DarkNavy — palet yang dipakai di sini milik Aqua,
            // KECUALI idle/open/hover: latar panel edit adalah theme.bg()
            // Aqua = extreme (206,219,240), BUKAN panel putih. Derivasi
            // dari panel runtuh ke latar itu (v2 awal: idle (206,230,255)
            // vs latar rasio 1,09 — lebih buruk dari v1 (243,249,255),
            // rasio 1,32). Karena itu ketiganya diturunkan dari extreme.
            let panel = Color32::from_rgb(255, 255, 255);
            let window = Color32::from_rgb(240, 242, 246);
            let extreme = Color32::from_rgb(206, 219, 240);
            let accent = Color32::from_rgb(10, 132, 255);
            let text = Color32::from_rgb(35, 35, 35);
            v.widgets.noninteractive.bg_fill = window;
            v.widgets.noninteractive.weak_bg_fill = panel;
            v.widgets.noninteractive.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(panel, text, 0.25));
            v.widgets.noninteractive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.noninteractive.corner_radius = 10.0.into();
            v.widgets.inactive.weak_bg_fill = mix(extreme, accent, 0.35);
            v.widgets.inactive.bg_fill = mix(extreme, accent, 0.35);
            v.widgets.inactive.bg_stroke = eframe::egui::Stroke::NONE;
            v.widgets.inactive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.hovered.bg_fill = mix(extreme, accent, 0.50);
            v.widgets.hovered.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(accent, panel, 0.5));
            v.widgets.hovered.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.active.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.weak_bg_fill = Color32::from_rgb(10, 132, 255);
            v.widgets.open.bg_fill = mix(extreme, accent, 0.45);
            v.widgets.open.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.open.corner_radius = 10.0.into();
            v.hyperlink_color = accent;
            v.faint_bg_color = mix(window, panel, 0.5);
            v.text_edit_bg_color = Some(extreme);
            v.code_bg_color = mix(extreme, panel, 0.5);
            v.warn_fg_color = Color32::from_rgb(200, 110, 0);
            v.error_fg_color = Color32::from_rgb(200, 0, 0);
            v.window_stroke = eframe::egui::Stroke::new(1.0, mix(panel, text, 0.30));
            v.window_shadow = eframe::egui::Shadow {
                offset: [10, 20],
                blur: 15,
                spread: 0,
                color: Color32::from_black_alpha(25),
            };
            v.popup_shadow = eframe::egui::Shadow {
                offset: [6, 10],
                blur: 8,
                spread: 0,
                color: Color32::from_black_alpha(25),
            };
            v.menu_corner_radius = 10.0.into();
            v.text_cursor.stroke = eframe::egui::Stroke::new(2.0, accent);
            v
        }
        // ===== TEMA BARU 0.8.29: palet port dari salinan eksperimen,
        // derivasi cakupan penuh 0.8.27 (BUKAN theme_visuals eksperimen
        // mentah — cakupannya parsial era lama). Aturan derivasi sama
        // seperti lengan DarkNavy; langkah idle/terbuka/hover dipilih
        // per tema agar lolos penjaga kontras v2 terhadap latar ASLI
        // panel edit (theme.bg(), bukan panel_fill) — lihat komentar
        // per lengan. Nilai keempat tema lama tidak diubah.
        Theme::Kertas => {
            let mut v = Visuals::light();
            v.panel_fill = Color32::from_rgb(253, 248, 236);
            v.window_fill = Color32::from_rgb(250, 243, 224);
            v.extreme_bg_color = Color32::from_rgb(237, 227, 200);
            v.selection.bg_fill = Color32::from_rgb(254, 215, 170);
            v.selection.stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(194, 65, 12));
            v.widgets.active.bg_fill = Color32::from_rgb(194, 65, 12);
            v.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(255, 255, 255));
            v.window_corner_radius = 6.0.into();
            v.widgets.inactive.corner_radius = 6.0.into();
            v.widgets.hovered.corner_radius = 6.0.into();
            v.widgets.active.corner_radius = 6.0.into();
            // Kertas terang: panel nyaris-putih + aksen oranye bata yang
            // cukup gelap — langkah 20% runtuh ke latar (rasio vs bg
            // hanya ~1,23, vs panel ~1,32, di bawah ambang 1,35), jadi
            // dipakai 35/45/50 (diam (232,184,158), rasio vs bg 1,53).
            let panel = Color32::from_rgb(253, 248, 236);
            let window = Color32::from_rgb(250, 243, 224);
            let extreme = Color32::from_rgb(237, 227, 200);
            let accent = Color32::from_rgb(194, 65, 12);
            let text = Color32::from_rgb(62, 39, 35);
            v.widgets.noninteractive.bg_fill = window;
            v.widgets.noninteractive.weak_bg_fill = panel;
            v.widgets.noninteractive.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(panel, text, 0.25));
            v.widgets.noninteractive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.noninteractive.corner_radius = 6.0.into();
            v.widgets.inactive.weak_bg_fill = mix(panel, accent, 0.35);
            v.widgets.inactive.bg_fill = mix(panel, accent, 0.35);
            v.widgets.inactive.bg_stroke = eframe::egui::Stroke::NONE;
            v.widgets.inactive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.hovered.bg_fill = mix(panel, accent, 0.50);
            v.widgets.hovered.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(accent, panel, 0.5));
            v.widgets.hovered.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.active.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.weak_bg_fill = Color32::from_rgb(194, 65, 12);
            v.widgets.open.bg_fill = mix(panel, accent, 0.45);
            v.widgets.open.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.open.corner_radius = 6.0.into();
            v.hyperlink_color = accent;
            v.faint_bg_color = mix(window, panel, 0.5);
            v.text_edit_bg_color = Some(extreme);
            v.code_bg_color = mix(extreme, panel, 0.5);
            v.warn_fg_color = Color32::from_rgb(200, 110, 0);
            v.error_fg_color = Color32::from_rgb(200, 0, 0);
            v.window_stroke = eframe::egui::Stroke::new(1.0, mix(panel, text, 0.30));
            v.window_shadow = eframe::egui::Shadow {
                offset: [10, 20],
                blur: 15,
                spread: 0,
                color: Color32::from_black_alpha(25),
            };
            v.popup_shadow = eframe::egui::Shadow {
                offset: [6, 10],
                blur: 8,
                spread: 0,
                color: Color32::from_black_alpha(25),
            };
            v.menu_corner_radius = 6.0.into();
            v.text_cursor.stroke = eframe::egui::Stroke::new(2.0, accent);
            v
        }
        Theme::Senja => {
            let mut v = Visuals::dark();
            v.panel_fill = Color32::from_rgb(45, 33, 69);
            v.window_fill = Color32::from_rgb(36, 26, 56);
            v.extreme_bg_color = Color32::from_rgb(23, 16, 42);
            v.selection.bg_fill = Color32::from_rgb(109, 40, 217);
            v.selection.stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(251, 146, 60));
            v.widgets.active.bg_fill = Color32::from_rgb(234, 88, 12);
            v.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(255, 255, 255));
            v.window_corner_radius = 10.0.into();
            v.widgets.inactive.corner_radius = 10.0.into();
            v.widgets.hovered.corner_radius = 10.0.into();
            v.widgets.active.corner_radius = 10.0.into();
            // Senja gelap: langkah standar 20/30/35 cukup (diam
            // (86,56,67), rasio vs bg 1,72, vs panel 1,44).
            let panel = Color32::from_rgb(45, 33, 69);
            let window = Color32::from_rgb(36, 26, 56);
            let extreme = Color32::from_rgb(23, 16, 42);
            let accent = Color32::from_rgb(251, 146, 60);
            let text = Color32::from_rgb(245, 240, 235);
            v.widgets.noninteractive.bg_fill = window;
            v.widgets.noninteractive.weak_bg_fill = panel;
            v.widgets.noninteractive.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(panel, text, 0.25));
            v.widgets.noninteractive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.noninteractive.corner_radius = 10.0.into();
            v.widgets.inactive.weak_bg_fill = mix(panel, accent, 0.20);
            v.widgets.inactive.bg_fill = mix(panel, accent, 0.20);
            v.widgets.inactive.bg_stroke = eframe::egui::Stroke::NONE;
            v.widgets.inactive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.hovered.bg_fill = mix(panel, accent, 0.35);
            v.widgets.hovered.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(accent, panel, 0.5));
            v.widgets.hovered.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.active.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.weak_bg_fill = Color32::from_rgb(234, 88, 12);
            v.widgets.open.bg_fill = mix(panel, accent, 0.30);
            v.widgets.open.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.open.corner_radius = 10.0.into();
            v.hyperlink_color = accent;
            v.faint_bg_color = mix(window, panel, 0.5);
            v.text_edit_bg_color = Some(extreme);
            v.code_bg_color = mix(extreme, panel, 0.5);
            v.warn_fg_color = Color32::from_rgb(255, 176, 0);
            v.error_fg_color = Color32::from_rgb(255, 90, 90);
            v.window_stroke = eframe::egui::Stroke::new(1.0, mix(panel, text, 0.30));
            v.window_shadow = eframe::egui::Shadow {
                offset: [10, 20],
                blur: 15,
                spread: 0,
                color: Color32::from_black_alpha(110),
            };
            v.popup_shadow = eframe::egui::Shadow {
                offset: [6, 10],
                blur: 8,
                spread: 0,
                color: Color32::from_black_alpha(110),
            };
            v.menu_corner_radius = 10.0.into();
            v.text_cursor.stroke = eframe::egui::Stroke::new(2.0, accent);
            v
        }
        Theme::Terminal => {
            let mut v = Visuals::dark();
            v.panel_fill = Color32::from_rgb(3, 16, 7);
            v.window_fill = Color32::from_rgb(0, 0, 0);
            v.extreme_bg_color = Color32::from_rgb(0, 0, 0);
            v.selection.bg_fill = Color32::from_rgb(0, 51, 17);
            v.selection.stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(51, 255, 102));
            v.widgets.active.bg_fill = Color32::from_rgb(51, 255, 102);
            v.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(0, 0, 0));
            v.window_corner_radius = 0.0.into();
            v.widgets.inactive.corner_radius = 0.0.into();
            v.widgets.hovered.corner_radius = 0.0.into();
            v.widgets.active.corner_radius = 0.0.into();
            // Terminal gelap: langkah standar 20/30/35 cukup (diam
            // (13,64,26), rasio vs bg 1,71, vs panel 1,65). Monospace
            // penuh diatur di apply_theme, seperti Neon.
            let panel = Color32::from_rgb(3, 16, 7);
            let window = Color32::from_rgb(0, 0, 0);
            let extreme = Color32::from_rgb(0, 0, 0);
            let accent = Color32::from_rgb(51, 255, 102);
            let text = Color32::from_rgb(51, 255, 102);
            v.widgets.noninteractive.bg_fill = window;
            v.widgets.noninteractive.weak_bg_fill = panel;
            v.widgets.noninteractive.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(panel, text, 0.25));
            v.widgets.noninteractive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.noninteractive.corner_radius = 0.0.into();
            v.widgets.inactive.weak_bg_fill = mix(panel, accent, 0.20);
            v.widgets.inactive.bg_fill = mix(panel, accent, 0.20);
            v.widgets.inactive.bg_stroke = eframe::egui::Stroke::NONE;
            v.widgets.inactive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.hovered.bg_fill = mix(panel, accent, 0.35);
            v.widgets.hovered.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(accent, panel, 0.5));
            v.widgets.hovered.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.active.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.weak_bg_fill = Color32::from_rgb(51, 255, 102);
            v.widgets.open.bg_fill = mix(panel, accent, 0.30);
            v.widgets.open.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.open.corner_radius = 0.0.into();
            v.hyperlink_color = accent;
            v.faint_bg_color = mix(window, panel, 0.5);
            v.text_edit_bg_color = Some(extreme);
            v.code_bg_color = mix(extreme, panel, 0.5);
            v.warn_fg_color = Color32::from_rgb(255, 176, 0);
            v.error_fg_color = Color32::from_rgb(255, 90, 90);
            v.window_stroke = eframe::egui::Stroke::new(1.0, mix(panel, text, 0.30));
            v.window_shadow = eframe::egui::Shadow {
                offset: [10, 20],
                blur: 15,
                spread: 0,
                color: Color32::from_black_alpha(110),
            };
            v.popup_shadow = eframe::egui::Shadow {
                offset: [6, 10],
                blur: 8,
                spread: 0,
                color: Color32::from_black_alpha(110),
            };
            v.menu_corner_radius = 0.0.into();
            v.text_cursor.stroke = eframe::egui::Stroke::new(2.0, accent);
            v
        }
        Theme::KontrasTinggi => {
            let mut v = Visuals::dark();
            v.panel_fill = Color32::from_rgb(0, 0, 0);
            v.window_fill = Color32::from_rgb(0, 0, 0);
            v.extreme_bg_color = Color32::from_rgb(0, 0, 0);
            v.selection.bg_fill = Color32::from_rgb(255, 255, 0);
            v.selection.stroke = eframe::egui::Stroke::new(1.5, Color32::from_rgb(255, 255, 0));
            v.widgets.active.bg_fill = Color32::from_rgb(255, 255, 0);
            v.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(0, 0, 0));
            v.window_corner_radius = 0.0.into();
            v.widgets.inactive.corner_radius = 0.0.into();
            v.widgets.hovered.corner_radius = 0.0.into();
            v.widgets.active.corner_radius = 0.0.into();
            // Kontras Tinggi: panel = bg = hitam, jadi langkah standar
            // 20/30/35 berlaku penuh (diam (51,51,0), rasio vs bg 1,61).
            // Garis jendela mengikuti aturan derivasi (panel + teks 30%)
            // seperti tema lain, bukan putih eksperimen — keseragaman
            // aturan di atas karakter satu field.
            let panel = Color32::from_rgb(0, 0, 0);
            let window = Color32::from_rgb(0, 0, 0);
            let extreme = Color32::from_rgb(0, 0, 0);
            let accent = Color32::from_rgb(255, 255, 0);
            let text = Color32::from_rgb(255, 255, 255);
            v.widgets.noninteractive.bg_fill = window;
            v.widgets.noninteractive.weak_bg_fill = panel;
            v.widgets.noninteractive.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(panel, text, 0.25));
            v.widgets.noninteractive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.noninteractive.corner_radius = 0.0.into();
            v.widgets.inactive.weak_bg_fill = mix(panel, accent, 0.20);
            v.widgets.inactive.bg_fill = mix(panel, accent, 0.20);
            v.widgets.inactive.bg_stroke = eframe::egui::Stroke::NONE;
            v.widgets.inactive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.hovered.bg_fill = mix(panel, accent, 0.35);
            v.widgets.hovered.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(accent, panel, 0.5));
            v.widgets.hovered.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.active.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.weak_bg_fill = Color32::from_rgb(255, 255, 0);
            v.widgets.open.bg_fill = mix(panel, accent, 0.30);
            v.widgets.open.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.open.corner_radius = 0.0.into();
            v.hyperlink_color = accent;
            v.faint_bg_color = mix(window, panel, 0.5);
            v.text_edit_bg_color = Some(extreme);
            v.code_bg_color = mix(extreme, panel, 0.5);
            v.warn_fg_color = Color32::from_rgb(255, 176, 0);
            v.error_fg_color = Color32::from_rgb(255, 90, 90);
            v.window_stroke = eframe::egui::Stroke::new(1.0, mix(panel, text, 0.30));
            v.window_shadow = eframe::egui::Shadow {
                offset: [10, 20],
                blur: 15,
                spread: 0,
                color: Color32::from_black_alpha(110),
            };
            v.popup_shadow = eframe::egui::Shadow {
                offset: [6, 10],
                blur: 8,
                spread: 0,
                color: Color32::from_black_alpha(110),
            };
            v.menu_corner_radius = 0.0.into();
            v.text_cursor.stroke = eframe::egui::Stroke::new(2.0, accent);
            v
        }
        Theme::Sakura => {
            let mut v = Visuals::light();
            v.panel_fill = Color32::from_rgb(255, 255, 255);
            v.window_fill = Color32::from_rgb(255, 245, 247);
            v.extreme_bg_color = Color32::from_rgb(253, 232, 236);
            v.selection.bg_fill = Color32::from_rgb(251, 207, 232);
            v.selection.stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(236, 72, 153));
            v.widgets.active.bg_fill = Color32::from_rgb(236, 72, 153);
            v.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(255, 255, 255));
            v.window_corner_radius = 12.0.into();
            v.widgets.inactive.corner_radius = 12.0.into();
            v.widgets.hovered.corner_radius = 12.0.into();
            v.widgets.active.corner_radius = 12.0.into();
            // Sakura terang, kasus yang sama persis seperti Aqua: panel
            // putih sementara latar panel edit adalah theme.bg()
            // (253,242,244) yang nyaris-putih — derivasi panel 20%
            // runtuh ke latar itu (rasio hanya ~1,19). Karena itu
            // diam/terbuka/hover diturunkan dari extreme dengan langkah
            // Aqua (35/45/50): diam (247,176,207), rasio vs bg 1,61.
            let panel = Color32::from_rgb(255, 255, 255);
            let window = Color32::from_rgb(255, 245, 247);
            let extreme = Color32::from_rgb(253, 232, 236);
            let accent = Color32::from_rgb(236, 72, 153);
            let text = Color32::from_rgb(74, 43, 58);
            v.widgets.noninteractive.bg_fill = window;
            v.widgets.noninteractive.weak_bg_fill = panel;
            v.widgets.noninteractive.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(panel, text, 0.25));
            v.widgets.noninteractive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.noninteractive.corner_radius = 12.0.into();
            v.widgets.inactive.weak_bg_fill = mix(extreme, accent, 0.35);
            v.widgets.inactive.bg_fill = mix(extreme, accent, 0.35);
            v.widgets.inactive.bg_stroke = eframe::egui::Stroke::NONE;
            v.widgets.inactive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.hovered.bg_fill = mix(extreme, accent, 0.50);
            v.widgets.hovered.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(accent, panel, 0.5));
            v.widgets.hovered.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.active.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.weak_bg_fill = Color32::from_rgb(236, 72, 153);
            v.widgets.open.bg_fill = mix(extreme, accent, 0.45);
            v.widgets.open.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.open.corner_radius = 12.0.into();
            v.hyperlink_color = accent;
            v.faint_bg_color = mix(window, panel, 0.5);
            v.text_edit_bg_color = Some(extreme);
            v.code_bg_color = mix(extreme, panel, 0.5);
            v.warn_fg_color = Color32::from_rgb(200, 110, 0);
            v.error_fg_color = Color32::from_rgb(200, 0, 0);
            v.window_stroke = eframe::egui::Stroke::new(1.0, mix(panel, text, 0.30));
            v.window_shadow = eframe::egui::Shadow {
                offset: [10, 20],
                blur: 15,
                spread: 0,
                color: Color32::from_black_alpha(25),
            };
            v.popup_shadow = eframe::egui::Shadow {
                offset: [6, 10],
                blur: 8,
                spread: 0,
                color: Color32::from_black_alpha(25),
            };
            v.menu_corner_radius = 12.0.into();
            v.text_cursor.stroke = eframe::egui::Stroke::new(2.0, accent);
            v
        }
        Theme::Samudra => {
            let mut v = Visuals::dark();
            v.panel_fill = Color32::from_rgb(10, 52, 56);
            v.window_fill = Color32::from_rgb(6, 42, 46);
            v.extreme_bg_color = Color32::from_rgb(4, 32, 36);
            v.selection.bg_fill = Color32::from_rgb(19, 78, 74);
            v.selection.stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(45, 212, 191));
            v.widgets.active.bg_fill = Color32::from_rgb(13, 148, 136);
            v.widgets.active.fg_stroke = eframe::egui::Stroke::new(1.0, Color32::from_rgb(255, 255, 255));
            v.window_corner_radius = 8.0.into();
            v.widgets.inactive.corner_radius = 8.0.into();
            v.widgets.hovered.corner_radius = 8.0.into();
            v.widgets.active.corner_radius = 8.0.into();
            // Samudra gelap: langkah standar 20/30/35 cukup (diam
            // (17,84,83), rasio vs bg 1,86, vs panel 1,57).
            let panel = Color32::from_rgb(10, 52, 56);
            let window = Color32::from_rgb(6, 42, 46);
            let extreme = Color32::from_rgb(4, 32, 36);
            let accent = Color32::from_rgb(45, 212, 191);
            let text = Color32::from_rgb(204, 251, 241);
            v.widgets.noninteractive.bg_fill = window;
            v.widgets.noninteractive.weak_bg_fill = panel;
            v.widgets.noninteractive.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(panel, text, 0.25));
            v.widgets.noninteractive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.noninteractive.corner_radius = 8.0.into();
            v.widgets.inactive.weak_bg_fill = mix(panel, accent, 0.20);
            v.widgets.inactive.bg_fill = mix(panel, accent, 0.20);
            v.widgets.inactive.bg_stroke = eframe::egui::Stroke::NONE;
            v.widgets.inactive.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.hovered.bg_fill = mix(panel, accent, 0.35);
            v.widgets.hovered.bg_stroke =
                eframe::egui::Stroke::new(1.0, mix(accent, panel, 0.5));
            v.widgets.hovered.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.active.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.weak_bg_fill = Color32::from_rgb(13, 148, 136);
            v.widgets.open.bg_fill = mix(panel, accent, 0.30);
            v.widgets.open.bg_stroke = eframe::egui::Stroke::new(1.0, accent);
            v.widgets.open.fg_stroke = eframe::egui::Stroke::new(1.0, text);
            v.widgets.open.corner_radius = 8.0.into();
            v.hyperlink_color = accent;
            v.faint_bg_color = mix(window, panel, 0.5);
            v.text_edit_bg_color = Some(extreme);
            v.code_bg_color = mix(extreme, panel, 0.5);
            v.warn_fg_color = Color32::from_rgb(255, 176, 0);
            v.error_fg_color = Color32::from_rgb(255, 90, 90);
            v.window_stroke = eframe::egui::Stroke::new(1.0, mix(panel, text, 0.30));
            v.window_shadow = eframe::egui::Shadow {
                offset: [10, 20],
                blur: 15,
                spread: 0,
                color: Color32::from_black_alpha(110),
            };
            v.popup_shadow = eframe::egui::Shadow {
                offset: [6, 10],
                blur: 8,
                spread: 0,
                color: Color32::from_black_alpha(110),
            };
            v.menu_corner_radius = 8.0.into();
            v.text_cursor.stroke = eframe::egui::Stroke::new(2.0, accent);
            v
        }
    }
}
// ===== THEME-STUDIO END theme_visuals =====

// ===== THEME-STUDIO BEGIN apply_theme =====
/// Terapkan tema ke context: visuals + ukuran font + padding tombol.
///
/// Ukuran font dibesarkan dari default egui agar sesuai prototype
/// (teks menu/panel yang terbaca, bukan kecil).
pub fn apply_theme(ctx: &eframe::egui::Context, theme: Theme) {
    ctx.set_theme(if matches!(theme, Theme::Mono | Theme::Aqua | Theme::Kertas | Theme::Sakura) {
        eframe::egui::Theme::Light
    } else {
        eframe::egui::Theme::Dark
    });
    ctx.set_visuals(theme_visuals(theme));
    ctx.all_styles_mut(|s| {
        use eframe::egui::{FontId, TextStyle};
        // Tema Neon memakai monospace untuk seluruh UI (sesuai prototype F2-Neon).
        // Font monospace bawaan egui adalah Hack; tidak ada font tambahan
        // yang dibundel dan tidak ada perubahan ukuran biner.
        // Terminal juga monospace penuh (karakter retro eksperimen).
        let mono_all = matches!(theme, Theme::Neon | Theme::Terminal);
        let font = |size: f32| {
            if mono_all {
                FontId::monospace(size)
            } else {
                FontId::proportional(size)
            }
        };
        s.text_styles.insert(TextStyle::Body, font(16.0));
        s.text_styles.insert(TextStyle::Button, font(15.0));
        s.text_styles.insert(TextStyle::Heading, font(22.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(15.0));
        s.text_styles.insert(TextStyle::Small, font(12.0));
        // Tombol lebih besar (seperti di prototype).
        s.spacing.button_padding = eframe::egui::vec2(12.0, 7.0);
        s.spacing.item_spacing = eframe::egui::vec2(8.0, 6.0);
        s.spacing.menu_margin = eframe::egui::Margin::same(8);
    });
}
// ===== THEME-STUDIO END apply_theme =====

/// Heading kecil untuk panel perkakas/pengaturan, gaya per-tema mengikuti prototype.
///
/// Teks di-uppercase, ~11px. egui tidak punya letter-spacing, jadi tracking
/// 0.8–2px dari prototype tidak bisa direproduksi persis.
pub fn panel_heading(ui: &mut eframe::egui::Ui, theme: Theme, text: &str) {
    use eframe::egui::{epaint::text::LayoutJob, pos2, vec2, Color32, FontId, RichText, Sense, Stroke};
    let upper = text.to_uppercase();
    match theme {
        Theme::DarkNavy => {
            ui.label(RichText::new(upper).size(11.0).color(Color32::from_rgb(77, 106, 148)));
        }
        Theme::Neon => {
            ui.label(
                RichText::new(upper)
                    .size(11.0)
                    .monospace()
                    .color(Color32::from_rgb(63, 122, 85)),
            );
        }
        Theme::Mono => {
            let color = Color32::BLACK;
            let rect = ui.label(RichText::new(upper).size(11.0).color(color)).rect;
            let y = rect.bottom() + 2.0;
            ui.painter()
                .line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(2.0_f32, color));
        }
        Theme::Aqua => {
            // Bold palsu: egui tidak membundel font bold, jadi galley digambar
            // dua kali dengan offset horizontal kecil.
            let color = Color32::from_rgb(110, 110, 115);
            let job = LayoutJob::simple(upper, FontId::proportional(11.0), color, f32::INFINITY);
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
            let painter = ui.painter();
            painter.galley(rect.min, galley.clone(), color);
            painter.galley(rect.min + vec2(0.75, 0.0), galley, color);
        }
        // Tema kustom (dari theme-studio): gaya netral mengikuti basis gelap/terang.
        _ => {
            let color = ui.visuals().text_color().gamma_multiply(0.6);
            ui.label(RichText::new(upper).size(11.0).color(color));
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(c: Color32) -> (u8, u8, u8) {
        (c.r(), c.g(), c.b())
    }

    #[test]
    fn pelengkap_tema_mengikuti_palet_dan_bukan_default_egui() {
        // (tema, tombol-diam, tombol-terbuka, teks, garis-jendela, strip-halus)
        let kasus = [
            (
                Theme::DarkNavy,
                (26, 53, 92),
                (32, 66, 112),
                (225, 232, 245),
                (77, 88, 109),
                (12, 23, 46),
            ),
            (
                Theme::Neon,
                (6, 61, 39),
                (6, 86, 54),
                (225, 232, 245),
                (73, 79, 81),
                (7, 12, 9),
            ),
            (
                Theme::Mono,
                (207, 207, 207),
                (184, 184, 184),
                (35, 35, 35),
                (189, 189, 189),
                (253, 253, 253),
            ),
            (
                Theme::Aqua,
                (137, 189, 245),
                (118, 180, 247),
                (35, 35, 35),
                (189, 189, 189),
                (248, 249, 251),
            ),
            // Tema baru 0.8.29 (langkah per tema lihat komentar lengan).
            (
                Theme::Kertas,
                (232, 184, 158),
                (226, 166, 135),
                (62, 39, 35),
                (196, 185, 176),
                (252, 246, 230),
            ),
            (
                Theme::Senja,
                (86, 56, 67),
                (107, 67, 66),
                (245, 240, 235),
                (105, 95, 119),
                (41, 30, 63),
            ),
            (
                Theme::Terminal,
                (13, 64, 26),
                (17, 88, 36),
                (51, 255, 102),
                (17, 88, 36),
                (2, 8, 4),
            ),
            (
                Theme::KontrasTinggi,
                (51, 51, 0),
                (77, 77, 0),
                (255, 255, 255),
                (77, 77, 77),
                (0, 0, 0),
            ),
            (
                Theme::Sakura,
                (247, 176, 207),
                (245, 160, 199),
                (74, 43, 58),
                (201, 191, 196),
                (255, 250, 251),
            ),
            (
                Theme::Samudra,
                (17, 84, 83),
                (21, 100, 97),
                (204, 251, 241),
                (68, 112, 112),
                (8, 47, 51),
            ),
        ];
        for (tema, diam, terbuka, teks, garis, halus) in kasus {
            let v = theme_visuals(tema);
            assert_eq!(rgb(v.widgets.inactive.bg_fill), diam, "{tema:?} tombol diam");
            assert_eq!(rgb(v.widgets.open.bg_fill), terbuka, "{tema:?} tombol terbuka");
            assert_eq!(
                rgb(v.widgets.noninteractive.fg_stroke.color),
                teks,
                "{tema:?} teks"
            );
            assert_eq!(rgb(v.window_stroke.color), garis, "{tema:?} garis jendela");
            assert_eq!(rgb(v.faint_bg_color), halus, "{tema:?} strip halus");
            // Hyperlink & kursor teks = aksen tema, bukan default egui.
            assert_eq!(v.hyperlink_color, tema.accent(), "{tema:?} hyperlink");
            assert_eq!(v.text_cursor.stroke.color, tema.accent(), "{tema:?} kursor");
            // Bukan lagi abu-abu default egui di kedua basis.
            assert_ne!(
                v.widgets.inactive.bg_fill,
                Visuals::dark().widgets.inactive.bg_fill,
                "{tema:?} masih default gelap"
            );
            assert_ne!(
                v.widgets.inactive.bg_fill,
                Visuals::light().widgets.inactive.bg_fill,
                "{tema:?} masih default terang"
            );
        }
    }

    #[test]
    fn kontrol_panel_terbedakan_dari_latar() {
        // Penjaga vonis user iterasi v2: kontrol diam panel kanan harus
        // JELAS terbedakan dari latar — bukan lagi selisih 10-12 kanal
        // dan rasio 1,06-1,11 seperti v1 (angka QA v1 vs panel_fill).
        // Dua latar diperiksa karena tombol yang sama dipakai di dua
        // permukaan: panel_fill (metrik QA; permukaan toolbar/menubar)
        // dan theme.bg() (latar ASLI panel edit — CentralPanel ber-frame
        // theme_bg(), bukan SidePanel; terbukti dari kode + sampling
        // piksel). Latar ketiga, window_fill (latar dialog), ikut
        // diperiksa karena fill yang sama dipakai tombol dialog.
        // Koreksi QA v1 yang dipakai di sini: Aqua BUKAN patokan angka
        // (1,06 vs panel = terburuk), melainkan patokan karakter (tint
        // biru + teks gelap); ambang rasio 1,35 di bawah nilai v2
        // terendah (1,41) dan di atas nilai v1 tertinggi (1,32).
        let d = |a: (u8, u8, u8), b: (u8, u8, u8)| {
            [a.0.abs_diff(b.0), a.1.abs_diff(b.1), a.2.abs_diff(b.2)]
                .into_iter()
                .max()
                .unwrap()
        };
        let lum = |c: (u8, u8, u8)| {
            let lin = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * lin(c.0) + 0.7152 * lin(c.1) + 0.0722 * lin(c.2)
        };
        let rasio = |a: (u8, u8, u8), b: (u8, u8, u8)| {
            let (la, lb) = (lum(a), lum(b));
            (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
        };
        for tema in Theme::all() {
            let v = theme_visuals(tema);
            let panel = rgb(v.panel_fill);
            let window = rgb(v.window_fill);
            let bg = rgb(tema.bg());
            let diam = rgb(v.widgets.inactive.bg_fill);
            let hover = rgb(v.widgets.hovered.bg_fill);
            let terbuka = rgb(v.widgets.open.bg_fill);
            for (nama, latar) in [("panel", panel), ("bg", bg), ("window", window)] {
                assert!(
                    d(diam, latar) >= 30,
                    "{tema:?} tombol diam masih menempel latar {nama}: {diam:?} vs {latar:?}"
                );
            }
            assert!(
                rasio(diam, panel) >= 1.35,
                "{tema:?} rasio vs panel di bawah ambang: {diam:?} vs {panel:?}"
            );
            assert!(
                rasio(diam, bg) >= 1.35,
                "{tema:?} rasio vs bg di bawah ambang: {diam:?} vs {bg:?}"
            );
            assert!(
                d(hover, diam) >= 10,
                "{tema:?} hover tak terbedakan dari diam: {hover:?} vs {diam:?}"
            );
            assert!(
                d(terbuka, diam) >= 10,
                "{tema:?} terbuka tak terbedakan dari diam: {terbuka:?} vs {diam:?}"
            );
        }
    }

    #[test]
    fn palet_lama_tidak_berubah() {
        // Nilai yang sudah ada sebelum 0.8.27 harus identik, kecuali
        // hovered yang digantikan ramp v2 (lihat catatan di bawah).
        let dn = theme_visuals(Theme::DarkNavy);
        assert_eq!(rgb(dn.panel_fill), (13, 26, 51));
        assert_eq!(rgb(dn.window_fill), (10, 20, 40));
        assert_eq!(rgb(dn.extreme_bg_color), (6, 13, 29));
        assert_eq!(rgb(dn.selection.bg_fill), (30, 77, 158));
        // v2: hovered DarkNavy lama (20,41,77) digantikan ramp 35% agar
        // urutan diam<hover<active terjaga sesudah diam dinaikkan ke 20%
        // (hovered lama akan lebih redup dari tombol diam baru).
        assert_eq!(rgb(dn.widgets.hovered.bg_fill), (35, 73, 122));
        assert_eq!(rgb(dn.widgets.active.bg_fill), (30, 77, 158));
        let ne = theme_visuals(Theme::Neon);
        assert_eq!(rgb(ne.panel_fill), (8, 13, 10));
        assert_eq!(rgb(ne.selection.bg_fill), (16, 46, 30));
        let mo = theme_visuals(Theme::Mono);
        assert_eq!(rgb(mo.panel_fill), (255, 255, 255));
        assert_eq!(rgb(mo.selection.bg_fill), (17, 17, 17));
        let aq = theme_visuals(Theme::Aqua);
        assert_eq!(rgb(aq.panel_fill), (255, 255, 255));
        assert_eq!(rgb(aq.selection.bg_fill), (10, 132, 255));
        // v2: hovered Aqua diturunkan dari extreme/theme.bg(), bukan
        // panel (lihat komentar lengan Aqua) — dikunci di nilai baru.
        assert_eq!(rgb(aq.widgets.hovered.bg_fill), (108, 176, 248));
        // Identitas tema: aksen & latar area gambar tidak berubah.
        assert_eq!(Theme::DarkNavy.accent(), Color32::from_rgb(77, 159, 255));
        assert_eq!(Theme::Neon.accent(), Color32::from_rgb(0, 255, 157));
        assert_eq!(Theme::Mono.accent(), Color32::from_rgb(17, 17, 17));
        assert_eq!(Theme::Aqua.accent(), Color32::from_rgb(10, 132, 255));
    }
}
