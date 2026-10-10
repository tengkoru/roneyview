//! Thread pemuat: membaca byte dari folder/ZIP lalu mendekode gambar di luar
//! thread UI. Permintaan "utama" (gambar yang sedang dilihat) selalu didahulukan;
//! prefetch yang sudah basi dilewati.

use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use eframe::egui::{ColorImage, Context};
use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{
    AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, RgbaImage,
};

use crate::source::{Listing, Reader};

/// Prefetch hanya dilayani bila masih sedekat ini dari gambar aktif.
const PREFETCH_RADIUS: usize = 3;

// ------------------------------------------------------------ anggaran memori
//
// Foto kamera 50 MP = 200 MB sebagai RGBA; dengan mipmap dan beberapa halaman di
// cache, mudah melewati 1 GB dan membuat sistem kehabisan memori (atau GPU
// berbagi-memori macet). Semua batas di sini diturunkan dari memori yang
// tersedia saat Roneyview dimulai.

/// Di bawah ini, prefetch dimatikan dan cache dikosongkan (kecuali halaman aktif).
pub const LOW_MEMORY: u64 = 256 << 20;

fn parse_meminfo(text: &str) -> Option<u64> {
    text.lines()
        .find(|l| l.starts_with("MemAvailable:"))
        .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map(|kb| kb * 1024)
}

/// Memori tersedia SAAT INI (byte), dibaca langsung dari /proc/meminfo.
pub fn available_now() -> Option<u64> {
    parse_meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

/// Memori tersedia saat start (dicache); 2 GB bila tidak terbaca.
fn available_memory() -> u64 {
    static CELL: OnceLock<u64> = OnceLock::new();
    *CELL.get_or_init(|| available_now().unwrap_or(2 << 30))
}

fn budget_for(avail: u64) -> usize {
    (avail / 8).clamp(96 << 20, 256 << 20) as usize
}

/// Piksel maksimum satu halaman: cache harus muat >= 2 halaman termasuk mipmap (x4/3).
fn pixels_for(budget: usize) -> usize {
    (budget / 11).clamp(4_000_000, 16_000_000)
}

/// Anggaran total ukuran tekstur halaman di cache (byte).
pub fn cache_budget() -> usize {
    budget_for(available_memory())
}

pub fn max_pixels() -> usize {
    pixels_for(cache_budget())
}

/// Batas alokasi dekoder: gambar yang butuh lebih dari ini ditolak dengan pesan,
/// bukan dibiarkan menghabiskan RAM.
fn alloc_limit() -> u64 {
    let base = (available_memory() / 3).clamp(256 << 20, 768 << 20);
    match available_now() {
        // Sisakan 192 MB untuk sistem; jangan pernah mendorongnya ke OOM.
        Some(now) => base.min(now.saturating_sub(192 << 20)).max(32 << 20),
        None => base,
    }
}

/// Tolak SEBELUM dekode bila buffer hasil dekode melebihi batas memori. Pustaka
/// `image` tidak menegakkan `max_alloc` pada buffer akhir, jadi berkas kecil yang
/// mengaku berukuran raksasa bisa membuat alokasi gagal dan mematikan proses
/// (atau menyeret sistem ke OOM).
fn check_bytes(bytes: u64) -> Result<(), String> {
    if bytes > alloc_limit() {
        Err(format!(
            "Gambar terlalu besar untuk memori yang tersedia ({} MB saat didekode)",
            bytes >> 20
        ))
    } else {
        Ok(())
    }
}

fn check_canvas(w: u32, h: u32) -> Result<(), String> {
    if w == 0 || h == 0 {
        return Err("Gambar berukuran nol".into());
    }
    check_bytes(u64::from(w) * u64::from(h) * 4)
}

/// Pesan galat yang jujur: batas memori bukan "berkas rusak".
fn describe(e: &image::ImageError, fallback: &str) -> String {
    match e {
        image::ImageError::Limits(_) => "Gambar terlalu besar untuk memori yang tersedia".to_string(),
        _ => fallback.to_string(),
    }
}
/// Anggaran memori untuk semua frame satu animasi (RGBA). Lebih dari ini dipotong.
const ANIM_BUDGET: usize = 128 * 1024 * 1024;
const MAX_ANIM_FRAMES: usize = 1500;

pub enum Command {
    Open {
        session: u64,
        listing: Arc<Listing>,
    },
    Load {
        session: u64,
        index: usize,
        primary: bool,
        max_side: usize,
        /// Batas piksel untuk decode permintaan ini (tier resolusi mengikuti zoom:
        /// tampilan fit cukup ~2MP, zoom >= 7x naik ke ~4MP). Selalu <= max_pixels().
        max_pixels: usize,
        /// Halaman yang sedang dilihat. Dikirim di SETIAP permintaan: halaman yang sudah
        /// ada di cache tidak diminta ulang, jadi fokus tidak boleh bergantung pada
        /// permintaan utama saja (kalau tidak, prefetch/pasangan dianggap "terlalu jauh").
        focus: usize,
        /// Anggaran cache (byte) dan perkiraan isi cache saat permintaan dikirim.
        /// Worker memakai ini untuk MELEWATKAN prefetch yang hasilnya pasti dibuang
        /// karena tidak muat anggaran — decode satu gambar 16MP bisa makan 8 detik CPU.
        budget: usize,
        held: usize,
    },
    /// Batalkan prefetch yang masih antre untuk indeks ini (mis. karena indeksnya
    /// baru saja diminta sebagai gambar utama ke worker satunya). Tidak bisa
    /// membatalkan decode yang sudah berjalan; itu tidak apa-apa (hasilnya tetap benar).
    Forget { session: u64, index: usize },
}

pub struct AnimFrame {
    pub image: Arc<ColorImage>,
    pub delay: Duration,
}

/// Hasil dekode murni (tanpa ukuran berkas).
pub struct Pixels {
    /// Frame pertama (atau satu-satunya).
    pub image: Arc<ColorImage>,
    /// Dimensi asli setelah orientasi EXIF (sebelum diperkecil untuk tekstur).
    pub orig: [u32; 2],
    /// Terisi hanya untuk animasi (>= 2 frame); frames[0] berbagi data dengan `image`.
    pub frames: Option<Vec<AnimFrame>>,
    /// Animasi dipotong karena melewati anggaran memori / jumlah frame.
    pub truncated: bool,
    /// Nama format untuk jendela Properties ("JPEG", "GIF (animasi, 12 frame)", ...).
    pub format: String,
}

pub struct Decoded {
    pub pixels: Pixels,
    pub file_size: u64,
}

pub enum Outcome {
    Ready(Decoded),
    /// Pratinjau blur cepat untuk gambar besar; selalu disusul `Ready`
    /// untuk indeks yang sama (dari pemrosesan yang sama, tanpa jeda antre).
    Preview(Decoded),
    Failed(String),
    Skipped,
}

pub struct Loaded {
    pub session: u64,
    pub index: usize,
    pub outcome: Outcome,
}

struct Job {
    index: usize,
    primary: bool,
    max_side: usize,
    max_pixels: usize,
    budget: usize,
    held: usize,
    seq: u64,
}

struct Worker {
    ctx: Context,
    tx: Sender<Loaded>,
    session: u64,
    listing: Option<Arc<Listing>>,
    reader: Option<Result<Reader, String>>,
    queue: Vec<Job>,
    seq: u64,
    focus: usize,
    /// true = worker utama (hanya memproses permintaan utama),
    /// false = worker prefetch (hanya memproses prefetch).
    handles_primary: bool,
}

/// Mulai thread pemuat. Mengembalikan `(utama, prefetch, hasil)`:
/// - `utama`: untuk permintaan utama (gambar yang sedang dilihat),
/// - `prefetch`: untuk prefetch (gambar di sekitarnya),
/// - `hasil`: hasil decode dari kedua worker.
///
/// Dua worker dipakai supaya satu gambar raksasa (decode bisa 8 detik) tidak
/// memblokir prefetch gambar-gambar lain di belakangnya. `Open` harus dikirim
/// ke KEDUA worker; `Load` dirutekan sesuai jenisnya.
pub fn spawn(
    ctx: Context,
) -> std::io::Result<(Sender<Command>, Sender<Command>, Receiver<Loaded>)> {
    let (main_tx, main_rx) = mpsc::channel::<Command>();
    let (pref_tx, pref_rx) = mpsc::channel::<Command>();
    let (out_tx, out_rx) = mpsc::channel::<Loaded>();
    // Worker dibangun di dalam thread-nya sendiri: pembaca arsip (mis. RAR) memegang
    // handle C yang tidak perlu/boleh berpindah thread.
    for (name, rx, handles_primary) in [
        ("roneyview-loader", main_rx, true),
        ("roneyview-loader-prefetch", pref_rx, false),
    ] {
        let ctx = ctx.clone();
        let tx = out_tx.clone();
        thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                Worker {
                    ctx,
                    tx,
                    session: 0,
                    listing: None,
                    reader: None,
                    queue: Vec::new(),
                    seq: 0,
                    focus: 0,
                    handles_primary,
                }
                .run(rx)
            })?;
    }
    Ok((main_tx, pref_tx, out_rx))
}

impl Worker {
    fn run(mut self, rx: Receiver<Command>) {
        loop {
            if self.queue.is_empty() {
                match rx.recv() {
                    Ok(c) => self.apply(c),
                    Err(_) => return, // UI sudah tutup
                }
            }
            while let Ok(c) = rx.try_recv() {
                self.apply(c);
            }
            let Some(job) = self.next_job() else {
                continue;
            };
            let outcome = self.process(&job);
            // Permintaan ganda untuk indeks yang sama tidak perlu didekode lagi.
            self.queue.retain(|j| j.index != job.index);
            let msg = Loaded {
                session: self.session,
                index: job.index,
                outcome,
            };
            if self.tx.send(msg).is_err() {
                return;
            }
            self.ctx.request_repaint();
        }
    }

    fn apply(&mut self, cmd: Command) {
        match cmd {
            Command::Open { session, listing } => {
                self.session = session;
                self.listing = Some(listing);
                self.reader = None;
                self.queue.clear();
                self.focus = 0;
            }
            Command::Load {
                session,
                index,
                primary,
                max_side,
                max_pixels,
                focus,
                budget,
                held,
            } => {
                // Tiap worker hanya menangani jenisnya sendiri (utama vs prefetch).
                if primary != self.handles_primary {
                    return;
                }
                if session != self.session {
                    return;
                }
                self.seq += 1;
                self.focus = focus;
                if primary {
                    for j in &mut self.queue {
                        j.primary = false; // hanya satu permintaan utama
                    }
                }
                if let Some(j) = self.queue.iter_mut().find(|j| j.index == index) {
                    j.max_side = max_side;
                    j.max_pixels = max_pixels;
                    j.budget = budget;
                    j.held = held;
                    if primary {
                        j.primary = true;
                        j.seq = self.seq;
                    }
                } else {
                    self.queue.push(Job {
                        index,
                        primary,
                        max_side,
                        max_pixels,
                        budget,
                        held,
                        seq: self.seq,
                    });
                }
            }
            Command::Forget { session, index } => {
                if session != self.session {
                    return;
                }
                // Hapus dari antrean bila belum mulai didekode. Tidak ada pesan
                // balasan: peminta sudah mencatatnya sebagai permintaan utama.
                self.queue.retain(|j| j.index != index);
            }
        }
    }

    fn next_job(&mut self) -> Option<Job> {
        let pos = self
            .queue
            .iter()
            .enumerate()
            .filter(|(_, j)| j.primary)
            .max_by_key(|(_, j)| j.seq)
            .map(|(p, _)| p)
            .or_else(|| {
                self.queue
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, j)| j.seq)
                    .map(|(p, _)| p)
            })?;
        Some(self.queue.swap_remove(pos))
    }

    fn process(&mut self, job: &Job) -> Outcome {
        if !job.primary && job.index.abs_diff(self.focus) > PREFETCH_RADIUS {
            return Outcome::Skipped;
        }
        let Some(listing) = self.listing.clone() else {
            return Outcome::Failed("Tidak ada sumber yang terbuka".into());
        };
        if self.reader.is_none() {
            self.reader = Some(listing.open_reader());
        }
        let reader = match self.reader.as_mut() {
            Some(Ok(r)) => r,
            Some(Err(e)) => return Outcome::Failed(e.clone()),
            None => return Outcome::Failed("Pembaca sumber tidak tersedia".into()),
        };
        let bytes = match reader.read(job.index) {
            Ok(b) => b,
            Err(e) => return Outcome::Failed(e),
        };
        let file_size = bytes.len() as u64;
        // Prefetch yang hasilnya pasti dibuang karena tidak muat anggaran tidak
        // perlu didekode: satu gambar 16MP bisa makan 8 detik CPU. Perkiraan
        // dihitung dari header saja (tanpa decode penuh); orientasi EXIF tidak
        // mengubah luas area sehingga tidak memengaruhi perkiraan.
        // (Permintaan utama selalu didekode: yang dilihat pengguna tidak boleh gagal.)
        if !job.primary {
            if let Some(est) = estimate_bytes(&bytes, job.max_side, job.max_pixels) {
                if est > job.budget || job.held.saturating_add(est) > job.budget {
                    return Outcome::Skipped;
                }
            }
        }
        match catch_unwind(AssertUnwindSafe(|| {
            decode_progressive(&bytes, job.max_side, job.max_pixels)
        })) {
            Ok(Ok((preview, sharp))) => {
                // Pratinjau dikirim langsung agar segera tampil; versi tajam
                // menyusul lewat hasil akhir pemrosesan ini.
                if let Some(p) = preview {
                    let _ = self.tx.send(Loaded {
                        session: self.session,
                        index: job.index,
                        outcome: Outcome::Preview(Decoded { pixels: p, file_size }),
                    });
                    self.ctx.request_repaint();
                }
                Outcome::Ready(Decoded {
                    pixels: sharp,
                    file_size,
                })
            }
            Ok(Err(e)) => Outcome::Failed(e),
            Err(_) => Outcome::Failed(
                "Dekoder gagal memproses berkas ini (data kemungkinan rusak)".into(),
            ),
        }
    }
}

/// Gambar dengan sisi panjang di atas ini mendapat pratinjau progresif
/// (blur cepat dulu, tajam menyusul). Gambar kecil decode-nya sudah cepat,
/// tidak perlu.
const PREVIEW_LONG_SIDE: u32 = 2500;
/// Ukuran pratinjau ~0.25MP: cukup untuk bayangan yang dikenali.
const PREVIEW_PIXELS: usize = 250_000;

/// Dekode progresif: satu kali decode penuh, lalu —
/// - gambar besar (>2500px): pratinjau blur cepat + versi tajam, atau
/// - gambar kecil: langsung versi tajam (tanpa pratinjau).
/// Mengembalikan `(pratinjau, tajam)`.
pub fn decode_progressive(
    bytes: &[u8],
    max_side: usize,
    max_pixels: usize,
) -> Result<(Option<Pixels>, Pixels), String> {
    let (preview, mut sharp) = decode_progressive_inner(bytes, max_side, max_pixels)?;
    let name = format_name(bytes);
    let format = match &sharp.frames {
        Some(f) => format!("{name} (animasi, {} frame)", f.len()),
        None => name,
    };
    sharp.format = format.clone();
    let preview = preview.map(|mut p| {
        p.format = format;
        p
    });
    Ok((preview, sharp))
}

fn decode_progressive_inner(
    bytes: &[u8],
    max_side: usize,
    max_pixels: usize,
) -> Result<(Option<Pixels>, Pixels), String> {
    let max_side_c = max_side.clamp(1024, 16384) as u32;

    #[cfg(feature = "heif")]
    if is_heif(bytes) {
        return decode_heif(bytes, max_side_c, max_pixels).map(|p| (None, p));
    }
    #[cfg(not(feature = "heif"))]
    if looks_like_heif(bytes) {
        return Err("HEIC/AVIF tidak ikut dikompilasi (bangun dengan fitur \"heif\")".into());
    }

    if let Some(p) = decode_animated(bytes, max_side_c, max_pixels)? {
        return Ok((None, p));
    }
    // Jalur statis (JPEG/PNG/...): satu decode penuh, dua output.
    let raw = load_raw(bytes)?;
    let big = raw.img.width().max(raw.img.height()) > PREVIEW_LONG_SIDE;
    // Pratinjau bersifat best-effort: gagal bukan alasan menggagalkan decode.
    let preview = if big { preview_pixels(&raw).ok() } else { None };
    let sharp = finish_raw(raw, max_side_c, max_pixels)?;
    Ok((preview, sharp))
}

/// Pratinjau blur untuk gambar besar: Nearest langsung ke ~0.25MP.
/// (Triangle ke ukuran sekecil ini hampir semahal ke ukuran penuh —
/// biayanya mengikuti rasio, bukan ukuran output — jadi tidak hemat.)
fn preview_pixels(raw: &RawImage) -> Result<Pixels, String> {
    let (w, h) = (raw.img.width(), raw.img.height());
    if w == 0 || h == 0 {
        return Err("Gambar berukuran nol".into());
    }
    let (ow, oh) = oriented_dims(w, h, raw.orientation);
    let s = (PREVIEW_PIXELS as f64 / (f64::from(ow) * f64::from(oh))).sqrt().min(1.0);
    let (tw, th) = (
        (f64::from(ow) * s).round() as u32,
        (f64::from(oh) * s).round() as u32,
    );
    let (tw, th) = (tw.max(1), th.max(1));
    // Target dihitung di ruang terorientasi, dipetakan kembali seperti finish_raw.
    let (uw, uh) = if swaps_axes(raw.orientation) {
        (th, tw)
    } else {
        (tw, th)
    };
    let mut img = raw.img.resize_exact(uw, uh, FilterType::Nearest);
    img.apply_orientation(raw.orientation);
    let image = Arc::new(to_color_image(img));
    Ok(Pixels {
        image,
        orig: [ow, oh],
        frames: None,
        truncated: false,
        format: String::new(),
    })
}

/// Nama format dari isi berkas (bukan ekstensi).
pub fn format_name(bytes: &[u8]) -> String {
    if looks_like_heif(bytes) {
        return if matches!(&bytes[8..12], b"avif" | b"avis") {
            "AVIF".to_string()
        } else {
            "HEIC/HEIF".to_string()
        };
    }
    match image::guess_format(bytes) {
        Ok(ImageFormat::Jpeg) => "JPEG",
        Ok(ImageFormat::Png) => "PNG",
        Ok(ImageFormat::Gif) => "GIF",
        Ok(ImageFormat::WebP) => "WebP",
        Ok(ImageFormat::Bmp) => "BMP",
        Ok(ImageFormat::Tiff) => "TIFF",
        Ok(ImageFormat::Qoi) => "QOI",
        Ok(ImageFormat::Tga) => "TGA",
        Ok(ImageFormat::Ico) => "ICO",
        _ => "Tidak diketahui",
    }
    .to_string()
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_alloc = Some(alloc_limit());
    limits
}

/// Ukuran tekstur akhir: dibatasi sisi terpanjang (batas GPU) DAN jumlah piksel
/// (batas memori). Rasio aspek dipertahankan.
fn fit_target(w: u32, h: u32, max_side: u32, max_pixels: usize) -> (u32, u32) {
    let (wf, hf) = (f64::from(w), f64::from(h));
    let by_side = f64::from(max_side) / wf.max(hf);
    let by_area = (max_pixels as f64 / (wf * hf)).sqrt();
    let scale = by_side.min(by_area).min(1.0);
    if scale >= 1.0 {
        return (w, h);
    }
    (
        ((wf * scale).round() as u32).max(1),
        ((hf * scale).round() as u32).max(1),
    )
}

/// Perkiraan ukuran halaman hasil decode (byte, termasuk mipmap x4/3) hanya dari
/// header gambar — tanpa decode penuh. `None` bila format tak dikenali dari
/// header (mis. HEIF yang ditangani jalur khusus); pemanggil melanjutkan decode
/// normal dalam kasus itu.
fn estimate_bytes(bytes: &[u8], max_side: usize, max_pixels: usize) -> Option<usize> {
    let max_side = max_side.clamp(1024, 16384) as u32;
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits());
    let decoder = reader.into_decoder().ok()?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let (tw, th) = fit_target(w, h, max_side, max_pixels);
    Some(tw as usize * th as usize * 4 * 4 / 3)
}

/// Gambar hasil dekode mentah: piksel BELUM diorientasi menurut EXIF.
struct RawImage {
    img: DynamicImage,
    orientation: Orientation,
}

/// Apakah orientasi ini menukar sumbu (lebar <-> tinggi).
fn swaps_axes(o: Orientation) -> bool {
    matches!(
        o,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    )
}

/// Dimensi setelah orientasi EXIF diterapkan.
fn oriented_dims(w: u32, h: u32, o: Orientation) -> (u32, u32) {
    if swaps_axes(o) {
        (h, w)
    } else {
        (w, h)
    }
}

/// Dekode penuh (dengan batas memori). Orientasi EXIF TIDAK diterapkan di sini:
/// pemanggil memperkecil dulu lalu mengorientasikan gambar kecilnya, supaya foto
/// berorientasi tidak membayar satu salinan penuh yang sia-sia.
fn load_raw(bytes: &[u8]) -> Result<RawImage, String> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("Gagal membaca berkas: {e}"))?;
    reader.limits(limits());
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| describe(&e, "Format tidak dikenali atau berkas rusak"))?;
    let (dw, dh) = decoder.dimensions();
    check_canvas(dw, dh)?;
    check_bytes(decoder.total_bytes())?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let img = DynamicImage::from_decoder(decoder)
        .map_err(|e| describe(&e, "Gambar rusak atau tidak lengkap"))?;
    Ok(RawImage { img, orientation })
}

fn finish_raw(raw: RawImage, max_side: u32, max_pixels: usize) -> Result<Pixels, String> {
    let (w, h) = (raw.img.width(), raw.img.height());
    if w == 0 || h == 0 {
        return Err("Gambar berukuran nol".into());
    }
    let (ow, oh) = oriented_dims(w, h, raw.orientation);
    // Target dihitung dalam ruang terorientasi, lalu dipetakan kembali ke ruang
    // sebelum orientasi supaya resize dikerjakan pada gambar yang belum diputar:
    // foto portrait 6000x4000 tidak lagi disalin 24 MP dua kali.
    let (tw, th) = fit_target(ow, oh, max_side, max_pixels);
    let (uw, uh) = if swaps_axes(raw.orientation) {
        (th, tw)
    } else {
        (tw, th)
    };
    let mut img = shrink_to(raw.img, uw, uh);
    img.apply_orientation(raw.orientation);
    let image = Arc::new(to_color_image(img));
    Ok(Pixels {
        image,
        orig: [ow, oh],
        frames: None,
        truncated: false,
        format: String::new(),
    })
}

/// Perkecil ke ukuran pasti dalam format aslinya (mis. RGB8), bukan RGBA:
/// menghindari salinan RGBA berukuran penuh yang bisa mencapai ratusan MB.
///
/// Memakai `fast_image_resize` (SIMD, ~20x lebih cepat daripada
/// `image::imageops` untuk foto besar); fallback ke resize bawaan `image`
/// bila tipe piksel tidak didukung.
fn shrink_to(img: DynamicImage, w: u32, h: u32) -> DynamicImage {
    let (iw, ih) = (img.width(), img.height());
    if (w, h) == (iw, ih) {
        return img;
    }
    match resize_backend() {
        ResizeBackend::FastImageResize => {
            fast_shrink(img, w, h).unwrap_or_else(|img| img.resize_exact(w, h, FilterType::Triangle))
        }
        // Di mesin yang SIMD-nya kena penalti (mis. VM tertentu), `image`
        // justru berkali-kali lebih cepat daripada `fast_image_resize`.
        ResizeBackend::ImageCrate => img.resize_exact(w, h, FilterType::Triangle),
    }
}

/// Backend resize terpilih lewat kalibrasi sekali jalan.
///
/// `fast_image_resize` biasanya jauh lebih cepat berkat SIMD, tetapi di
/// sebagian mesin virtual instruksi AVX2-nya sangat lambat (terukur 3x lebih
/// lambat daripada kode skalar) sehingga ia malah jadi 4-8x lebih lambat
/// daripada `image`. Kalibrasi ini memilih yang tercepat di mesin ini.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ResizeBackend {
    FastImageResize,
    ImageCrate,
}

static RESIZE_BACKEND: OnceLock<ResizeBackend> = OnceLock::new();

fn resize_backend() -> ResizeBackend {
    *RESIZE_BACKEND.get_or_init(calibrate_resize_backend)
}

fn calibrate_resize_backend() -> ResizeBackend {
    use std::time::Instant;
    // Gambar sintetis 1280x960 RGBA; cukup besar untuk mengukur, cukup kecil
    // supaya kalibrasi sekali jalan tidak terasa (ratusan ms sekali saja).
    let (w, h) = (1280u32, 960u32);
    let mut px = vec![0u8; (w as usize) * (h as usize) * 4];
    for (i, b) in px.iter_mut().enumerate() {
        *b = ((i.wrapping_mul(2654435761)) >> 16) as u8;
    }
    let img = match image::RgbaImage::from_raw(w, h, px) {
        Some(buf) => DynamicImage::ImageRgba8(buf),
        None => return ResizeBackend::ImageCrate,
    };
    let (tw, th) = (w / 2, h / 2);
    let t0 = Instant::now();
    let fir_ok = fast_shrink(img.clone(), tw, th).is_ok();
    let t_fir = t0.elapsed();
    let t0 = Instant::now();
    let _ = img.resize_exact(tw, th, FilterType::Triangle);
    let t_img = t0.elapsed();
    // Pilih `fast_image_resize` hanya bila ia JELAS lebih cepat (margin 25%):
    // ragu-ragu = pakai `image` yang perilakunya konsisten di semua mesin.
    if fir_ok && t_fir.as_secs_f64() * 1.25 < t_img.as_secs_f64() {
        ResizeBackend::FastImageResize
    } else {
        ResizeBackend::ImageCrate
    }
}

/// Resize via `fast_image_resize`. `Err` mengembalikan gambar utuh untuk fallback.
fn fast_shrink(img: DynamicImage, w: u32, h: u32) -> Result<DynamicImage, DynamicImage> {
    use fast_image_resize::{
        FilterType as FirFilterType, ResizeAlg, ResizeOptions, Resizer,
    };

    let mut dst = match blank_like(&img, w, h) {
        Some(d) => d,
        None => return Err(img),
    };
    // Bilinear: kualitas setara Triangle untuk downscale tampilan,
    // jauh lebih cepat. `mul_div_alpha` bawaan menangani alfa dengan benar.
    let opts =
        ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FirFilterType::Bilinear));
    match Resizer::new().resize(&img, &mut dst, &opts) {
        Ok(()) => Ok(dst),
        Err(_) => Err(img),
    }
}

/// Gambar kosong dengan tipe piksel yang sama dengan sumber (syarat
/// `fast_image_resize`: tipe src dan dst harus sama).
fn blank_like(img: &DynamicImage, w: u32, h: u32) -> Option<DynamicImage> {
    Some(match img {
        DynamicImage::ImageLuma8(_) => DynamicImage::new_luma8(w, h),
        DynamicImage::ImageLumaA8(_) => DynamicImage::new_luma_a8(w, h),
        DynamicImage::ImageRgb8(_) => DynamicImage::new_rgb8(w, h),
        DynamicImage::ImageRgba8(_) => DynamicImage::new_rgba8(w, h),
        DynamicImage::ImageLuma16(_) => DynamicImage::new_luma16(w, h),
        DynamicImage::ImageLumaA16(_) => DynamicImage::new_luma_a16(w, h),
        DynamicImage::ImageRgb16(_) => DynamicImage::new_rgb16(w, h),
        DynamicImage::ImageRgba16(_) => DynamicImage::new_rgba16(w, h),
        DynamicImage::ImageRgb32F(_) => DynamicImage::new_rgb32f(w, h),
        DynamicImage::ImageRgba32F(_) => DynamicImage::new_rgba32f(w, h),
        _ => return None,
    })
}

/// Diperkecil dalam format aslinya (mis. RGB8), bukan RGBA: menghindari salinan
/// RGBA berukuran penuh yang bisa mencapai ratusan MB.
fn shrink(img: DynamicImage, max_side: u32, max_pixels: usize) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    let (nw, nh) = fit_target(w, h, max_side, max_pixels);
    shrink_to(img, nw, nh)
}

fn to_color_image(img: DynamicImage) -> ColorImage {
    let rgba = img.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
}

/// Animasi GIF / WebP / APNG. `Ok(None)` = bukan animasi (atau hanya 1 frame),
/// biarkan jalur gambar statis yang menangani.
fn decode_animated(bytes: &[u8], max_side: u32, max_pixels: usize) -> Result<Option<Pixels>, String> {
    let frames = match image::guess_format(bytes).ok() {
        Some(ImageFormat::Gif) => {
            let mut d = GifDecoder::new(Cursor::new(bytes))
                .map_err(|_| "GIF rusak atau tidak lengkap".to_string())?;
            let (w, h) = d.dimensions();
            check_canvas(w, h)?;
            let _ = d.set_limits(limits());
            d.into_frames()
        }
        Some(ImageFormat::WebP) => {
            let mut d = WebPDecoder::new(Cursor::new(bytes))
                .map_err(|_| "WebP rusak atau tidak lengkap".to_string())?;
            if !d.has_animation() {
                return Ok(None);
            }
            let (w, h) = d.dimensions();
            check_canvas(w, h)?;
            let _ = d.set_limits(limits());
            d.into_frames()
        }
        Some(ImageFormat::Png) => {
            let mut d = PngDecoder::new(Cursor::new(bytes))
                .map_err(|_| "PNG rusak atau tidak lengkap".to_string())?;
            if !d.is_apng().unwrap_or(false) {
                return Ok(None);
            }
            let (w, h) = d.dimensions();
            check_canvas(w, h)?;
            let _ = d.set_limits(limits());
            d.apng()
                .map_err(|_| "APNG rusak atau tidak lengkap".to_string())?
                .into_frames()
        }
        _ => return Ok(None),
    };

    let mut out: Vec<AnimFrame> = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;
    let mut canvas = [0u32, 0u32];
    for item in frames {
        let frame = match item {
            Ok(f) => f,
            Err(e) => {
                if out.is_empty() {
                    return Err(format!("Animasi rusak: {e}"));
                }
                truncated = true; // pakai frame yang sudah terbaca
                break;
            }
        };
        let (n, d) = frame.delay().numer_denom_ms();
        let ms = if d == 0 { 100 } else { n / d };
        // Perilaku peramban: jeda sangat kecil dianggap 100 ms.
        let ms = if ms < 20 { 100 } else { ms.min(10_000) };

        let buf: RgbaImage = frame.into_buffer();
        if out.is_empty() {
            canvas = [buf.width(), buf.height()];
        }
        let img = shrink(DynamicImage::ImageRgba8(buf), max_side, max_pixels);
        let cost = img.width() as usize * img.height() as usize * 4;
        if !out.is_empty() && (total + cost > ANIM_BUDGET || out.len() >= MAX_ANIM_FRAMES) {
            truncated = true;
            break;
        }
        total += cost;
        out.push(AnimFrame {
            image: Arc::new(to_color_image(img)),
            delay: Duration::from_millis(u64::from(ms)),
        });
    }

    if out.len() < 2 || canvas[0] == 0 || canvas[1] == 0 {
        return Ok(None);
    }
    Ok(Some(Pixels {
        image: out[0].image.clone(),
        orig: canvas,
        frames: Some(out),
        truncated,
        format: String::new(),
    }))
}

// ------------------------------------------------------------------- HEIC / AVIF

/// Brand ISO-BMFF milik HEIF/AVIF (byte 4..8 = "ftyp", 8..12 = major brand).
fn looks_like_heif(bytes: &[u8]) -> bool {
    bytes.len() >= 12
        && &bytes[4..8] == b"ftyp"
        && matches!(
            &bytes[8..12],
            b"heic"
                | b"heix"
                | b"hevc"
                | b"hevx"
                | b"heim"
                | b"heis"
                | b"hevm"
                | b"hevs"
                | b"mif1"
                | b"mif2"
                | b"msf1"
                | b"avif"
                | b"avis"
        )
}

#[cfg(feature = "heif")]
fn is_heif(bytes: &[u8]) -> bool {
    looks_like_heif(bytes)
}

#[cfg(feature = "heif")]
fn decode_heif(bytes: &[u8], max_side: u32, max_pixels: usize) -> Result<Pixels, String> {
    finish_raw(
        RawImage {
            img: DynamicImage::ImageRgba8(load_heif_rgba(bytes)?),
            // `decode` libheif sudah menerapkan rotasi/mirror/crop dari berkas.
            orientation: Orientation::NoTransforms,
        },
        max_side,
        max_pixels,
    )
}

#[cfg(feature = "heif")]
fn load_heif_rgba(bytes: &[u8]) -> Result<RgbaImage, String> {
    use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};

    thread_local! {
        // Inisialisasi libheif sekali per thread pemuat.
        static HEIF: LibHeif = LibHeif::new();
    }
    let fail = |e: libheif_rs::HeifError| {
        format!(
            "HEIC/AVIF tidak dapat didekode ({e}). Pastikan libheif beserta dekoder \
             HEVC/AV1 terpasang (paket libheif-plugin-libde265 dan libheif-plugin-dav1d)."
        )
    };
    let ctx = HeifContext::read_from_bytes(bytes).map_err(fail)?;
    let handle = ctx.primary_image_handle().map_err(fail)?;
    // Tolak sebelum dekode bila hasil RGBA-nya melebihi batas memori.
    check_canvas(handle.width(), handle.height())?;
    // `decode` sudah menerapkan rotasi/mirror/crop yang tersimpan di berkas.
    let image = HEIF
        .with(|lib| lib.decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), None))
        .map_err(fail)?;
    let planes = image.planes();
    let plane = planes
        .interleaved
        .ok_or_else(|| "Data piksel HEIC/AVIF tidak tersedia".to_string())?;
    let (w, h) = (plane.width as usize, plane.height as usize);
    if w == 0 || h == 0 {
        return Err("Gambar berukuran nol".into());
    }
    let row_len = w * 4;
    let mut buf = Vec::with_capacity(row_len * h);
    for y in 0..h {
        let start = y * plane.stride;
        let row = plane
            .data
            .get(start..start + row_len)
            .ok_or_else(|| "Data piksel HEIC/AVIF terpotong".to_string())?;
        buf.extend_from_slice(row);
    }
    RgbaImage::from_raw(w as u32, h as u32, buf)
        .ok_or_else(|| "Ukuran data piksel tidak cocok".to_string())
}

/// Dekode ke RGBA untuk wallpaper: dibatasi `max_side` dan `max_pixels`.
/// GIF/WebP/APNG beranimasi diambil frame pertamanya.
pub fn decode_rgba(bytes: &[u8], max_side: u32, max_pixels: usize) -> Result<RgbaImage, String> {
    #[cfg(feature = "heif")]
    let raw = if is_heif(bytes) {
        RawImage {
            // `decode` libheif sudah menerapkan rotasi/mirror/crop dari berkas.
            img: DynamicImage::ImageRgba8(load_heif_rgba(bytes)?),
            orientation: Orientation::NoTransforms,
        }
    } else {
        load_raw(bytes)?
    };
    #[cfg(not(feature = "heif"))]
    let raw = {
        if looks_like_heif(bytes) {
            return Err("HEIC/AVIF tidak ikut dikompilasi (bangun dengan fitur \"heif\")".into());
        }
        load_raw(bytes)?
    };
    let (w, h) = (raw.img.width(), raw.img.height());
    if w == 0 || h == 0 {
        return Err("Gambar berukuran nol".into());
    }
    // Sama seperti finish_raw: perkecil dulu dalam ruang sebelum orientasi,
    // lalu orientasikan gambar kecilnya.
    let (ow, oh) = oriented_dims(w, h, raw.orientation);
    let (tw, th) = fit_target(ow, oh, max_side, max_pixels);
    let (uw, uh) = if swaps_axes(raw.orientation) {
        (th, tw)
    } else {
        (tw, th)
    };
    let mut img = shrink_to(raw.img, uw, uh);
    img.apply_orientation(raw.orientation);
    Ok(img.into_rgba8())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper tes: ambil versi tajam dari decode_progressive.
    fn decode_sharp(bytes: &[u8], max_side: usize, max_pixels: usize) -> Result<Pixels, String> {
        decode_progressive(bytes, max_side, max_pixels).map(|(_, sharp)| sharp)
    }
    use image::{ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = RgbaImage::from_pixel(w, h, Rgba([10, 200, 30, 255]));
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn jpeg_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = RgbImage::from_pixel(w, h, Rgb([200, 30, 40]));
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(img)
            .write_to(&mut out, ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn target_dibatasi_sisi_dan_jumlah_piksel() {
        // di bawah semua batas: tidak berubah
        assert_eq!(fit_target(4000, 3000, 16384, 16_000_000), (4000, 3000));
        // 54 MP -> dipangkas ke <= 16 MP dengan rasio aspek tetap
        let (w, h) = fit_target(9000, 6000, 16384, 16_000_000);
        // toleransi 0,1% untuk pembulatan ke piksel utuh
        assert!(u64::from(w) * u64::from(h) <= 16_016_000, "{w}x{h}");
        assert!((f64::from(w) / f64::from(h) - 1.5).abs() < 0.01);
        // batas sisi tetap berlaku untuk strip tinggi
        let (w, h) = fit_target(1000, 40000, 8192, 16_000_000);
        assert_eq!(h, 8192);
        assert_eq!(w, 205);
        // tidak pernah nol
        assert_eq!(fit_target(1, 100000, 1024, 4_000_000).0, 1);
    }

    #[test]
    fn anggaran_mengikuti_memori_tersedia_dengan_batas_aman() {
        assert_eq!(budget_for(512 << 20), 96 << 20); // RAM sempit: batas bawah
        assert_eq!(budget_for(1 << 30), 128 << 20);
        assert_eq!(budget_for(16 << 30), 256 << 20); // RAM longgar: batas atas
        assert_eq!(pixels_for(96 << 20), 9_151_208);
        assert_eq!(pixels_for(256 << 20), 16_000_000);
        // dua halaman penuh (+mipmap) selalu muat dalam anggaran
        for avail in [256u64 << 20, 1 << 30, 4 << 30, 32 << 30] {
            let b = budget_for(avail);
            let page = pixels_for(b) * 4 * 4 / 3;
            assert!(page * 2 <= b + (b / 10), "avail={avail}");
        }
    }

    #[test]
    fn meminfo_dibaca_dengan_benar() {
        let sample = "MemTotal:       16384000 kB\nMemFree:  100 kB\nMemAvailable:    2048000 kB\nBuffers: 1 kB\n";
        assert_eq!(parse_meminfo(sample), Some(2_048_000 * 1024));
        assert_eq!(parse_meminfo("MemTotal: 1 kB\n"), None);
        assert_eq!(parse_meminfo(""), None);
        assert_eq!(parse_meminfo("MemAvailable: abc kB"), None);
    }

    #[test]
    fn galat_batas_memori_dijelaskan_bukan_dianggap_rusak() {
        use image::error::{LimitError, LimitErrorKind};
        let e = image::ImageError::Limits(LimitError::from_kind(LimitErrorKind::InsufficientMemory));
        assert!(describe(&e, "x").contains("terlalu besar"));
        let io = image::ImageError::IoError(std::io::Error::other("rusak"));
        assert_eq!(describe(&io, "fallback"), "fallback");
    }

    #[test]
    fn png_yang_mengaku_40000x40000_ditolak_tanpa_mengalokasikan_memori() {
        // Berkas ±100 byte yang mengaku 1,6 gigapiksel (6,4 GB RGBA).
        let data: &[u8] = include_bytes!("../tests/fixtures/enorme.png");
        assert!(data.len() < 200);
        let err = match decode_sharp(data, 4096, max_pixels()) {
            Ok(_) => panic!("seharusnya ditolak"),
            Err(e) => e,
        };
        assert!(!err.is_empty());
    }

    #[test]
    fn gif_yang_mengaku_65535x65535_ditolak_tanpa_mengalokasikan_memori() {
        // Header GIF minimal: layar logis 65535x65535 (17 GB RGBA), tanpa frame.
        let mut g = b"GIF89a".to_vec();
        g.extend_from_slice(&65535u16.to_le_bytes());
        g.extend_from_slice(&65535u16.to_le_bytes());
        g.extend_from_slice(&[0, 0, 0, b';']);
        assert!(decode_sharp(&g, 4096, max_pixels()).is_err());
    }

    #[test]
    fn dekode_png_biasa() {
        let p = decode_sharp(&png_bytes(40, 30), 4096, max_pixels()).unwrap();
        assert_eq!(p.orig, [40, 30]);
        assert_eq!(p.image.size, [40, 30]);
        assert!(p.frames.is_none());
    }

    #[test]
    fn gambar_besar_diperkecil_tetapi_dimensi_asli_dicatat() {
        let p = decode_sharp(&png_bytes(3000, 1500), 1024, max_pixels()).unwrap();
        assert_eq!(p.orig, [3000, 1500]);
        assert_eq!(p.image.size, [1024, 512]);
    }

    #[test]
    fn batas_piksel_permintaan_mengikuti_tier_zoom() {
        // Tier fit (~2MP): gambar 4.5MP dipotong ke <= 2MP.
        let p = decode_sharp(&png_bytes(3000, 1500), 16384, 2_000_000).unwrap();
        let px = p.image.size[0] * p.image.size[1];
        assert!(px <= 2_000_000, "dapat {px}");
        assert_eq!(p.orig, [3000, 1500]);
        // Tier zoom >= 7x (~4MP): gambar yang sama boleh sampai 4MP.
        let p = decode_sharp(&png_bytes(3000, 1500), 16384, 4_000_000).unwrap();
        let px = p.image.size[0] * p.image.size[1];
        assert!(px <= 4_000_000 && px > 2_000_000, "dapat {px}");
        // Gambar kecil tidak diperbesar oleh tier.
        let p = decode_sharp(&png_bytes(40, 30), 16384, 2_000_000).unwrap();
        assert_eq!(p.image.size, [40, 30]);
    }

    #[test]
    fn progresif_gambar_besar_dapat_pratinjau_dan_tajam() {
        // >2500px: pratinjau kecil + tajam 2MP dari satu decode.
        let (preview, sharp) = decode_progressive(&png_bytes(3000, 1500), 16384, 2_000_000).unwrap();
        let p = preview.expect("gambar besar harus punya pratinjau");
        let ppx = p.image.size[0] * p.image.size[1];
        assert!(ppx <= 260_000, "pratinjau {ppx}"); // toleransi pembulatan
        assert_eq!(p.orig, [3000, 1500]);
        let spx = sharp.image.size[0] * sharp.image.size[1];
        assert!(spx <= 2_000_000 && spx > ppx, "tajam {spx}");
        assert_eq!(sharp.orig, [3000, 1500]);
    }

    #[test]
    fn progresif_gambar_kecil_tanpa_pratinjau() {
        // <=2500px: langsung tajam, tanpa pratinjau.
        let (preview, sharp) = decode_progressive(&png_bytes(40, 30), 16384, 2_000_000).unwrap();
        assert!(preview.is_none());
        assert_eq!(sharp.image.size, [40, 30]);
    }

    #[test]
    fn pekerja_mengirim_pratinjau_lalu_tajam_untuk_gambar_besar() {
        use crate::source::Listing;
        use std::time::Duration;
        let dir = std::env::temp_dir().join("rv-prog-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.png"), png_bytes(3000, 1600)).unwrap();
        let (listing, _) = Listing::open(&dir, true).unwrap();
        let ctx = eframe::egui::Context::default();
        let (main_tx, pref_tx, out_rx) = spawn(ctx).unwrap();
        let listing = std::sync::Arc::new(listing);
        main_tx
            .send(Command::Open { session: 1, listing: listing.clone() })
            .unwrap();
        pref_tx.send(Command::Open { session: 1, listing }).unwrap();
        main_tx
            .send(Command::Load {
                session: 1,
                index: 0,
                primary: true,
                max_side: 16384,
                max_pixels: 2_000_000,
                focus: 0,
                budget: 256 << 20,
                held: 0,
            })
            .unwrap();
        // Pratinjau harus tiba dulu, disusul versi tajam.
        let m1 = out_rx.recv_timeout(Duration::from_secs(60)).unwrap();
        assert!(matches!(m1.outcome, Outcome::Preview(_)), "pertama harus Preview");
        if let Outcome::Preview(d) = m1.outcome {
            let s = d.pixels.image.size;
            assert!(s[0] * s[1] <= 260_000, "pratinjau {}", s[0] * s[1]);
        }
        let m2 = out_rx.recv_timeout(Duration::from_secs(60)).unwrap();
        assert!(matches!(m2.outcome, Outcome::Ready(_)), "kedua harus Ready");
        if let Outcome::Ready(d) = m2.outcome {
            let s = d.pixels.image.size;
            assert!(s[0] * s[1] <= 2_000_000, "tajam {}", s[0] * s[1]);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn jpeg_besar_didekode_dengan_dimensi_dan_warna_benar() {
        let data = jpeg_bytes(3000, 2000);
        let p = decode_sharp(&data, 4096, max_pixels()).unwrap();
        assert_eq!(p.orig, [3000, 2000]);
        assert_eq!(p.image.size, [3000, 2000]);
        assert!(p.frames.is_none());
        // merah solid (toleransi kompresi JPEG)
        let c = p.image.pixels[0];
        assert!(c.r() > 150 && c.g() < 100 && c.b() < 100, "{c:?}");
        assert_eq!(p.format, "JPEG");
    }

    #[test]
    fn orientasi_diterapkan_setelah_resize_bukan_sebelum() {
        // Simulasi foto portrait dengan EXIF Rotate90: piksel 2000x3000,
        // dimensi terorientasi 3000x2000.
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(2000, 3000, Rgb([1, 2, 3])));
        let raw = RawImage {
            img,
            orientation: Orientation::Rotate90,
        };
        let p = finish_raw(raw, 1024, max_pixels()).unwrap();
        assert_eq!(p.orig, [3000, 2000]);
        assert_eq!(p.image.size, [1024, 683]);
    }

    #[test]
    fn shrink_cepat_menurunkan_ukuran_dengan_warna_benar() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(4000, 3000, Rgb([10, 200, 30])));
        let small = shrink_to(img, 1000, 750);
        assert_eq!((small.width(), small.height()), (1000, 750));
        let p = small.into_rgb8().get_pixel(500, 375).0;
        assert!(p[1] > 150 && p[0] < 100 && p[2] < 100, "{p:?}");
        // RGBA (ada alfa) juga lewat jalur cepat
        let a = DynamicImage::ImageRgba8(RgbaImage::from_pixel(2000, 1000, Rgba([1, 2, 3, 255])));
        let small_a = shrink_to(a, 500, 250);
        assert_eq!((small_a.width(), small_a.height()), (500, 250));
    }

    #[test]
    fn sumbu_bertukar_hanya_untuk_putaran_90_270() {
        assert!(swaps_axes(Orientation::Rotate90));
        assert!(swaps_axes(Orientation::Rotate270));
        assert!(swaps_axes(Orientation::Rotate90FlipH));
        assert!(swaps_axes(Orientation::Rotate270FlipH));
        assert!(!swaps_axes(Orientation::NoTransforms));
        assert!(!swaps_axes(Orientation::Rotate180));
        assert!(!swaps_axes(Orientation::FlipHorizontal));
        assert!(!swaps_axes(Orientation::FlipVertical));
        assert_eq!(oriented_dims(100, 200, Orientation::Rotate90), (200, 100));
        assert_eq!(oriented_dims(100, 200, Orientation::NoTransforms), (100, 200));
    }

    fn gif_bytes(frames: &[([u8; 4], u32)], w: u32, h: u32) -> Vec<u8> {
        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Delay, Frame};
        let mut out = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut out);
            enc.set_repeat(Repeat::Infinite).unwrap();
            for (rgba, delay_ms) in frames {
                let img = RgbaImage::from_pixel(w, h, Rgba(*rgba));
                let f = Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(*delay_ms, 1));
                enc.encode_frame(f).unwrap();
            }
        }
        out
    }

    #[test]
    fn gif_animasi_terdekode_semua_frame_dengan_jeda() {
        let data = gif_bytes(
            &[([255, 0, 0, 255], 50), ([0, 255, 0, 255], 120), ([0, 0, 255, 255], 5)],
            24,
            16,
        );
        let p = decode_sharp(&data, 4096, max_pixels()).unwrap();
        let frames = p.frames.expect("harus dikenali sebagai animasi");
        assert_eq!(frames.len(), 3);
        assert_eq!(p.orig, [24, 16]);
        assert!(!p.truncated);
        assert_eq!(frames[0].delay, Duration::from_millis(50));
        assert_eq!(frames[1].delay, Duration::from_millis(120));
        // jeda < 20 ms dinaikkan ke 100 ms seperti peramban
        assert_eq!(frames[2].delay, Duration::from_millis(100));
        assert_eq!(frames[0].image.pixels[0].r(), 255);
        assert_eq!(frames[1].image.pixels[0].g(), 255);
        assert_eq!(frames[2].image.pixels[0].b(), 255);
        assert!(Arc::ptr_eq(&p.image, &frames[0].image));
    }

    #[test]
    fn gif_satu_frame_diperlakukan_sebagai_gambar_statis() {
        let data = gif_bytes(&[([10, 20, 30, 255], 100)], 8, 8);
        let p = decode_sharp(&data, 4096, max_pixels()).unwrap();
        assert!(p.frames.is_none());
        assert_eq!(p.image.size, [8, 8]);
    }

    #[test]
    fn gif_animasi_besar_diperkecil_semua_frame() {
        let data = gif_bytes(&[([1, 2, 3, 255], 40), ([4, 5, 6, 255], 40)], 3000, 1500);
        let p = decode_sharp(&data, 1024, max_pixels()).unwrap();
        let frames = p.frames.unwrap();
        assert_eq!(p.orig, [3000, 1500]);
        assert!(frames.iter().all(|f| f.image.size == [1024, 512]));
    }

    #[test]
    fn gif_terpotong_dekoder_tetap_menampilkan_frame_yang_terbaca() {
        let data = gif_bytes(
            &[([9, 9, 9, 255], 40), ([8, 8, 8, 255], 40), ([7, 7, 7, 255], 40)],
            64,
            64,
        );
        let cut = &data[..data.len() - 40];
        // Boleh berupa animasi parsial atau galat, tetapi tidak boleh panik.
        match decode_sharp(cut, 4096, max_pixels()) {
            Ok(p) => assert!(p.image.size == [64, 64]),
            Err(e) => assert!(!e.is_empty()),
        }
    }

    #[cfg(feature = "heif")]
    #[test]
    fn heic_dan_avif_terdekode_dengan_warna_benar() {
        let heic: &[u8] = include_bytes!("../tests/fixtures/merah.heic");
        let avif: &[u8] = include_bytes!("../tests/fixtures/merah.avif");
        for (nama, data) in [("heic", heic), ("avif", avif)] {
            let p = decode_sharp(data, 4096, max_pixels()).unwrap_or_else(|e| panic!("{nama}: {e}"));
            assert_eq!(p.orig, [48, 32], "{nama}");
            assert!(p.frames.is_none(), "{nama}");
            let c = p.image.pixels[0]; // pojok kiri-atas: merah solid
            assert!(
                c.r() > 170 && c.g() < 100 && c.b() < 100,
                "{nama}: warna {c:?} bukan merah"
            );
        }
    }

    #[cfg(feature = "heif")]
    #[test]
    fn heic_rusak_memberi_galat_bukan_panik() {
        let heic: &[u8] = include_bytes!("../tests/fixtures/merah.heic");
        assert!(decode_sharp(&heic[..heic.len() / 2], 4096, max_pixels()).is_err());
        let mut sampah = heic.to_vec();
        for b in sampah.iter_mut().skip(12) {
            *b = 0xA5;
        }
        assert!(decode_sharp(&sampah, 4096, max_pixels()).is_err());
    }

    #[test]
    fn data_sampah_memberi_galat_bukan_panik() {
        assert!(decode_sharp(b"ini bukan gambar", 4096, max_pixels()).is_err());
        assert!(decode_sharp(&[], 4096, max_pixels()).is_err());
        let mut rusak = png_bytes(64, 64);
        rusak.truncate(rusak.len() / 2);
        assert!(decode_sharp(&rusak, 4096, max_pixels()).is_err());
    }

    #[test]
    fn nama_format_dikenali_dari_isi_bukan_ekstensi() {
        assert_eq!(format_name(&png_bytes(4, 4)), "PNG");
        assert_eq!(format_name(b"GIF89a\x01\x00\x01\x00\x00\x00\x00;"), "GIF");
        assert_eq!(format_name(b"bukan gambar"), "Tidak diketahui");
        let p = decode_sharp(&png_bytes(4, 4), 4096, max_pixels()).unwrap();
        assert_eq!(p.format, "PNG");
        let g = gif_bytes(&[([1, 2, 3, 255], 50), ([4, 5, 6, 255], 50)], 8, 8);
        assert_eq!(decode_sharp(&g, 4096, max_pixels()).unwrap().format, "GIF (animasi, 2 frame)");
        #[cfg(feature = "heif")]
        {
            let heic: &[u8] = include_bytes!("../tests/fixtures/merah.heic");
            let avif: &[u8] = include_bytes!("../tests/fixtures/merah.avif");
            assert_eq!(format_name(heic), "HEIC/HEIF");
            assert_eq!(format_name(avif), "AVIF");
        }
    }

    #[test]
    fn decode_rgba_untuk_wallpaper_dibatasi_ukuran() {
        let img = decode_rgba(&png_bytes(300, 200), 8192, 40_000_000).unwrap();
        assert_eq!(img.dimensions(), (300, 200));
        let small = decode_rgba(&png_bytes(300, 200), 100, 40_000_000).unwrap();
        assert_eq!(small.dimensions(), (100, 67));
        assert!(decode_rgba(b"sampah", 8192, 1000).is_err());
        let g = gif_bytes(&[([9, 9, 9, 255], 50), ([8, 8, 8, 255], 50)], 8, 8);
        assert_eq!(decode_rgba(&g, 8192, 1_000_000).unwrap().dimensions(), (8, 8));
    }
}




