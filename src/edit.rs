//! Logika murni perkakas edit (crop, resize, color picker, tone mapping, PDF).
//! Tanpa ketergantungan pada state aplikasi, supaya mudah dites.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, RgbaImage};

// --------------------------------------------------------------------- warna

/// sRGB (0..255) -> HSL (h 0..360, s/l 0..1).
pub fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

/// sRGB (0..255) -> HSV (h 0..360, s/v 0..1).
pub fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let s = if max == 0.0 { 0.0 } else { d / max };
    if d.abs() < f32::EPSILON {
        return (0.0, s, max);
    }
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, max)
}

/// sRGB (0..255) -> CMYK (0..1).
pub fn rgb_to_cmyk(r: u8, g: u8, b: u8) -> (f32, f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let k = 1.0 - r.max(g).max(b);
    if k >= 1.0 {
        return (0.0, 0.0, 0.0, 1.0);
    }
    ((1.0 - r - k) / (1.0 - k), (1.0 - g - k) / (1.0 - k), (1.0 - b - k) / (1.0 - k), k)
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// sRGB (0..255) -> CIELAB (D65). L 0..100, a/b sekitar -128..127.
pub fn rgb_to_lab(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (
        srgb_to_linear(r as f32 / 255.0),
        srgb_to_linear(g as f32 / 255.0),
        srgb_to_linear(b as f32 / 255.0),
    );
    // sRGB -> XYZ (D65), dikali 100.
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) * 100.0;
    let y = (0.2126 * r + 0.7152 * g + 0.0722 * b) * 100.0;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) * 100.0;
    // XYZ -> Lab, white point D65 (95.047, 100, 108.883).
    let f = |t: f32| {
        if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 }
    };
    let (fx, fy, fz) = (f(x / 95.047), f(y / 100.0), f(z / 108.883));
    (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}

/// "#RRGGBB" (atau "#RRGGBBAA" bila `with_alpha` dan alpha < 255).
pub fn fmt_hex(r: u8, g: u8, b: u8, a: u8, with_alpha: bool) -> String {
    if with_alpha && a < 255 {
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    } else {
        format!("#{r:02X}{g:02X}{b:02X}")
    }
}

// ---------------------------------------------------------------------- crop

/// Seret dari `a` ke `b` (koordinat gambar, boleh di luar) -> persegi
/// [x, y, w, h] yang dinormalisasi dan dijepit ke `bounds` (w, h gambar).
/// Mengembalikan `None` bila seleksi terlalu kecil (< 2 px).
pub fn drag_rect(a: (f32, f32), b: (f32, f32), bounds: (u32, u32)) -> Option<[u32; 4]> {
    let x0 = a.0.min(b.0).clamp(0.0, bounds.0 as f32);
    let y0 = a.1.min(b.1).clamp(0.0, bounds.1 as f32);
    let x1 = a.0.max(b.0).clamp(0.0, bounds.0 as f32);
    let y1 = a.1.max(b.1).clamp(0.0, bounds.1 as f32);
    if x1 - x0 < 2.0 || y1 - y0 < 2.0 {
        return None;
    }
    Some([x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32])
}

/// Terapkan rasio aspek ke persegi: persegi terbesar dengan rasio `ratio`
/// (w/h, `None` = bebas) di dalam `rect`, berpusat sama, dijepit ke `bounds`.
pub fn apply_aspect(rect: [u32; 4], ratio: Option<f32>, bounds: (u32, u32)) -> [u32; 4] {
    let Some(ratio) = ratio.filter(|r| *r > 0.0 && r.is_finite()) else {
        return rect;
    };
    let (x, y, w, h) = (rect[0] as f32, rect[1] as f32, rect[2] as f32, rect[3] as f32);
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let (mut nw, mut nh) = if w / h > ratio { (h * ratio, h) } else { (w, w / ratio) };
    // Jepit ke dalam gambar, jaga pusat.
    nw = nw.min(bounds.0 as f32);
    nh = nh.min(bounds.1 as f32);
    if nw / nh > ratio {
        nw = nh * ratio;
    } else {
        nh = nw / ratio;
    }
    let nx = (cx - nw / 2.0).clamp(0.0, (bounds.0 as f32 - nw).max(0.0));
    let ny = (cy - nh / 2.0).clamp(0.0, (bounds.1 as f32 - nh).max(0.0));
    [nx as u32, ny as u32, nw.max(1.0) as u32, nh.max(1.0) as u32]
}

/// Geser persegi `rect` sejauh `delta` (koordinat gambar), dijepit ke `bounds`.
/// Ukuran tidak berubah.
pub fn move_rect(rect: [u32; 4], delta: (f32, f32), bounds: (u32, u32)) -> [u32; 4] {
    let (bw, bh) = (bounds.0 as f32, bounds.1 as f32);
    let (w, h) = (rect[2] as f32, rect[3] as f32);
    let nx = (rect[0] as f32 + delta.0).clamp(0.0, (bw - w).max(0.0));
    let ny = (rect[1] as f32 + delta.1).clamp(0.0, (bh - h).max(0.0));
    [nx as u32, ny as u32, rect[2], rect[3]]
}

/// Ubah ukuran seleksi dari sebuah sudut: `anchor` adalah sudut berlawanan
/// yang tetap, `p` posisi pointer (koordinat gambar). Bila `ratio` (w/h) ada,
/// kotak menjaga rasio berjangkar di `anchor`. Hasil dijepit ke `bounds`.
pub fn resize_crop_corner(
    anchor: (f32, f32),
    p: (f32, f32),
    ratio: Option<f32>,
    bounds: (u32, u32),
) -> [u32; 4] {
    let (bw, bh) = (bounds.0 as f32, bounds.1 as f32);
    let px = p.0.clamp(0.0, bw);
    let py = p.1.clamp(0.0, bh);
    let mut w = (px - anchor.0).abs();
    let mut h = (py - anchor.1).abs();
    if let Some(r) = ratio.filter(|r| *r > 0.0 && r.is_finite()) {
        // Muat kotak berasio di dalam daerah seret.
        if w / r <= h {
            h = w / r;
        } else {
            w = h * r;
        }
    }
    w = w.max(1.0);
    h = h.max(1.0);
    let sx: f32 = if px >= anchor.0 { 1.0 } else { -1.0 };
    let sy: f32 = if py >= anchor.1 { 1.0 } else { -1.0 };
    let fx = anchor.0 + sx * w;
    let fy = anchor.1 + sy * h;
    let x = anchor.0.min(fx).clamp(0.0, bw);
    let y = anchor.1.min(fy).clamp(0.0, bh);
    let x2 = anchor.0.max(fx).clamp(0.0, bw);
    let y2 = anchor.1.max(fy).clamp(0.0, bh);
    [x as u32, y as u32, (x2 - x).max(1.0) as u32, (y2 - y).max(1.0) as u32]
}

/// Sisi seleksi crop yang diseret.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CropEdge {
    Left,
    Right,
    Top,
    Bottom,
}

/// Ubah ukuran seleksi dari sebuah sisi: `p` posisi pointer (koordinat gambar).
/// Sisi berlawanan tetap; bila `ratio` (w/h) ada, dimensi tegak lurus
/// menyesuaikan rasio dan dipusatkan pada posisi semula. Hasil dijepit ke `bounds`.
pub fn resize_crop_edge(
    rect: [u32; 4],
    edge: CropEdge,
    p: (f32, f32),
    ratio: Option<f32>,
    bounds: (u32, u32),
) -> [u32; 4] {
    let (bw, bh) = (bounds.0 as f32, bounds.1 as f32);
    let (x, y, w, h) = (rect[0] as f32, rect[1] as f32, rect[2] as f32, rect[3] as f32);
    let r = ratio.filter(|r| *r > 0.0 && r.is_finite());
    match edge {
        CropEdge::Left | CropEdge::Right => {
            let fixed = if edge == CropEdge::Left { x + w } else { x };
            let px = p.0.clamp(0.0, bw);
            let mut nw = (fixed - px).abs().max(1.0);
            let mut nh = h;
            if let Some(r) = r {
                nh = (nw / r).min(bh).max(1.0);
                nw = nh * r;
            }
            let cy = y + h / 2.0;
            let ny = (cy - nh / 2.0).clamp(0.0, (bh - nh).max(0.0));
            // Sisi yang diseret (disesuaikan kembali bila beraspek).
            let dragged = if edge == CropEdge::Left { fixed - nw } else { fixed + nw };
            let x0 = fixed.min(dragged).clamp(0.0, bw);
            let x1 = fixed.max(dragged).clamp(0.0, bw);
            [x0 as u32, ny as u32, (x1 - x0).max(1.0) as u32, nh.max(1.0) as u32]
        }
        CropEdge::Top | CropEdge::Bottom => {
            let fixed = if edge == CropEdge::Top { y + h } else { y };
            let py = p.1.clamp(0.0, bh);
            let mut nh = (fixed - py).abs().max(1.0);
            let mut nw = w;
            if let Some(r) = r {
                nw = (nh * r).min(bw).max(1.0);
                nh = nw / r;
            }
            let cx = x + w / 2.0;
            let nx = (cx - nw / 2.0).clamp(0.0, (bw - nw).max(0.0));
            let dragged = if edge == CropEdge::Top { fixed - nh } else { fixed + nh };
            let y0 = fixed.min(dragged).clamp(0.0, bh);
            let y1 = fixed.max(dragged).clamp(0.0, bh);
            [nx as u32, y0 as u32, nw.max(1.0) as u32, (y1 - y0).max(1.0) as u32]
        }
    }
}

/// Potong `img` ke `rect` [x, y, w, h] (dijepit ke dalam gambar).
pub fn crop_image(img: &RgbaImage, rect: [u32; 4]) -> RgbaImage {
    let (iw, ih) = (img.width(), img.height());
    let x = rect[0].min(iw.saturating_sub(1));
    let y = rect[1].min(ih.saturating_sub(1));
    let w = rect[2].min(iw - x).max(1);
    let h = rect[3].min(ih - y).max(1);
    let mut out = RgbaImage::new(w, h);
    for yy in 0..h {
        for xx in 0..w {
            out.put_pixel(xx, yy, *img.get_pixel(x + xx, y + yy));
        }
    }
    out
}

// -------------------------------------------------------------------- resize

/// Dimensi baru bila hanya satu sisi diubah dan aspek dikunci.
pub fn resize_keep_aspect(
    orig_w: u32,
    orig_h: u32,
    new_w: Option<u32>,
    new_h: Option<u32>,
) -> (u32, u32) {
    if orig_w == 0 || orig_h == 0 {
        return (orig_w, orig_h);
    }
    match (new_w, new_h) {
        (Some(w), Some(h)) => (w.max(1), h.max(1)),
        (Some(w), None) => (w.max(1), ((w as u64 * orig_h as u64) / orig_w.max(1) as u64).max(1) as u32),
        (None, Some(h)) => (((h as u64 * orig_w as u64) / orig_h.max(1) as u64).max(1) as u32, h.max(1)),
        (None, None) => (orig_w, orig_h),
    }
}

pub fn resize_image(img: &RgbaImage, w: u32, h: u32, filter: FilterType) -> RgbaImage {
    let dyn_img = DynamicImage::ImageRgba8(img.clone());
    dyn_img.resize(w.max(1), h.max(1), filter).into_rgba8()
}

// ---------------------------------------------------------------- tone map

/// Parameter tone mapping. Nilai bawaan = identitas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneParams {
    /// EV: -3..2, 0 = tidak berubah.
    pub exposure: f32,
    /// Putih referensi: 0.5..2, 1 = tidak berubah.
    pub reference_white: f32,
    /// Kompresi highlight: 0..1, 0 = tanpa kompresi (clip keras).
    pub highlight: f32,
    /// Saturasi: 0 = abu-abu, 1 = asli, 2 = jenuh ganda.
    pub saturation: f32,
}

impl Default for ToneParams {
    fn default() -> Self {
        ToneParams { exposure: 0.0, reference_white: 1.0, highlight: 0.0, saturation: 1.0 }
    }
}

/// LUT sRGB byte -> linier float. Dibangun sekali per proses; mengubah 3x `powf`
/// per piksel menjadi 3x lookup saat tone mapping.
fn srgb_to_linear_lut() -> &'static [f32; 256] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0.0f32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            *v = srgb_to_linear(i as f32 / 255.0);
        }
        t
    })
}

/// LUT linier terkuantisasi (0..1 dalam 4096 langkah) -> sRGB byte.
/// Akurasi < 1 nilai sRGB dari versi `powf`; cukup untuk preview maupun hasil akhir.
fn linear_to_srgb_lut() -> &'static [u8; 4097] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[u8; 4097]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0u8; 4097];
        for (i, v) in t.iter_mut().enumerate() {
            *v = (linear_to_srgb(i as f32 / 4096.0) * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        t
    })
}

/// Parameter tone yang sudah diurai: gain dan faktor kompresi highlight.
/// Dihitung sekali per pemanggilan, bukan per piksel.
fn tone_factors(p: &ToneParams) -> (f32, f32) {
    let gain = 2f32.powf(p.exposure) / p.reference_white.max(0.05);
    let k = p.highlight * 4.0;
    (gain, k)
}

/// Terapkan gain + kompresi highlight + clamp ke satu kanal linier.
#[inline]
fn tone_channel(v: f32, gain: f32, k: f32) -> f32 {
    let mut v = v * gain;
    if k > 0.0 {
        v = v / (1.0 + k * v);
    }
    v.clamp(0.0, 1.0)
}

/// Tone-map satu piksel LINIER (0..tidak terbatas) -> sRGB 0..1.
pub fn tone_map_linear_pixel(rgb: [f32; 3], p: &ToneParams) -> [f32; 3] {
    let (gain, k) = tone_factors(p);
    let mut out = [0.0f32; 3];
    let mut luma = 0.0;
    for (i, c) in rgb.iter().enumerate() {
        let v = tone_channel(*c, gain, k);
        out[i] = v;
        luma += [0.2126, 0.7152, 0.0722][i] * v;
    }
    // Saturasi di sekitar luma.
    for v in out.iter_mut() {
        *v = (luma + p.saturation * (*v - luma)).clamp(0.0, 1.0);
    }
    // Linear -> sRGB untuk ditampilkan/disimpan.
    out.map(linear_to_srgb)
}

/// Tone-map gambar sRGB 8-bit: sRGB -> linier -> tone -> sRGB.
/// Versi cepat: konversi sRGB<->linier lewat LUT (bukan `powf` per piksel).
pub fn tone_map_srgb(img: &RgbaImage, p: &ToneParams) -> RgbaImage {
    let to_lin = srgb_to_linear_lut();
    let to_srgb = linear_to_srgb_lut();
    let (gain, k) = tone_factors(p);
    let sat = p.saturation;
    let mut out = RgbaImage::new(img.width(), img.height());
    for (dst, src) in out.pixels_mut().zip(img.pixels()) {
        let r = tone_channel(to_lin[src[0] as usize], gain, k);
        let g = tone_channel(to_lin[src[1] as usize], gain, k);
        let b = tone_channel(to_lin[src[2] as usize], gain, k);
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        // Saturasi di sekitar luma, lalu kuantisasi + LUT ke sRGB byte.
        let q = |v: f32| {
            let v = (luma + sat * (v - luma)).clamp(0.0, 1.0);
            to_srgb[(v * 4096.0).round() as usize]
        };
        *dst = image::Rgba([q(r), q(g), q(b), src[3]]);
    }
    out
}

/// Tone-map buffer HDR linier (RGB float, 3 kanal per piksel) -> RGBA8.
pub fn tone_map_linear_buffer(rgb: &[f32], w: u32, h: u32, p: &ToneParams) -> RgbaImage {
    let to_srgb = linear_to_srgb_lut();
    let (gain, k) = tone_factors(p);
    let sat = p.saturation;
    let mut out = RgbaImage::new(w, h);
    for (i, px) in out.pixels_mut().enumerate() {
        let r = tone_channel(rgb[i * 3], gain, k);
        let g = tone_channel(rgb[i * 3 + 1], gain, k);
        let b = tone_channel(rgb[i * 3 + 2], gain, k);
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let q = |v: f32| {
            let v = (luma + sat * (v - luma)).clamp(0.0, 1.0);
            to_srgb[(v * 4096.0).round() as usize]
        };
        *px = image::Rgba([q(r), q(g), q(b), 255]);
    }
    out
}

// ----------------------------------------------------------------------- PDF

/// Encode gambar sebagai JPEG (kualitas 0..100) untuk disisipkan ke PDF.
pub fn jpeg_encode(img: &RgbaImage, quality: u8) -> Result<Vec<u8>, String> {
    // JPEG tidak punya alpha: gabungkan ke latar putih.
    let mut rgb = image::RgbImage::new(img.width(), img.height());
    for (x, y, px) in img.enumerate_pixels() {
        let a = px[3] as f32 / 255.0;
        rgb.put_pixel(
            x,
            y,
            image::Rgb([
                (px[0] as f32 * a + 255.0 * (1.0 - a)) as u8,
                (px[1] as f32 * a + 255.0 * (1.0 - a)) as u8,
                (px[2] as f32 * a + 255.0 * (1.0 - a)) as u8,
            ]),
        );
    }
    let mut buf = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut buf, quality)
        .encode_image(&DynamicImage::ImageRgb8(rgb))
        .map_err(|e| format!("Gagal encode JPEG: {e}"))?;
    Ok(buf.into_inner())
}

/// Satu gambar di dalam satu halaman PDF: posisi/ukuran dalam point (1/72 inci).
pub struct PdfSlot {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub jpeg: Vec<u8>,
    pub img_w: u32,
    pub img_h: u32,
}

pub struct PdfPage {
    pub width: f32,
    pub height: f32,
    pub slots: Vec<PdfSlot>,
}

/// Bangun dokumen PDF 1.4 minimal dengan gambar JPEG (DCTDecode).
/// Setiap halaman memakai koordinat PDF standar (asal di kiri bawah).
pub fn build_pdf(pages: &[PdfPage]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut offsets: Vec<usize> = Vec::new();
    let push = |out: &mut Vec<u8>, s: &str| out.extend_from_slice(s.as_bytes());

    push(&mut out, "%PDF-1.4\n");
    out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");
    // Objek 1: katalog. Objek 2: daftar halaman. Gambar & halaman menyusul.
    let mut page_obj_ids: Vec<usize> = Vec::new();
    let mut next_id = 3usize;
    // Rencanakan nomor objek dulu: tiap halaman = 1 objek halaman + 1 stream
    // konten + N objek gambar.
    let mut page_objs: Vec<(usize, usize, Vec<usize>)> = Vec::new();
    for page in pages {
        let page_id = next_id;
        next_id += 1;
        let content_id = next_id;
        next_id += 1;
        let mut img_ids = Vec::new();
        for _ in &page.slots {
            img_ids.push(next_id);
            next_id += 1;
        }
        page_objs.push((page_id, content_id, img_ids));
        page_obj_ids.push(page_id);
    }
    let total = next_id - 1;

    // 1: katalog.
    offsets.push(out.len());
    push(&mut out, "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    // 2: daftar halaman.
    offsets.push(out.len());
    let kids: Vec<String> = page_obj_ids.iter().map(|i| format!("{i} 0 R")).collect();
    push(
        &mut out,
        &format!("2 0 obj\n<< /Type /Pages /Kids [{}] /Count {} >>\nendobj\n", kids.join(" "), pages.len()),
    );
    // Halaman, stream konten, gambar.
    for (pi, page) in pages.iter().enumerate() {
        let (page_id, content_id, img_ids) = &page_objs[pi];
        let xobjects: Vec<String> = img_ids
            .iter()
            .enumerate()
            .map(|(i, id)| format!("/Im{i} {id} 0 R"))
            .collect();
        offsets.push(out.len());
        push(
            &mut out,
            &format!(
                "{page_id} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /XObject << {} >> >> /Contents {content_id} 0 R >>\nendobj\n",
                page.width, page.height, xobjects.join(" ")
            ),
        );
        // Stream konten: gambar tiap slot.
        let mut content = String::new();
        for (i, slot) in page.slots.iter().enumerate() {
            content.push_str(&format!(
                "q {} 0 0 {} {} {} cm /Im{i} Do Q\n",
                slot.w, slot.h, slot.x, slot.y
            ));
        }
        offsets.push(out.len());
        push(
            &mut out,
            &format!("{content_id} 0 obj\n<< /Length {} >>\nstream\n{content}\nendstream\nendobj\n", content.len()),
        );
        // Objek gambar JPEG.
        for (i, slot) in page.slots.iter().enumerate() {
            offsets.push(out.len());
            push(
                &mut out,
                &format!(
                    "{} 0 obj\n<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
                    img_ids[i], slot.img_w, slot.img_h, slot.jpeg.len()
                ),
            );
            out.extend_from_slice(&slot.jpeg);
            push(&mut out, "\nendstream\nendobj\n");
        }
    }
    // Tabel xref.
    let xref_pos = out.len();
    push(&mut out, &format!("xref\n0 {}\n", total + 1));
    push(&mut out, "0000000000 65535 f \n");
    for off in &offsets {
        push(&mut out, &format!("{off:010} 00000 n \n"));
    }
    push(
        &mut out,
        &format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n", total + 1),
    );
    out
}

// ----------------------------------------------------------------------- tes

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn konversi_warna_benar() {
        // Merah murni.
        let (h, s, l) = rgb_to_hsl(255, 0, 0);
        assert!(approx(h, 0.0, 0.5) && approx(s, 1.0, 0.01) && approx(l, 0.5, 0.01));
        let (h, s, v) = rgb_to_hsv(255, 0, 0);
        assert!(approx(h, 0.0, 0.5) && approx(s, 1.0, 0.01) && approx(v, 1.0, 0.01));
        let (c, m, y, k) = rgb_to_cmyk(255, 0, 0);
        assert!(approx(c, 0.0, 0.01) && approx(m, 1.0, 0.01) && approx(y, 1.0, 0.01) && approx(k, 0.0, 0.01));
        // Putih -> Lab (100, ~0, ~0).
        let (l, a, b) = rgb_to_lab(255, 255, 255);
        assert!(approx(l, 100.0, 0.5) && approx(a, 0.0, 0.5) && approx(b, 0.0, 0.5));
        // Hitam -> Lab (0, 0, 0).
        let (l, a, b) = rgb_to_lab(0, 0, 0);
        assert!(approx(l, 0.0, 0.5) && approx(a, 0.0, 0.5) && approx(b, 0.0, 0.5));
        assert_eq!(fmt_hex(255, 0, 128, 255, true), "#FF0080");
        assert_eq!(fmt_hex(255, 0, 128, 128, true), "#FF008080");
    }

    #[test]
    fn crop_math_benar() {
        // Seret terbalik + di luar gambar -> dijepit.
        assert_eq!(drag_rect((90.0, 90.0), (10.0, 10.0), (100, 100)), Some([10, 10, 80, 80]));
        assert_eq!(drag_rect((0.0, 0.0), (1.0, 1.0), (100, 100)), None, "terlalu kecil");
        // Rasio 1:1 dari 100x50 -> 50x50 di tengah.
        assert_eq!(apply_aspect([0, 0, 100, 50], Some(1.0), (100, 50)), [25, 0, 50, 50]);
        // Bebas -> tidak berubah.
        assert_eq!(apply_aspect([0, 0, 100, 50], None, (100, 50)), [0, 0, 100, 50]);
        // Crop nyata.
        let img = RgbaImage::from_pixel(10, 10, image::Rgba([7, 8, 9, 255]));
        let c = crop_image(&img, [2, 3, 4, 5]);
        assert_eq!((c.width(), c.height()), (4, 5));
        assert_eq!(c.get_pixel(0, 0), &image::Rgba([7, 8, 9, 255]));
    }

    #[test]
    fn crop_pindah_dan_ubah_ukuran() {
        // Geser: ukuran tetap, dijepit ke dalam gambar.
        assert_eq!(move_rect([10, 10, 20, 20], (5.0, -3.0), (100, 100)), [15, 7, 20, 20]);
        assert_eq!(move_rect([10, 10, 20, 20], (-50.0, 0.0), (100, 100)), [0, 10, 20, 20]);
        assert_eq!(move_rect([80, 80, 20, 20], (50.0, 50.0), (100, 100)), [80, 80, 20, 20]);
        // Sudut bebas: jangkar kiri-atas tetap.
        assert_eq!(
            resize_crop_corner((10.0, 10.0), (50.0, 40.0), None, (100, 100)),
            [10, 10, 40, 30]
        );
        // Sudut 1:1: jangkar tetap, rasio dijaga.
        let r = resize_crop_corner((10.0, 10.0), (50.0, 40.0), Some(1.0), (100, 100));
        assert_eq!([r[0], r[1]], [10, 10]);
        assert_eq!(r[2], r[3], "harus persegi");
        assert!((r[2] as f32 - 30.0).abs() < 1.5, "w mengikuti sisi terkecil");
        // Sisi kanan bebas: sisi kiri tetap.
        assert_eq!(
            resize_crop_edge([10, 20, 30, 40], CropEdge::Right, (60.0, 0.0), None, (100, 100)),
            [10, 20, 50, 40]
        );
        // Sisi kanan 1:1: tinggi menyesuaikan, dipusatkan vertikal.
        let e = resize_crop_edge([10, 20, 30, 40], CropEdge::Right, (60.0, 0.0), Some(1.0), (200, 200));
        assert_eq!(e[2], e[3], "harus persegi");
        assert_eq!(e[0], 10, "sisi kiri tetap");
    }

    #[test]
    fn resize_math_benar() {
        assert_eq!(resize_keep_aspect(800, 600, Some(400), None), (400, 300));
        assert_eq!(resize_keep_aspect(800, 600, None, Some(300)), (400, 300));
        let img = RgbaImage::from_pixel(10, 10, image::Rgba([9, 9, 9, 255]));
        let r = resize_image(&img, 5, 5, FilterType::Nearest);
        assert_eq!((r.width(), r.height()), (5, 5));
    }

    #[test]
    fn tone_map_identitas_dan_eksposur() {
        let img = RgbaImage::from_pixel(4, 4, image::Rgba([128, 128, 128, 255]));
        let id = tone_map_srgb(&img, &ToneParams::default());
        // Identitas (toleransi pembulatan sRGB).
        assert!((id.get_pixel(0, 0)[0] as i32 - 128).abs() <= 2);
        // +1 EV membuat lebih terang, -1 EV lebih gelap.
        let up = tone_map_srgb(&img, &ToneParams { exposure: 1.0, ..Default::default() });
        let down = tone_map_srgb(&img, &ToneParams { exposure: -1.0, ..Default::default() });
        assert!(up.get_pixel(0, 0)[0] > 128);
        assert!(down.get_pixel(0, 0)[0] < 128);
        // Saturasi 0 -> abu-abu.
        let colored = RgbaImage::from_pixel(2, 2, image::Rgba([200, 30, 30, 255]));
        let gray = tone_map_srgb(&colored, &ToneParams { saturation: 0.0, ..Default::default() });
        let p = gray.get_pixel(0, 0);
        assert_eq!(p[0], p[1]);
        assert_eq!(p[1], p[2]);
    }

    #[test]
    fn pdf_minimal_valid() {
        let img = RgbaImage::from_pixel(8, 8, image::Rgba([200, 30, 30, 255]));
        let jpeg = jpeg_encode(&img, 85).unwrap();
        let pages = vec![PdfPage {
            width: 595.0,
            height: 842.0,
            slots: vec![PdfSlot { x: 0.0, y: 0.0, w: 595.0, h: 842.0, jpeg, img_w: 8, img_h: 8 }],
        }];
        let pdf = build_pdf(&pages);
        assert!(pdf.starts_with(b"%PDF-1.4"));
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/Type /Page"));
        assert!(text.contains("/DCTDecode"));
        assert!(text.contains("%%EOF"));
    }
}

