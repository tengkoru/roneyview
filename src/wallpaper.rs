//! Set as wallpaper untuk banyak desktop.
//!
//! Tidak ada satu cara yang berlaku di semua desktop: Xfce/KDE/GNOME dkk. menggambar
//! jendela desktop sendiri di atas root window X11, sehingga wallpaper yang dilukis ke
//! root tertutup. Karena itu Roneyview mendeteksi desktop yang berjalan lalu memakai
//! mekanisme resminya; bila tidak ada desktop manager (Fluxbox, Openbox, i3, ...), ia
//! melukis sendiri ke root window X11.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::{self, FilterType};
use image::{ExtendedColorType, ImageEncoder, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------ jenis dasar

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum WpStyle {
    None,
    Centered,
    Tiled,
    Stretched,
    Scaled,
    #[default]
    Zoomed,
}

impl WpStyle {
    pub const ALL: [WpStyle; 6] = [
        WpStyle::None,
        WpStyle::Centered,
        WpStyle::Tiled,
        WpStyle::Stretched,
        WpStyle::Scaled,
        WpStyle::Zoomed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            WpStyle::None => "None",
            WpStyle::Centered => "Centered",
            WpStyle::Tiled => "Tiled",
            WpStyle::Stretched => "Stretched",
            WpStyle::Scaled => "Scaled",
            WpStyle::Zoomed => "Zoomed",
        }
    }

    pub fn parse(s: &str) -> Option<WpStyle> {
        Self::ALL
            .into_iter()
            .find(|v| v.label().eq_ignore_ascii_case(s.trim()))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ColorMode {
    #[default]
    Solid,
    Transparent,
}

impl ColorMode {
    pub fn label(self) -> &'static str {
        match self {
            ColorMode::Solid => "Solid color",
            ColorMode::Transparent => "Transparent",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WpPrefs {
    pub style: WpStyle,
    pub mode: ColorMode,
    pub color: [u8; 3],
}

impl Default for WpPrefs {
    fn default() -> Self {
        WpPrefs {
            style: WpStyle::default(),
            mode: ColorMode::Solid,
            color: [0, 0, 0],
        }
    }
}

pub fn color_hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// "#rrggbb" atau "rrggbb" (huruf besar/kecil bebas).
pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let t = s.trim().trim_start_matches('#');
    if t.len() != 6 || !t.is_ascii() {
        return None;
    }
    let v = u32::from_str_radix(t, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

// --------------------------------------------------------------- tata letak murni

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Ubin diulang dari (0,0) ke seluruh layar.
    pub tiled: bool,
}

/// Letak gambar `iw x ih` pada layar `sw x sh` untuk gaya tertentu. `None` = tanpa gambar.
/// Dipakai bersama oleh pratinjau dan renderer, jadi keduanya selalu sama.
pub fn place(style: WpStyle, iw: f32, ih: f32, sw: f32, sh: f32) -> Option<Placement> {
    if iw <= 0.0 || ih <= 0.0 || sw <= 0.0 || sh <= 0.0 {
        return None;
    }
    let centered = |w: f32, h: f32| Placement {
        x: (sw - w) / 2.0,
        y: (sh - h) / 2.0,
        w,
        h,
        tiled: false,
    };
    match style {
        WpStyle::None => None,
        WpStyle::Centered => Some(centered(iw, ih)),
        WpStyle::Tiled => Some(Placement {
            x: 0.0,
            y: 0.0,
            w: iw,
            h: ih,
            tiled: true,
        }),
        WpStyle::Stretched => Some(Placement {
            x: 0.0,
            y: 0.0,
            w: sw,
            h: sh,
            tiled: false,
        }),
        WpStyle::Scaled => {
            let k = (sw / iw).min(sh / ih);
            Some(centered(iw * k, ih * k))
        }
        WpStyle::Zoomed => {
            let k = (sw / iw).max(sh / ih);
            Some(centered(iw * k, ih * k))
        }
    }
}

fn blend(px: Rgba<u8>, bg: [u8; 3]) -> [u8; 3] {
    let a = u32::from(px[3]);
    let mix = |c: u8, b: u8| ((u32::from(c) * a + u32::from(b) * (255 - a) + 127) / 255) as u8;
    [mix(px[0], bg[0]), mix(px[1], bg[1]), mix(px[2], bg[2])]
}

/// Batas piksel hasil skala (mencegah alokasi raksasa pada gambar kecil + layar besar).
const MAX_SCALED_PIXELS: u64 = 120_000_000;

/// Gambar kanvas `sw x sh` (opak) berisi warna latar + gambar menurut gaya.
pub fn render_canvas(
    src: &RgbaImage,
    style: WpStyle,
    bg: [u8; 3],
    sw: u32,
    sh: u32,
) -> Result<RgbaImage, String> {
    if sw == 0 || sh == 0 {
        return Err("Ukuran layar nol".into());
    }
    let mut canvas = RgbaImage::from_pixel(sw, sh, Rgba([bg[0], bg[1], bg[2], 255]));
    let (iw, ih) = src.dimensions();
    let Some(p) = place(style, iw as f32, ih as f32, sw as f32, sh as f32) else {
        return Ok(canvas);
    };
    if p.tiled {
        for y in 0..sh {
            let sy = y % ih;
            for x in 0..sw {
                let c = blend(*src.get_pixel(x % iw, sy), bg);
                canvas.put_pixel(x, y, Rgba([c[0], c[1], c[2], 255]));
            }
        }
        return Ok(canvas);
    }
    let (tw, th) = ((p.w.round() as u32).max(1), (p.h.round() as u32).max(1));
    if u64::from(tw) * u64::from(th) > MAX_SCALED_PIXELS {
        return Err("Hasil skala terlalu besar untuk layar ini".into());
    }
    let x = p.x.round() as i64;
    let y = p.y.round() as i64;
    if (tw, th) == (iw, ih) {
        imageops::overlay(&mut canvas, src, x, y);
    } else {
        let resized = imageops::resize(src, tw, th, FilterType::Lanczos3);
        imageops::overlay(&mut canvas, &resized, x, y);
    }
    // Pembulatan saat pencampuran alpha bisa menyisakan 254; hasil akhir selalu opak.
    for px in canvas.pixels_mut() {
        px[3] = 255;
    }
    Ok(canvas)
}

// ------------------------------------------------------------- deteksi desktop

#[derive(Clone, Debug, PartialEq)]
pub enum Backend {
    Xfce,
    Gnome,
    Cinnamon,
    Mate,
    Kde,
    Lxde,
    LxQt,
    Sway,
    X11Root,
    Unsupported(String),
}

impl Backend {
    pub fn label(&self) -> String {
        match self {
            Backend::Xfce => "Xfce (xfconf)".into(),
            Backend::Gnome => "GNOME/Budgie/Unity (gsettings)".into(),
            Backend::Cinnamon => "Cinnamon (gsettings)".into(),
            Backend::Mate => "MATE (gsettings)".into(),
            Backend::Kde => "KDE Plasma (D-Bus)".into(),
            Backend::Lxde => "LXDE (pcmanfm)".into(),
            Backend::LxQt => "LXQt (pcmanfm-qt)".into(),
            Backend::Sway => "Sway (swaymsg)".into(),
            Backend::X11Root => "Root window X11 (tanpa desktop manager)".into(),
            Backend::Unsupported(why) => format!("Tidak didukung: {why}"),
        }
    }

    pub fn supported(&self) -> bool {
        !matches!(self, Backend::Unsupported(_))
    }

    /// Hanya Xfce yang punya warna "Transparent" sungguhan.
    pub fn has_transparent(&self) -> bool {
        matches!(self, Backend::Xfce)
    }
}

/// Potret lingkungan proses (dipisah supaya deteksi bisa diuji).
#[derive(Clone, Debug, Default)]
pub struct Env {
    /// Token huruf kecil dari XDG_CURRENT_DESKTOP dan DESKTOP_SESSION.
    pub desktops: Vec<String>,
    pub display: bool,
    pub wayland_session: bool,
    pub swaysock: bool,
}

impl Env {
    pub fn from_process() -> Env {
        let mut desktops: Vec<String> = Vec::new();
        for var in ["XDG_CURRENT_DESKTOP", "DESKTOP_SESSION"] {
            if let Ok(v) = std::env::var(var) {
                desktops.extend(v.split([':', ';']).map(|t| t.trim().to_ascii_lowercase()));
            }
        }
        desktops.retain(|t| !t.is_empty());
        Env {
            desktops,
            display: std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty()),
            wayland_session: std::env::var("XDG_SESSION_TYPE")
                .is_ok_and(|v| v.eq_ignore_ascii_case("wayland")),
            swaysock: std::env::var_os("SWAYSOCK").is_some(),
        }
    }

    fn is(&self, names: &[&str]) -> bool {
        self.desktops
            .iter()
            .any(|d| names.iter().any(|n| d == n || d.contains(n)))
    }
}

pub fn which(cmd: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            std::fs::metadata(dir.join(cmd))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

pub fn detect_with(env: &Env, has: &dyn Fn(&str) -> bool) -> Backend {
    let need = |tool: &str, b: Backend| {
        if has(tool) {
            b
        } else {
            Backend::Unsupported(format!("perintah \"{tool}\" tidak ditemukan"))
        }
    };
    if env.is(&["xfce"]) {
        return need("xfconf-query", Backend::Xfce);
    }
    if env.is(&["kde", "plasma"]) {
        return if has("gdbus") {
            Backend::Kde
        } else {
            need("qdbus", Backend::Kde)
        };
    }
    if env.is(&["cinnamon"]) {
        return need("gsettings", Backend::Cinnamon);
    }
    if env.is(&["mate"]) {
        return need("gsettings", Backend::Mate);
    }
    if env.is(&["gnome", "unity", "budgie", "pantheon", "ubuntu"]) {
        return need("gsettings", Backend::Gnome);
    }
    if env.is(&["lxqt"]) {
        return need("pcmanfm-qt", Backend::LxQt);
    }
    if env.is(&["lxde"]) {
        return need("pcmanfm", Backend::Lxde);
    }
    if env.swaysock || env.is(&["sway"]) {
        return need("swaymsg", Backend::Sway);
    }
    if env.wayland_session {
        return Backend::Unsupported("sesi Wayland dengan compositor yang belum didukung".into());
    }
    if env.display {
        return Backend::X11Root;
    }
    Backend::Unsupported("tidak ada sesi grafis yang terdeteksi".into())
}

pub fn detect() -> Backend {
    detect_with(&Env::from_process(), &which)
}

// ------------------------------------------------------- penyusun perintah (murni)

fn style_code_xfce(s: WpStyle) -> i32 {
    match s {
        WpStyle::None => 0,
        WpStyle::Centered => 1,
        WpStyle::Tiled => 2,
        WpStyle::Stretched => 3,
        WpStyle::Scaled => 4,
        WpStyle::Zoomed => 5,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct XfProp {
    pub path: String,
    pub ty: &'static str,
    pub values: Vec<String>,
}

/// Properti xfce4-desktop untuk satu monitor/workspace (`base` = ".../workspace0").
pub fn xfce_props(base: &str, file: &str, style: WpStyle, mode: ColorMode, c: [u8; 3]) -> Vec<XfProp> {
    let p = |name: &str, ty: &'static str, values: Vec<String>| XfProp {
        path: format!("{base}/{name}"),
        ty,
        values,
    };
    let f = |v: u8| format!("{:.6}", f64::from(v) / 255.0);
    vec![
        p("last-image", "string", vec![file.to_string()]),
        p(
            "image-show",
            "bool",
            vec![(style != WpStyle::None).to_string()],
        ),
        p("image-style", "int", vec![style_code_xfce(style).to_string()]),
        // color-style: 0 = solid, 3 = transparent (xfdesktop)
        p(
            "color-style",
            "int",
            vec![if mode == ColorMode::Transparent { "3" } else { "0" }.to_string()],
        ),
        p(
            "rgba1",
            "double",
            vec![f(c[0]), f(c[1]), f(c[2]), "1.000000".to_string()],
        ),
    ]
}

fn picture_option(s: WpStyle) -> &'static str {
    match s {
        WpStyle::None => "none",
        WpStyle::Centered => "centered",
        WpStyle::Tiled => "wallpaper",
        WpStyle::Stretched => "stretched",
        WpStyle::Scaled => "scaled",
        WpStyle::Zoomed => "zoom",
    }
}

pub fn file_uri(path: &str) -> String {
    let mut out = String::from("file://");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Perintah `gsettings set ...` untuk GNOME / Cinnamon / MATE.
pub fn gsettings_cmds(b: &Backend, file: &str, style: WpStyle, c: [u8; 3]) -> Vec<Vec<String>> {
    let hex = color_hex(c);
    let set = |schema: &str, key: &str, val: String| {
        vec!["gsettings".to_string(), "set".into(), schema.to_string(), key.to_string(), val]
    };
    let mut v = Vec::new();
    match b {
        Backend::Mate => {
            let s = "org.mate.background";
            v.push(set(s, "picture-filename", file.to_string()));
            v.push(set(s, "picture-options", picture_option(style).into()));
            v.push(set(s, "primary-color", hex.clone()));
            v.push(set(s, "secondary-color", hex));
            v.push(set(s, "color-shading-type", "solid".into()));
            v.push(set(s, "draw-background", "true".into()));
        }
        Backend::Cinnamon => {
            let s = "org.cinnamon.desktop.background";
            v.push(set(s, "picture-uri", file_uri(file)));
            v.push(set(s, "picture-options", picture_option(style).into()));
            v.push(set(s, "primary-color", hex.clone()));
            v.push(set(s, "secondary-color", hex));
            v.push(set(s, "color-shading-type", "solid".into()));
        }
        _ => {
            let s = "org.gnome.desktop.background";
            v.push(set(s, "picture-uri", file_uri(file)));
            v.push(set(s, "picture-uri-dark", file_uri(file)));
            v.push(set(s, "picture-options", picture_option(style).into()));
            v.push(set(s, "primary-color", hex.clone()));
            v.push(set(s, "secondary-color", hex));
            v.push(set(s, "color-shading-type", "solid".into()));
        }
    }
    v
}

fn js_str(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Skrip Plasma (dijalankan lewat org.kde.PlasmaShell.evaluateScript).
pub fn kde_script(file: &str, style: WpStyle, c: [u8; 3]) -> String {
    let hex = js_str(&color_hex(c));
    if style == WpStyle::None {
        return format!(
            "var all = desktops(); for (var i = 0; i < all.length; i++) {{ var d = all[i]; \
             d.wallpaperPlugin = \"org.kde.color\"; \
             d.currentConfigGroup = Array(\"Wallpaper\", \"org.kde.color\", \"General\"); \
             d.writeConfig(\"Color\", {hex}); }}"
        );
    }
    // FillMode Qt: 0 Stretch, 1 PreserveAspectFit, 2 PreserveAspectCrop, 3 Tile, 6 Pad
    let fill = match style {
        WpStyle::Stretched => 0,
        WpStyle::Scaled => 1,
        WpStyle::Zoomed => 2,
        WpStyle::Tiled => 3,
        _ => 6,
    };
    format!(
        "var all = desktops(); for (var i = 0; i < all.length; i++) {{ var d = all[i]; \
         d.wallpaperPlugin = \"org.kde.image\"; \
         d.currentConfigGroup = Array(\"Wallpaper\", \"org.kde.image\", \"General\"); \
         d.writeConfig(\"Image\", {}); d.writeConfig(\"FillMode\", {fill}); \
         d.writeConfig(\"Color\", {hex}); }}",
        js_str(&file_uri(file))
    )
}

pub fn pcmanfm_cmd(qt: bool, file: &str, style: WpStyle) -> Vec<String> {
    let mode = match style {
        WpStyle::None => "color",
        WpStyle::Centered => "center",
        WpStyle::Tiled => "tile",
        WpStyle::Stretched => "stretch",
        WpStyle::Scaled => "fit",
        WpStyle::Zoomed => {
            if qt {
                "zoom"
            } else {
                "crop"
            }
        }
    };
    vec![
        if qt { "pcmanfm-qt" } else { "pcmanfm" }.to_string(),
        format!("--set-wallpaper={file}"),
        format!("--wallpaper-mode={mode}"),
    ]
}

pub fn sway_cmd(file: &str, style: WpStyle, c: [u8; 3]) -> Vec<String> {
    let hex = color_hex(c);
    let mut v = vec!["swaymsg".to_string(), "output".into(), "*".into(), "bg".into()];
    if style == WpStyle::None {
        v.extend([hex, "solid_color".to_string()]);
        return v;
    }
    let mode = match style {
        WpStyle::Centered => "center",
        WpStyle::Tiled => "tile",
        WpStyle::Stretched => "stretch",
        WpStyle::Scaled => "fit",
        _ => "fill",
    };
    v.extend([file.to_string(), mode.to_string(), hex]);
    v
}

// -------------------------------------------------------------- menjalankan perintah

/// Jalankan perintah dengan batas waktu; kembalikan stdout, atau pesan galat.
pub fn run(cmd: &[String], timeout: Duration) -> Result<String, String> {
    let (prog, args) = cmd.split_first().ok_or("Perintah kosong")?;
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Tidak dapat menjalankan {prog}: {e}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                let mut err = String::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_string(&mut out);
                }
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err);
                }
                return if status.success() {
                    Ok(out)
                } else {
                    let msg = err.trim();
                    Err(format!(
                        "{prog} gagal ({}){}{}",
                        status.code().map_or("sinyal".to_string(), |c| c.to_string()),
                        if msg.is_empty() { "" } else { ": " },
                        msg
                    ))
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{prog} tidak merespons (waktu habis)"));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("Gagal menunggu {prog}: {e}")),
        }
    }
}

const CMD_TIMEOUT: Duration = Duration::from_secs(20);

fn apply_xfce(file: &str, style: WpStyle, mode: ColorMode, c: [u8; 3]) -> Result<String, String> {
    let listing = run(
        &["xfconf-query".into(), "-c".into(), "xfce4-desktop".into(), "-l".into()],
        CMD_TIMEOUT,
    )
    .unwrap_or_default();
    let mut bases: Vec<String> = listing
        .lines()
        .filter_map(|l| l.trim().strip_suffix("/last-image").map(str::to_string))
        .collect();
    if bases.is_empty() {
        // Instalasi baru: belum ada properti. Buat untuk tiap monitor + monitor0.
        let mut names = x11_monitor_names();
        names.push("0".into());
        names.dedup();
        bases = names
            .into_iter()
            .map(|n| format!("/backdrop/screen0/monitor{n}/workspace0"))
            .collect();
    }
    bases.sort();
    bases.dedup();
    for base in &bases {
        for prop in xfce_props(base, file, style, mode, c) {
            let mut cmd: Vec<String> = vec![
                "xfconf-query".into(),
                "-c".into(),
                "xfce4-desktop".into(),
                "-p".into(),
                prop.path.clone(),
                "-n".into(),
            ];
            for v in &prop.values {
                cmd.extend(["-t".to_string(), prop.ty.to_string(), "-s".to_string(), v.clone()]);
            }
            if run(&cmd, CMD_TIMEOUT).is_err() {
                // Tipe lama berbeda: reset lalu buat ulang.
                let reset: Vec<String> = vec![
                    "xfconf-query".into(),
                    "-c".into(),
                    "xfce4-desktop".into(),
                    "-p".into(),
                    prop.path.clone(),
                    "-r".into(),
                ];
                let _ = run(&reset, CMD_TIMEOUT);
                run(&cmd, CMD_TIMEOUT)?;
            }
        }
    }
    Ok(format!("Wallpaper diterapkan ke Xfce ({} layar/workspace)", bases.len()))
}

// ------------------------------------------------------------ root window X11

/// Nama monitor dari RandR (mis. "HDMI-1"); kosong bila tidak tersedia.
fn x11_monitor_names() -> Vec<String> {
    use x11rb::connection::Connection;
    use x11rb::protocol::randr::ConnectionExt as _;
    use x11rb::protocol::xproto::ConnectionExt as _;
    let Ok((conn, snum)) = x11rb::connect(None) else {
        return Vec::new();
    };
    let root = conn.setup().roots[snum].root;
    let Ok(cookie) = conn.randr_get_monitors(root, true) else {
        return Vec::new();
    };
    let Ok(reply) = cookie.reply() else {
        return Vec::new();
    };
    reply
        .monitors
        .iter()
        .filter_map(|m| {
            let name = conn.get_atom_name(m.name).ok()?.reply().ok()?.name;
            String::from_utf8(name).ok()
        })
        .collect()
}

/// Lukis wallpaper ke root window X11 (untuk window manager tanpa desktop manager).
pub fn apply_root(src: &RgbaImage, style: WpStyle, bg: [u8; 3]) -> Result<String, String> {
    use x11rb::connection::{Connection, RequestConnection};
    use x11rb::wrapper::ConnectionExt as _;
    use x11rb::protocol::randr::ConnectionExt as _;
    use x11rb::protocol::xproto::{
        AtomEnum, ChangeWindowAttributesAux, CloseDown, ConnectionExt as _, CreateGCAux,
        ImageFormat, ImageOrder, PropMode,
    };

    let (conn, snum) =
        x11rb::connect(None).map_err(|e| format!("Tidak dapat terhubung ke server X11: {e}"))?;
    let setup = conn.setup();
    let screen = &setup.roots[snum];
    let root = screen.root;
    let depth = screen.root_depth;
    let (sw, sh) = (u32::from(screen.width_in_pixels), u32::from(screen.height_in_pixels));

    let bpp = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == depth)
        .map_or(0, |f| f.bits_per_pixel);
    if bpp != 32 {
        return Err(format!("Format piksel root X11 tidak didukung (kedalaman {depth}, {bpp} bpp)"));
    }
    let masks_ok = screen
        .allowed_depths
        .iter()
        .flat_map(|d| d.visuals.iter())
        .find(|v| v.visual_id == screen.root_visual)
        .is_some_and(|v| v.red_mask == 0xff_0000 && v.green_mask == 0xff00 && v.blue_mask == 0xff);
    if !masks_ok {
        return Err("Visual root X11 bukan TrueColor RGB 8-bit yang didukung".into());
    }
    let msb = setup.image_byte_order == ImageOrder::MSB_FIRST;

    let monitors: Vec<(i32, i32, u32, u32)> = conn
        .randr_get_monitors(root, true)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| {
            r.monitors
                .iter()
                .map(|m| (i32::from(m.x), i32::from(m.y), u32::from(m.width), u32::from(m.height)))
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec![(0, 0, sw, sh)]);

    // Susun seluruh layar maya dalam urutan byte server (BGRX atau XRGB).
    let (sw_us, sh_us) = (sw as usize, sh as usize);
    let mut buf = vec![0u8; sw_us * sh_us * 4];
    let put = |buf: &mut [u8], i: usize, r: u8, g: u8, b: u8| {
        if msb {
            buf[i..i + 4].copy_from_slice(&[0, r, g, b]);
        } else {
            buf[i..i + 4].copy_from_slice(&[b, g, r, 0]);
        }
    };
    for i in 0..sw_us * sh_us {
        put(&mut buf, i * 4, bg[0], bg[1], bg[2]);
    }
    for (mx, my, mw, mh) in &monitors {
        let canvas = render_canvas(src, style, bg, *mw, *mh)?;
        for y in 0..*mh as i32 {
            let ty = my + y;
            if ty < 0 || ty >= sh as i32 {
                continue;
            }
            for x in 0..*mw as i32 {
                let tx = mx + x;
                if tx < 0 || tx >= sw as i32 {
                    continue;
                }
                let px = canvas.get_pixel(x as u32, y as u32);
                put(&mut buf, (ty as usize * sw_us + tx as usize) * 4, px[0], px[1], px[2]);
            }
        }
    }

    let pixmap = conn.generate_id().map_err(|e| e.to_string())?;
    conn.create_pixmap(depth, pixmap, root, sw as u16, sh as u16)
        .map_err(|e| e.to_string())?;
    let gc = conn.generate_id().map_err(|e| e.to_string())?;
    conn.create_gc(gc, pixmap, &CreateGCAux::new())
        .map_err(|e| e.to_string())?;
    let max_bytes = conn.maximum_request_bytes().saturating_sub(256).max(4096);
    let rows_per_req = (max_bytes / (sw_us * 4)).clamp(1, 256);
    let mut y = 0usize;
    while y < sh_us {
        let h = rows_per_req.min(sh_us - y);
        conn.put_image(
            ImageFormat::Z_PIXMAP,
            pixmap,
            gc,
            sw as u16,
            h as u16,
            0,
            y as i16,
            0,
            depth,
            &buf[y * sw_us * 4..(y + h) * sw_us * 4],
        )
        .map_err(|e| e.to_string())?;
        y += h;
    }
    conn.free_gc(gc).map_err(|e| e.to_string())?;

    let atom = |name: &str| -> Result<u32, String> {
        conn.intern_atom(false, name.as_bytes())
            .map_err(|e| e.to_string())?
            .reply()
            .map(|r| r.atom)
            .map_err(|e| e.to_string())
    };
    let a_root = atom("_XROOTPMAP_ID")?;
    let a_eset = atom("ESETROOT_PMAP_ID")?;
    let read_pixmap = |a: u32| -> Option<u32> {
        let r = conn
            .get_property(false, root, a, AtomEnum::PIXMAP, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        let first = r.value32()?.next();
        first
    };
    // Konvensi Esetroot: bila kedua properti menunjuk pixmap yang sama, pixmap itu
    // milik pengatur wallpaper sebelumnya dan boleh dibebaskan.
    let old = match (read_pixmap(a_root), read_pixmap(a_eset)) {
        (Some(a), Some(b)) if a == b && a != 0 => Some(a),
        _ => None,
    };

    conn.change_window_attributes(root, &ChangeWindowAttributesAux::new().background_pixmap(pixmap))
        .map_err(|e| e.to_string())?;
    conn.clear_area(false, root, 0, 0, 0, 0).map_err(|e| e.to_string())?;
    for a in [a_root, a_eset] {
        conn.change_property32(PropMode::REPLACE, root, a, AtomEnum::PIXMAP, &[pixmap])
            .map_err(|e| e.to_string())?;
    }
    if let Some(o) = old {
        if o != pixmap {
            let _ = conn.free_pixmap(o);
        }
    }
    // Pixmap harus bertahan setelah Roneyview keluar.
    conn.set_close_down_mode(CloseDown::RETAIN_PERMANENT)
        .map_err(|e| e.to_string())?;
    conn.flush().map_err(|e| e.to_string())?;
    conn.get_input_focus()
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("Server X11 menolak perubahan: {e}"))?;
    Ok(format!(
        "Wallpaper dilukis ke root window X11 ({} monitor, {}x{})",
        monitors.len(),
        sw,
        sh
    ))
}

// ---------------------------------------------------- berkas, konfigurasi, restore

fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("roneyview"))
}

pub fn write_png(img: &RgbaImage, path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Tidak dapat membuat folder: {e}"))?;
    }
    let tmp = path.with_extension("png.tmp");
    let file = std::fs::File::create(&tmp).map_err(|e| format!("Tidak dapat menulis berkas: {e}"))?;
    PngEncoder::new_with_quality(std::io::BufWriter::new(file), CompressionType::Fast, PngFilter::Adaptive)
        .write_image(img.as_raw(), img.width(), img.height(), ExtendedColorType::Rgba8)
        .map_err(|e| format!("Gagal menyimpan PNG: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("Gagal menyimpan PNG: {e}"))
}

/// Simpan salinan gambar sumber untuk desktop yang butuh jalur berkas. Nama unik tiap
/// kali (desktop tidak memuat ulang bila jalurnya sama); salinan lama dihapus.
fn export_for_desktop(img: &RgbaImage) -> Result<PathBuf, String> {
    let dir = data_dir().ok_or("Folder data pengguna tidak ditemukan")?;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let path = dir.join(format!("wallpaper-{ms}.png"));
    write_png(img, &path)?;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with("wallpaper-") && n.ends_with(".png") && e.path() != path {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(path)
}

fn is_desktop_safe_ext(p: &Path) -> bool {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "jpg" | "jpeg" | "png" | "bmp"))
}

/// Konfigurasi untuk `--restore-wallpaper` (hanya dipakai backend root window).
#[derive(Serialize, Deserialize)]
struct RestoreConfig {
    image: PathBuf,
    style: WpStyle,
    color: [u8; 3],
}

fn restore_path() -> Option<PathBuf> {
    crate::settings::config_path("wallpaper.json")
}

fn autostart_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("autostart").join("roneyview-wallpaper.desktop"))
}

fn desktop_exec_quote(p: &str) -> String {
    let mut s = String::from("\"");
    for c in p.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('"');
    s
}

fn save_restore(src: &RgbaImage, style: WpStyle, color: [u8; 3]) -> Result<(), String> {
    let image = data_dir().ok_or("Folder data pengguna tidak ditemukan")?.join("wallpaper-source.png");
    write_png(src, &image)?;
    let cfg = RestoreConfig { image, style, color };
    let json = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    let path = restore_path().ok_or("Folder konfigurasi tidak ditemukan")?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, json).map_err(|e| e.to_string())?;

    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .into_owned();
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Roneyview wallpaper\n\
         Comment=Memulihkan wallpaper saat login\nExec={} --restore-wallpaper\n\
         NoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
        desktop_exec_quote(&exe)
    );
    let auto = autostart_path().ok_or("Folder autostart tidak ditemukan")?;
    if let Some(d) = auto.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(auto, entry).map_err(|e| e.to_string())
}

/// Pasang wallpaper dengan backend terpilih. `original` = berkas biasa yang boleh dipakai
/// langsung (bila formatnya dikenal desktop); selain itu diekspor sebagai PNG.
pub fn apply(
    backend: &Backend,
    src: &RgbaImage,
    original: Option<&Path>,
    style: WpStyle,
    mode: ColorMode,
    color: [u8; 3],
) -> Result<String, String> {
    // Hanya Xfce yang punya "Transparent"; di tempat lain dipakai warna terpilih.
    let color = if mode == ColorMode::Transparent && !backend.has_transparent() {
        [0, 0, 0]
    } else {
        color
    };
    let file_for_desktop = |src: &RgbaImage| -> Result<String, String> {
        let p = match original {
            Some(o) if is_desktop_safe_ext(o) && o.is_absolute() => o.to_path_buf(),
            _ => export_for_desktop(src)?,
        };
        Ok(p.to_string_lossy().into_owned())
    };
    match backend {
        Backend::Unsupported(why) => Err(format!("Desktop ini belum didukung: {why}")),
        Backend::X11Root => {
            let msg = apply_root(src, style, color)?;
            let saved = save_restore(src, style, color);
            Ok(match saved {
                Ok(()) => format!("{msg}. Dipulihkan otomatis saat login (autostart)."),
                Err(e) => format!("{msg}. Peringatan: pemulihan saat login gagal disiapkan ({e})."),
            })
        }
        Backend::Xfce => apply_xfce(&file_for_desktop(src)?, style, mode, color),
        Backend::Gnome | Backend::Cinnamon | Backend::Mate => {
            let file = file_for_desktop(src)?;
            for cmd in gsettings_cmds(backend, &file, style, color) {
                if let Err(e) = run(&cmd, CMD_TIMEOUT) {
                    // `picture-uri-dark` hanya ada di GNOME >= 42; tidak boleh menggagalkan semuanya.
                    if cmd.get(3).is_some_and(|k| k == "picture-uri-dark") {
                        continue;
                    }
                    return Err(e);
                }
            }
            Ok(format!("Wallpaper diterapkan ({})", backend.label()))
        }
        Backend::Kde => {
            let file = file_for_desktop(src)?;
            let script = kde_script(&file, style, color);
            let gdbus: Vec<String> = [
                "gdbus", "call", "--session", "--dest", "org.kde.plasmashell",
                "--object-path", "/PlasmaShell", "--method",
                "org.kde.PlasmaShell.evaluateScript",
            ]
            .iter()
            .map(|s| s.to_string())
            .chain([script.clone()])
            .collect();
            if which("gdbus") {
                run(&gdbus, CMD_TIMEOUT)?;
            } else {
                let q: Vec<String> = ["qdbus", "org.kde.plasmashell", "/PlasmaShell", "org.kde.PlasmaShell.evaluateScript"]
                    .iter()
                    .map(|s| s.to_string())
                    .chain([script])
                    .collect();
                run(&q, CMD_TIMEOUT)?;
            }
            Ok("Wallpaper diterapkan ke KDE Plasma".into())
        }
        Backend::Lxde | Backend::LxQt => {
            let file = file_for_desktop(src)?;
            run(&pcmanfm_cmd(*backend == Backend::LxQt, &file, style), CMD_TIMEOUT)?;
            Ok(format!("Wallpaper diterapkan ({})", backend.label()))
        }
        Backend::Sway => {
            let file = file_for_desktop(src)?;
            run(&sway_cmd(&file, style, color), CMD_TIMEOUT)?;
            Ok("Wallpaper diterapkan ke Sway".into())
        }
    }
}

/// `roneyview --restore-wallpaper`: pasang ulang wallpaper root window saat login.
pub fn restore() -> Result<String, String> {
    let path = restore_path().ok_or("Folder konfigurasi tidak ditemukan")?;
    let raw = std::fs::read_to_string(&path).map_err(|_| "Belum ada wallpaper yang disimpan".to_string())?;
    let cfg: RestoreConfig = serde_json::from_str(&raw).map_err(|e| format!("Konfigurasi rusak: {e}"))?;
    let bytes = std::fs::read(&cfg.image).map_err(|e| format!("Gambar sumber hilang: {e}"))?;
    let img = crate::loader::decode_rgba(&bytes, 8192, 40_000_000)?;
    apply_root(&img, cfg.style, cfg.color)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32, px: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba(px))
    }

    #[test]
    fn warna_hex_bolak_balik_dan_penolakan_input_buruk() {
        assert_eq!(color_hex([255, 0, 16]), "#ff0010");
        assert_eq!(parse_hex("#FF0010"), Some([255, 0, 16]));
        assert_eq!(parse_hex("ff0010"), Some([255, 0, 16]));
        assert_eq!(parse_hex(" #abc "), None);
        assert_eq!(parse_hex("#gg0000"), None);
        assert_eq!(parse_hex("#é00000"), None);
        assert_eq!(WpStyle::parse("zoomed"), Some(WpStyle::Zoomed));
        assert_eq!(WpStyle::parse("TILED"), Some(WpStyle::Tiled));
        assert_eq!(WpStyle::parse("kacau"), None);
    }

    #[test]
    fn tata_letak_setiap_gaya() {
        // gambar 200x100 pada layar 400x400
        let (iw, ih, sw, sh) = (200.0, 100.0, 400.0, 400.0);
        assert_eq!(place(WpStyle::None, iw, ih, sw, sh), None);
        let c = place(WpStyle::Centered, iw, ih, sw, sh).unwrap();
        assert_eq!((c.x, c.y, c.w, c.h), (100.0, 150.0, 200.0, 100.0));
        let t = place(WpStyle::Tiled, iw, ih, sw, sh).unwrap();
        assert!(t.tiled && (t.x, t.y, t.w, t.h) == (0.0, 0.0, 200.0, 100.0));
        let st = place(WpStyle::Stretched, iw, ih, sw, sh).unwrap();
        assert_eq!((st.x, st.y, st.w, st.h), (0.0, 0.0, 400.0, 400.0));
        let sc = place(WpStyle::Scaled, iw, ih, sw, sh).unwrap(); // muat tanpa terpotong
        assert_eq!((sc.x, sc.y, sc.w, sc.h), (0.0, 100.0, 400.0, 200.0));
        let z = place(WpStyle::Zoomed, iw, ih, sw, sh).unwrap(); // penuh, terpotong
        assert_eq!((z.x, z.y, z.w, z.h), (-200.0, 0.0, 800.0, 400.0));
        assert_eq!(place(WpStyle::Zoomed, 0.0, 10.0, 10.0, 10.0), None);
        assert_eq!(place(WpStyle::Zoomed, 10.0, 10.0, 0.0, 10.0), None);
    }

    #[test]
    fn renderer_menggambar_warna_latar_dan_posisi_yang_benar() {
        let red = img(2, 2, [255, 0, 0, 255]);
        let bg = [0, 0, 255];
        // None: hanya warna latar
        let c = render_canvas(&red, WpStyle::None, bg, 6, 4).unwrap();
        assert!(c.pixels().all(|p| p.0 == [0, 0, 255, 255]));
        // Centered: kotak merah 2x2 di tengah kanvas 6x4 -> piksel (2..4, 1..3)
        let c = render_canvas(&red, WpStyle::Centered, bg, 6, 4).unwrap();
        assert_eq!(c.get_pixel(2, 1).0, [255, 0, 0, 255]);
        assert_eq!(c.get_pixel(3, 2).0, [255, 0, 0, 255]);
        assert_eq!(c.get_pixel(0, 0).0, [0, 0, 255, 255]);
        assert_eq!(c.get_pixel(5, 3).0, [0, 0, 255, 255]);
        // Stretched: seluruh kanvas merah
        let c = render_canvas(&red, WpStyle::Stretched, bg, 6, 4).unwrap();
        assert!(c.pixels().all(|p| p.0 == [255, 0, 0, 255]));
        // Scaled: gambar 2x2 pada 6x4 -> 4x4 di tengah, bilah biru di kiri/kanan
        let c = render_canvas(&red, WpStyle::Scaled, bg, 6, 4).unwrap();
        assert_eq!(c.get_pixel(0, 2).0, [0, 0, 255, 255]);
        assert_eq!(c.get_pixel(3, 2).0, [255, 0, 0, 255]);
        // Zoomed: penuh tanpa bilah
        let c = render_canvas(&red, WpStyle::Zoomed, bg, 6, 4).unwrap();
        assert!(c.pixels().all(|p| p.0 == [255, 0, 0, 255]));
    }

    #[test]
    fn renderer_ubin_mengulang_pola_dan_alpha_dicampur_dengan_latar() {
        let mut tile = RgbaImage::new(2, 1);
        tile.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        tile.put_pixel(1, 0, Rgba([0, 255, 0, 255]));
        let c = render_canvas(&tile, WpStyle::Tiled, [0, 0, 0], 5, 2).unwrap();
        let row: Vec<_> = (0..5).map(|x| c.get_pixel(x, 1).0[..3].to_vec()).collect();
        assert_eq!(row[0], vec![255, 0, 0]);
        assert_eq!(row[1], vec![0, 255, 0]);
        assert_eq!(row[2], vec![255, 0, 0]);
        assert_eq!(row[4], vec![255, 0, 0]);
        // setengah transparan di atas latar putih
        let half = img(1, 1, [0, 0, 0, 128]);
        let c = render_canvas(&half, WpStyle::Stretched, [255, 255, 255], 2, 2).unwrap();
        let v = c.get_pixel(0, 0).0[0];
        assert!((125..=130).contains(&v), "{v}");
        assert_eq!(c.get_pixel(0, 0).0[3], 255);
    }

    #[test]
    fn renderer_menolak_hasil_skala_raksasa_dan_layar_nol() {
        let tiny = img(1, 1, [1, 2, 3, 255]);
        assert!(render_canvas(&tiny, WpStyle::Stretched, [0; 3], 0, 10).is_err());
        assert!(render_canvas(&tiny, WpStyle::Zoomed, [0; 3], 20_000, 10_000).is_err());
    }

    fn env(desktops: &[&str], display: bool, wayland: bool, sway: bool) -> Env {
        Env {
            desktops: desktops.iter().map(|s| s.to_string()).collect(),
            display,
            wayland_session: wayland,
            swaysock: sway,
        }
    }

    #[test]
    fn deteksi_desktop_untuk_banyak_lingkungan() {
        let all = |_: &str| true;
        let none = |_: &str| false;
        let cases: Vec<(Env, Backend)> = vec![
            (env(&["xfce"], true, false, false), Backend::Xfce),
            (env(&["x-cinnamon", "cinnamon"], true, false, false), Backend::Cinnamon),
            (env(&["mate"], true, false, false), Backend::Mate),
            (env(&["ubuntu", "gnome"], true, true, false), Backend::Gnome),
            (env(&["kde"], true, true, false), Backend::Kde),
            (env(&["lxqt"], true, false, false), Backend::LxQt),
            (env(&["lxde"], true, false, false), Backend::Lxde),
            (env(&["fluxbox"], true, false, false), Backend::X11Root),
            (env(&[], true, false, false), Backend::X11Root),
            (env(&[], false, true, true), Backend::Sway),
        ];
        for (e, want) in cases {
            assert_eq!(detect_with(&e, &all), want, "{e:?}");
        }
        assert!(matches!(detect_with(&env(&["xfce"], true, false, false), &none), Backend::Unsupported(_)));
        assert!(matches!(detect_with(&env(&["hyprland"], true, true, false), &all), Backend::Unsupported(_)));
        assert!(matches!(detect_with(&env(&[], false, false, false), &all), Backend::Unsupported(_)));
        assert!(!Backend::Unsupported("x".into()).supported());
    }

    #[test]
    fn properti_xfce_sesuai_nama_dan_nilai_xfdesktop() {
        let p = xfce_props("/backdrop/screen0/monitorA/workspace0", "/a/b.png", WpStyle::Zoomed, ColorMode::Solid, [255, 0, 51]);
        let get = |n: &str| p.iter().find(|x| x.path.ends_with(n)).unwrap().clone();
        assert_eq!(get("/last-image").values, vec!["/a/b.png"]);
        assert_eq!(get("/image-style").values, vec!["5"]);
        assert_eq!(get("/image-show").values, vec!["true"]);
        assert_eq!(get("/color-style").values, vec!["0"]);
        assert_eq!(get("/rgba1").values, vec!["1.000000", "0.000000", "0.200000", "1.000000"]);
        let t = xfce_props("/x", "/f.png", WpStyle::None, ColorMode::Transparent, [0; 3]);
        let g = |n: &str| t.iter().find(|x| x.path.ends_with(n)).unwrap().values.clone();
        assert_eq!(g("/image-style"), vec!["0"]);
        assert_eq!(g("/image-show"), vec!["false"]);
        assert_eq!(g("/color-style"), vec!["3"]);
        for (s, code) in [(WpStyle::Centered, "1"), (WpStyle::Tiled, "2"), (WpStyle::Stretched, "3"), (WpStyle::Scaled, "4")] {
            let q = xfce_props("/x", "/f", s, ColorMode::Solid, [0; 3]);
            assert_eq!(q.iter().find(|x| x.path.ends_with("/image-style")).unwrap().values, vec![code]);
        }
    }

    #[test]
    fn perintah_gsettings_dan_uri_berkas() {
        assert_eq!(file_uri("/home/a b/ü.png"), "file:///home/a%20b/%C3%BC.png");
        let g = gsettings_cmds(&Backend::Gnome, "/x/y.jpg", WpStyle::Tiled, [1, 2, 3]);
        let flat: Vec<String> = g.iter().map(|c| c.join(" ")).collect();
        assert!(flat.contains(&"gsettings set org.gnome.desktop.background picture-uri file:///x/y.jpg".to_string()));
        assert!(flat.contains(&"gsettings set org.gnome.desktop.background picture-options wallpaper".to_string()));
        assert!(flat.contains(&"gsettings set org.gnome.desktop.background primary-color #010203".to_string()));
        let m = gsettings_cmds(&Backend::Mate, "/x/y.jpg", WpStyle::Zoomed, [0; 3]);
        assert!(m.iter().any(|c| c[2] == "org.mate.background" && c[3] == "picture-filename" && c[4] == "/x/y.jpg"));
        assert!(m.iter().any(|c| c[3] == "picture-options" && c[4] == "zoom"));
        let c = gsettings_cmds(&Backend::Cinnamon, "/x/y.jpg", WpStyle::None, [0; 3]);
        assert!(c.iter().all(|c| c[2] == "org.cinnamon.desktop.background"));
        assert!(c.iter().any(|c| c[3] == "picture-options" && c[4] == "none"));
    }

    #[test]
    fn skrip_kde_dan_perintah_lxde_lxqt_sway() {
        let k = kde_script("/a \"b\"/c.png", WpStyle::Zoomed, [255, 0, 0]);
        assert!(k.contains("org.kde.image") && k.contains("FillMode\", 2"), "{k}");
        assert!(k.contains("file:///a%20%22b%22/c.png"), "{k}");
        assert!(k.contains("\"#ff0000\""));
        assert!(kde_script("/a.png", WpStyle::Centered, [0; 3]).contains("FillMode\", 6"));
        assert!(kde_script("/a.png", WpStyle::Tiled, [0; 3]).contains("FillMode\", 3"));
        let none = kde_script("/a.png", WpStyle::None, [1, 2, 3]);
        assert!(none.contains("org.kde.color") && !none.contains("org.kde.image"));
        assert_eq!(js_str("a\"b\\c"), "\"a\\\"b\\\\c\"");

        assert_eq!(pcmanfm_cmd(false, "/f.png", WpStyle::Zoomed), vec!["pcmanfm", "--set-wallpaper=/f.png", "--wallpaper-mode=crop"]);
        assert_eq!(pcmanfm_cmd(true, "/f.png", WpStyle::Zoomed)[2], "--wallpaper-mode=zoom");
        assert_eq!(pcmanfm_cmd(true, "/f.png", WpStyle::Scaled)[2], "--wallpaper-mode=fit");
        assert_eq!(sway_cmd("/f.png", WpStyle::Scaled, [0, 0, 0]), vec!["swaymsg", "output", "*", "bg", "/f.png", "fit", "#000000"]);
        assert_eq!(sway_cmd("/f.png", WpStyle::None, [255, 255, 255]), vec!["swaymsg", "output", "*", "bg", "#ffffff", "solid_color"]);
    }

    #[test]
    fn menjalankan_perintah_menangani_sukses_gagal_dan_waktu_habis() {
        let ok = run(&["echo".into(), "halo".into()], Duration::from_secs(5)).unwrap();
        assert_eq!(ok.trim(), "halo");
        let err = run(&["sh".into(), "-c".into(), "echo rusak >&2; exit 3".into()], Duration::from_secs(5)).unwrap_err();
        assert!(err.contains("(3)") && err.contains("rusak"), "{err}");
        let t = run(&["sleep".into(), "5".into()], Duration::from_millis(200)).unwrap_err();
        assert!(t.contains("waktu habis"), "{t}");
        assert!(run(&["/tidak/ada/perintah".into()], Duration::from_secs(1)).is_err());
        assert!(run(&[], Duration::from_secs(1)).is_err());
    }

    #[test]
    fn ekstensi_aman_untuk_desktop() {
        assert!(is_desktop_safe_ext(Path::new("/a/B.JPG")));
        assert!(is_desktop_safe_ext(Path::new("/a/b.png")));
        assert!(!is_desktop_safe_ext(Path::new("/a/b.heic")));
        assert!(!is_desktop_safe_ext(Path::new("/a/b.webp")));
        assert_eq!(desktop_exec_quote("/a b/\"c\""), "\"/a b/\\\"c\\\"\"");
    }
}
