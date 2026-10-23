//! Thread pemuat: membaca byte dari folder/ZIP lalu mendekode gambar di luar
//! thread UI. Permintaan "utama" (gambar yang sedang dilihat) selalu didahulukan;
//! prefetch yang sudah basi dilewati.

use std::collections::VecDeque;
use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
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
pub const ANIM_BUDGET: usize = 128 * 1024 * 1024;
const MAX_ANIM_FRAMES: usize = 1500;
/// TIFF tidak menyimpan jeda antar-halaman; pakai nilai tetap yang nyaman
/// untuk dilihat (tetap bisa di-step manual per frame).
const TIFF_FRAME_DELAY_MS: u64 = 500;

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
    /// Lanjutkan stream GIF halaman `index` mulai `start_frame`, mengomposit
    /// di ruang tier di atas snapshot kanvas `base` (kanvas SESUDAH frame
    /// `start_frame - 1` diterapkan disposalnya; `None` = kanvas kosong,
    /// hanya sah untuk `start_frame == 0`). Hasil dikirim sebagai
    /// `Outcome::AnimChunk`/`AnimDone`/`AnimFailed` seperti stream awal.
    AnimStream {
        session: u64,
        index: usize,
        start_frame: usize,
        base: Option<Arc<ColorImage>>,
        max_side: usize,
        max_pixels: usize,
    },
    /// Batalkan stream GIF yang sedang berjalan/antre untuk indeks ini
    /// (stream lama digantikan stream baru, atau halamannya dibuang).
    CancelAnim { session: u64, index: usize },
    /// Simpan frame ke-`frame` dari GIF animasi halaman `index` sebagai PNG
    /// di `dest`, didekode penuh berurutan di worker (UI tidak membeku).
    /// Hasil: `Outcome::AnimSaved` / `Outcome::AnimJobFailed`.
    SaveAnimFrame {
        session: u64,
        index: usize,
        frame: usize,
        dest: PathBuf,
        max_side: usize,
        max_pixels: usize,
    },
    /// Ekspor SEMUA frame GIF animasi halaman `index` sebagai PNG ke `dir`
    /// (nama `{stem}_frameNNN.png`), didekode berurutan di worker.
    /// Hasil: `Outcome::AnimExported` / `Outcome::AnimJobFailed`.
    ExportAnimFrames {
        session: u64,
        index: usize,
        dir: PathBuf,
        stem: String,
        max_side: usize,
        max_pixels: usize,
    },
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

/// Metode disposal satu frame GIF (cara kanvas diperlakukan sesudah frame
/// ini ditampilkan), nilai yang sama seperti `gif::DisposalMethod`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dispose {
    Any,
    Keep,
    Background,
    Previous,
}

impl Dispose {
    fn from_gif(d: gif::DisposalMethod) -> Self {
        match d {
            gif::DisposalMethod::Any => Dispose::Any,
            gif::DisposalMethod::Keep => Dispose::Keep,
            gif::DisposalMethod::Background => Dispose::Background,
            gif::DisposalMethod::Previous => Dispose::Previous,
        }
    }

    /// True bila kanvas sesudah frame ini SAMA dengan gambar frame yang
    /// tampil — hanya saat itu gambar frame tersimpan bisa dipakai sebagai
    /// kanvas dasar untuk mengomposit frame berikutnya.
    pub fn keeps_canvas(self) -> bool {
        matches!(self, Dispose::Any | Dispose::Keep)
    }
}

/// Indeks satu frame GIF dari pemindaian dua fase: semua yang perlu
/// diketahui TANPA mendekode piksel frame itu.
#[derive(Clone, Debug)]
pub struct GifFrameInfo {
    /// Posisi byte awal grup blok frame ini (GCE bila ada, kalau tidak
    /// deskriptor gambarnya) — titik mulai untuk membuka dekoder tepat
    /// di frame ini (dengan awalan header berkas di depannya).
    pub offset: usize,
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
    /// Jeda tampil, sudah dinormalisasi seperti jalur lama
    /// (delay GIF x10 ms; < 20 ms -> 100 ms; maks 10 detik).
    pub delay: Duration,
    pub dispose: Dispose,
    /// Indeks palet transparan frame ini, bila ada.
    pub transparent: Option<u8>,
    /// True bila rect frame menutupi seluruh kanvas logis.
    pub full_cover: bool,
}

/// Indeks lengkap satu berkas GIF animasi (fase satu streaming).
#[derive(Clone, Debug)]
pub struct GifIndex {
    /// Dimensi kanvas logis GIF.
    pub canvas: [u32; 2],
    pub frames: Vec<GifFrameInfo>,
    /// Panjang awalan berkas sebelum grup blok frame pertama
    /// (header + deskriptor layar + palet global + ekstensi awal).
    pub prefix_len: usize,
    /// True bila berkas punya frame melebihi `MAX_ANIM_FRAMES`
    /// (frame selebihnya tidak diindeks dan tidak akan dimuat).
    pub truncated: bool,
}

/// Metadata animasi yang dikirim bersama `Outcome::Preview` versi
/// streaming: total frame dan tabel jeda/disposal SUDAH diketahui penuh
/// sejak awal berkat indeks dua fase, walau frame-frame-nya sendiri
/// masih mengalir lewat `Outcome::AnimChunk`.
pub struct AnimMeta {
    pub total: usize,
    pub delays: Vec<Duration>,
    pub disposes: Vec<Dispose>,
    /// Jarak snapshot checkpoint kanvas (dalam frame) yang dikirim
    /// worker bersama potongan frame.
    pub cadence: usize,
    /// True bila total dipotong pada `MAX_ANIM_FRAMES`.
    pub truncated: bool,
}

pub enum Outcome {
    Ready(Decoded),
    /// Frame pertama GIF animasi dari jalur streaming, dikirim segera
    /// supaya langsung tampil selagi frame sisanya mengalir. Disusul
    /// `AnimChunk`* lalu tepat satu `AnimDone`/`AnimFailed`.
    Preview(Decoded, AnimMeta),
    /// Potongan frame komposit hasil streaming (frame `start` ..
    /// `start + frames.len()`), ditambahkan app ke halaman yang tampil.
    /// `checkpoints` berisi snapshot kanvas (resolusi tier) SESUDAH
    /// frame berindeks itu diterapkan disposalnya.
    AnimChunk {
        start: usize,
        frames: Vec<AnimFrame>,
        checkpoints: Vec<(usize, Arc<ColorImage>)>,
    },
    /// Stream GIF selesai: semua frame sampai `total` sudah dikirim.
    AnimDone { total: usize },
    /// Stream GIF putus di tengah: frame sebelum `from_frame` tetap
    /// tampil, sisanya tidak termuat.
    AnimFailed { from_frame: usize, message: String },
    /// Ekspor frame selesai (atau berhenti lebih awal: `done < total`).
    AnimExported { done: usize, total: usize, dir: PathBuf },
    /// Satu frame animasi tersimpan sebagai PNG lewat worker.
    AnimSaved { dest: PathBuf },
    /// Tugas animasi sampingan (simpan/ekspor frame) gagal.
    AnimJobFailed { message: String },
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

/// Tugas animasi sampingan untuk worker utama, diproses di sela-sela
/// antrean `Load` (sesudah permintaan utama, sebelum diam total).
enum AnimTask {
    Stream {
        index: usize,
        start_frame: usize,
        base: Option<Arc<ColorImage>>,
        max_side: usize,
        max_pixels: usize,
    },
    SaveFrame {
        index: usize,
        frame: usize,
        dest: PathBuf,
        max_side: usize,
        max_pixels: usize,
    },
    Export {
        index: usize,
        dir: PathBuf,
        stem: String,
        max_side: usize,
        max_pixels: usize,
    },
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
    /// Kanal perintah, disimpan supaya loop streaming bisa menguras
    /// perintah yang datang di tengah stream (batal / sesi baru).
    rx: Option<Receiver<Command>>,
    anim_tasks: VecDeque<AnimTask>,
    /// (sesi, indeks) stream yang SEDANG berjalan di worker ini.
    stream_key: Option<(u64, usize)>,
    /// Ditetapkan saat stream berjalan harus berhenti di potongan berikut.
    abort_stream: bool,
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
                    rx: None,
                    anim_tasks: VecDeque::new(),
                    stream_key: None,
                    abort_stream: false,
                }
                .run(rx)
            })?;
    }
    Ok((main_tx, pref_tx, out_rx))
}

impl Worker {
    fn run(mut self, rx: Receiver<Command>) {
        self.rx = Some(rx);
        loop {
            if self.queue.is_empty() && self.anim_tasks.is_empty() {
                match self.rx.as_ref().map(|r| r.recv()) {
                    Some(Ok(c)) => self.apply(c),
                    _ => return, // UI sudah tutup
                }
            }
            self.drain_commands();
            if let Some(job) = self.next_job() {
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
            } else if let Some(task) = self.anim_tasks.pop_front() {
                self.process_anim_task(task);
            }
        }
    }

    /// Terapkan semua perintah yang sudah menunggu di kanal. Dipakai juga
    /// dari tengah loop streaming: perintah `Open`/`CancelAnim` yang tiba
    /// saat stream berjalan harus segera terasa efeknya.
    fn drain_commands(&mut self) {
        let Some(rx) = self.rx.take() else {
            return;
        };
        while let Ok(c) = rx.try_recv() {
            self.apply(c);
        }
        self.rx = Some(rx);
    }

    fn apply(&mut self, cmd: Command) {
        match cmd {
            Command::Open { session, listing } => {
                self.session = session;
                self.listing = Some(listing);
                self.reader = None;
                self.queue.clear();
                self.anim_tasks.clear();
                // Stream sesi lama (kalau sedang berjalan) berhenti di
                // potongan berikutnya.
                self.abort_stream = true;
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
            Command::AnimStream {
                session,
                index,
                start_frame,
                base,
                max_side,
                max_pixels,
            } => {
                if !self.handles_primary || session != self.session {
                    return;
                }
                // Stream baru untuk indeks yang sama menggantikan yang antre.
                self.anim_tasks.retain(|t| {
                    !matches!(t, AnimTask::Stream { index: i, .. } if *i == index)
                });
                self.anim_tasks.push_back(AnimTask::Stream {
                    index,
                    start_frame,
                    base,
                    max_side,
                    max_pixels,
                });
            }
            Command::CancelAnim { session, index } => {
                if session != self.session {
                    return;
                }
                self.anim_tasks.retain(|t| {
                    !matches!(t, AnimTask::Stream { index: i, .. } if *i == index)
                });
                if self.stream_key == Some((session, index)) {
                    self.abort_stream = true;
                }
            }
            Command::SaveAnimFrame {
                session,
                index,
                frame,
                dest,
                max_side,
                max_pixels,
            } => {
                if !self.handles_primary || session != self.session {
                    return;
                }
                self.anim_tasks.push_back(AnimTask::SaveFrame {
                    index,
                    frame,
                    dest,
                    max_side,
                    max_pixels,
                });
            }
            Command::ExportAnimFrames {
                session,
                index,
                dir,
                stem,
                max_side,
                max_pixels,
            } => {
                if !self.handles_primary || session != self.session {
                    return;
                }
                self.anim_tasks.push_back(AnimTask::Export {
                    index,
                    dir,
                    stem,
                    max_side,
                    max_pixels,
                });
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

    /// Baca byte satu entri listing lewat reader sesi ini (dibuka malas).
    /// Dipakai jalur `Load` maupun tugas animasi; pesan galatnya sama.
    fn read_listing_bytes(&mut self, index: usize) -> Result<Vec<u8>, String> {
        let Some(listing) = self.listing.clone() else {
            return Err("Tidak ada sumber yang terbuka".into());
        };
        if self.reader.is_none() {
            self.reader = Some(listing.open_reader());
        }
        match self.reader.as_mut() {
            Some(Ok(r)) => r.read(index),
            Some(Err(e)) => Err(e.clone()),
            None => Err("Pembaca sumber tidak tersedia".into()),
        }
    }

    fn process(&mut self, job: &Job) -> Outcome {
        if !job.primary && job.index.abs_diff(self.focus) > PREFETCH_RADIUS {
            return Outcome::Skipped;
        }
        let bytes = match self.read_listing_bytes(job.index) {
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
        // GIF animasi pada permintaan utama: jalur streaming (indeks dua
        // fase, frame pertama segera, sisa frame mengalir sebagai
        // potongan). Semua pesannya dikirim langsung dari sini; `true`
        // berarti berkas ini sudah tertangani penuh (termasuk galatnya).
        // Bukan GIF animasi -> `false`, jalur decode biasa di bawah.
        if job.primary && self.stream_gif_initial(job, &bytes) {
            return Outcome::Skipped;
        }
        match catch_unwind(AssertUnwindSafe(|| {
            decode_progressive(&bytes, job.max_side, job.max_pixels)
        })) {
            Ok(Ok(sharp)) => Outcome::Ready(Decoded {
                pixels: sharp,
                file_size,
            }),
            Ok(Err(e)) => Outcome::Failed(e),
            Err(_) => Outcome::Failed(
                "Dekoder gagal memproses berkas ini (data kemungkinan rusak)".into(),
            ),
        }
    }
}

// ------------------------------------------------------------ streaming GIF
//
// Jalur khusus GIF animasi untuk permintaan utama. Dua fase:
// (1) INDEKS — struktur blok dipindai mentah (offset tiap frame) dan
//     metadata semua frame dibaca lewat dekoder `gif` dengan
//     `skip_frame_decoding` (tanpa mendekode piksel), jadi total frame,
//     jeda, rect, dan disposal diketahui sejak awal;
// (2) STREAM — dekoder dibuka tepat di offset frame yang dibutuhkan dan
//     frame dikomposit SATU per satu ke satu kanvas (semantik disposal
//     sama persis dengan iterator `image::codecs::gif`), frame pertama
//     dikirim segera sebagai `Preview`, sisanya sebagai `AnimChunk`.
// Karena komposit dilakukan berurutan dari SATU aliran, frame lama yang
// terdesak anggaran di sisi app bisa dibuat ulang kapan pun dari
// checkpoint + offset indeks — tidak ada lagi pemotongan permanen.

/// Pindai struktur blok GIF mentah: posisi awal grup blok tiap frame
/// (dimulai dari GCE frame itu bila ada, kalau tidak dari deskriptor
/// gambarnya). `None` = struktur tidak dikenali (pemanggil jatuh ke
/// jalur lama); berkas terpotong juga menghasilkan `None` di sini —
/// jalur lama yang memutuskan galat/parsialnya.
fn gif_block_offsets(bytes: &[u8]) -> Option<Vec<usize>> {
    if bytes.len() < 14 || !matches!(&bytes[0..6], b"GIF87a" | b"GIF89a") {
        return None;
    }
    // Lewati rangkaian sub-blok data: (ukuran, isi)* diakhiri ukuran 0.
    fn skip_sub_blocks(bytes: &[u8], mut pos: usize) -> Option<usize> {
        loop {
            let n = *bytes.get(pos)? as usize;
            pos += 1;
            if n == 0 {
                return Some(pos);
            }
            pos = pos.checked_add(n)?;
            if pos > bytes.len() {
                return None;
            }
        }
    }
    let packed = *bytes.get(10)?;
    let mut pos = 13usize;
    if packed & 0x80 != 0 {
        // Palet global: 3 * 2^(N+1) byte.
        pos = pos.checked_add(3usize << ((packed & 0x07) + 1))?;
    }
    let mut offsets = Vec::new();
    // Awal grup blok frame berikutnya (ekstensi pertama sesudah frame
    // sebelumnya); GCE sebuah frame selalu di dalam grupnya sendiri.
    let mut group_start: Option<usize> = None;
    loop {
        let b = *bytes.get(pos)?;
        match b {
            0x3B => break, // trailer
            0x21 => {
                // Ekstensi: label 1 byte lalu sub-blok (GCE pun berbentuk
                // sub-blok berukuran 4, jadi aturan umum ini berlaku).
                if group_start.is_none() {
                    group_start = Some(pos);
                }
                pos = skip_sub_blocks(bytes, pos + 2)?;
            }
            0x2C => {
                let start = group_start.take().unwrap_or(pos);
                offsets.push(start);
                // Deskriptor gambar: 9 byte sesudah 0x2C; byte terakhir
                // (packed) menentukan palet lokal.
                let desc_packed = *bytes.get(pos + 9)?;
                pos += 10;
                if desc_packed & 0x80 != 0 {
                    pos = pos.checked_add(3usize << ((desc_packed & 0x07) + 1))?;
                }
                pos += 1; // LZW minimum code size
                pos = skip_sub_blocks(bytes, pos)?;
            }
            _ => return None,
        }
    }
    if offsets.is_empty() {
        None
    } else {
        Some(offsets)
    }
}

fn gif_memory_limit() -> gif::MemoryLimit {
    match std::num::NonZeroU64::new(alloc_limit().max(1)) {
        Some(n) => gif::MemoryLimit::Bytes(n),
        None => gif::MemoryLimit::Unlimited,
    }
}

/// Fase satu streaming: bangun indeks lengkap GIF dari `bytes`.
/// `None` = bukan GIF, bukan animasi (< 2 frame), strukturnya tidak
/// konsisten, atau berkasnya rusak di tingkat header — dalam semua
/// kasus itu jalur decode lama yang menangani (dan melaporkan galatnya)
/// supaya perilaku di luar jalur streaming tidak berubah sedikit pun.
fn scan_gif_index(bytes: &[u8]) -> Option<GifIndex> {
    if image::guess_format(bytes).ok() != Some(ImageFormat::Gif) {
        return None;
    }
    let offsets = gif_block_offsets(bytes)?;
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::RGBA);
    opts.skip_frame_decoding(true);
    opts.set_memory_limit(gif_memory_limit());
    let mut dec = opts.read_info(Cursor::new(bytes)).ok()?;
    let (cw, ch) = (u32::from(dec.width()), u32::from(dec.height()));
    // Bom dimensi ditolak jalur lama dengan pesan yang jelas; di sini
    // cukup jangan masuk jalur streaming.
    check_canvas(cw, ch).ok()?;
    let mut frames: Vec<GifFrameInfo> = Vec::new();
    let mut truncated = false;
    loop {
        let info = match dec.next_frame_info() {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => return None,
        };
        if frames.len() >= MAX_ANIM_FRAMES {
            truncated = true;
            break;
        }
        // Sama seperti `image`: delay satuan 10 ms, lalu normalisasi
        // peramban yang sama seperti `decode_animated`.
        let ms_raw = u32::from(info.delay) * 10;
        let ms = if ms_raw < 20 { 100 } else { ms_raw.min(10_000) };
        let (left, top, w, h) = (
            u32::from(info.left),
            u32::from(info.top),
            u32::from(info.width),
            u32::from(info.height),
        );
        frames.push(GifFrameInfo {
            offset: 0, // diisi di bawah sesudah jumlahnya cocok
            left,
            top,
            width: w,
            height: h,
            delay: Duration::from_millis(u64::from(ms)),
            dispose: Dispose::from_gif(info.dispose),
            transparent: info.transparent,
            full_cover: left == 0 && top == 0 && w == cw && h == ch,
        });
    }
    if frames.len() < 2 {
        return None;
    }
    // Dua pemindai independen harus sepakat soal jumlah frame; bila
    // tidak, struktur berkas ini tidak dipahami — jalur lama saja.
    if truncated {
        if offsets.len() <= frames.len() {
            return None;
        }
    } else if offsets.len() != frames.len() {
        return None;
    }
    for (f, off) in frames.iter_mut().zip(offsets.iter()) {
        f.offset = *off;
    }
    Some(GifIndex {
        canvas: [cw, ch],
        prefix_len: offsets[0],
        frames,
        truncated,
    })
}

/// Campur satu piksel seperti iterator GIF di crate `image`:
/// piksel frame beralfa 0 mempertontonkan piksel kanvas sebelumnya;
/// kanvas diperbarui menurut disposal frame ini.
fn blend_pixel(dispose: Dispose, previous: &mut image::Rgba<u8>, current: &mut image::Rgba<u8>) {
    if current.0[3] == 0 {
        *current = *previous;
    }
    match dispose {
        Dispose::Any | Dispose::Keep => *previous = *current,
        Dispose::Background => *previous = image::Rgba([0, 0, 0, 0]),
        Dispose::Previous => {}
    }
}

/// Komposit `frame` (RGBA seukuran rect-nya) ke `canvas` pada posisi
/// (left, top) dengan aturan disposal `dispose`; `canvas` bermutasi
/// menjadi keadaan untuk frame BERIKUTNYA dan nilai kembaliannya
/// adalah gambar yang tampil untuk frame ini. Semantiknya meniru
/// persis `GifFrameIterator` di crate `image` (termasuk mengabaikan
/// warna latar, seperti komentar di sana: demi kompatibilitas web).
fn composite_onto(
    canvas: &mut RgbaImage,
    frame: &RgbaImage,
    left: u32,
    top: u32,
    dispose: Dispose,
) -> RgbaImage {
    let (cw, ch) = canvas.dimensions();
    let mut out = canvas.clone();
    let x0 = left.min(cw);
    let y0 = top.min(ch);
    let x1 = left.saturating_add(frame.width()).min(cw);
    let y1 = top.saturating_add(frame.height()).min(ch);
    for y in y0..y1 {
        for x in x0..x1 {
            let mut cur = *frame.get_pixel(x - left, y - top);
            let prev = canvas.get_pixel_mut(x, y);
            blend_pixel(dispose, prev, &mut cur);
            *out.get_pixel_mut(x, y) = cur;
        }
    }
    out
}

/// Jarak snapshot checkpoint (dalam frame) untuk animasi sepanjang
/// `total` dengan biaya `frame_bytes` per frame: cukup rapat supaya
/// decode-ulang dari checkpoint murah, cukup jarang supaya snapshotnya
/// sendiri tidak menghabiskan anggaran frame.
fn checkpoint_cadence(total: usize, frame_bytes: usize) -> usize {
    if total == 0 || frame_bytes == 0 {
        return 1;
    }
    let slots = (ANIM_BUDGET / frame_bytes).max(1);
    let want = (slots / 3).clamp(1, 8);
    total.div_ceil(want).max(1)
}

/// Aliran dekode GIF yang dibuka tepat di satu frame indeks dan
/// menghasilkan gambar komposit (resolusi penuh) berurutan dari sana.
struct GifPlayer {
    dec: gif::Decoder<Cursor<Vec<u8>>>,
    /// Kanvas post-disposal (keadaan untuk frame berikutnya).
    canvas: RgbaImage,
    buf: Vec<u8>,
}

impl GifPlayer {
    fn open(bytes: &[u8], gi: &GifIndex, start: usize) -> Result<GifPlayer, String> {
        let off = gi.frames[start].offset;
        // Aliran = awalan berkas (header + palet global) + byte dari
        // grup blok frame `start` sampai akhir berkas. Palet lokal dan
        // GCE tiap frame ikut di dalam grup bloknya masing-masing.
        let mut stream = Vec::with_capacity(bytes.len().saturating_sub(off) + gi.prefix_len);
        stream.extend_from_slice(&bytes[..gi.prefix_len]);
        stream.extend_from_slice(&bytes[off..]);
        let mut opts = gif::DecodeOptions::new();
        opts.set_color_output(gif::ColorOutput::RGBA);
        opts.set_memory_limit(gif_memory_limit());
        let dec = opts
            .read_info(Cursor::new(stream))
            .map_err(|_| "GIF rusak atau tidak lengkap".to_string())?;
        Ok(GifPlayer {
            dec,
            canvas: RgbaImage::from_pixel(gi.canvas[0], gi.canvas[1], image::Rgba([0, 0, 0, 0])),
            buf: Vec::new(),
        })
    }

    /// Frame mentah berikutnya: piksel RGBA seukuran rect frame itu
    /// sendiri (belum dikomposit), beserta posisi dan disposalnya.
    /// `None` bila aliran habis.
    fn next_raw(&mut self) -> Option<Result<(RgbaImage, u32, u32, Dispose), String>> {
        let (left, top, fw, fh, dispose) = match self.dec.next_frame_info() {
            Ok(Some(f)) => (
                u32::from(f.left),
                u32::from(f.top),
                u32::from(f.width),
                u32::from(f.height),
                Dispose::from_gif(f.dispose),
            ),
            Ok(None) => return None,
            Err(e) => return Some(Err(format!("Animasi rusak: {e}"))),
        };
        let need = self.dec.buffer_size();
        let mut buf = std::mem::take(&mut self.buf);
        if buf.len() != need {
            buf = vec![0; need];
        }
        if let Err(e) = self.dec.read_into_buffer(&mut buf) {
            return Some(Err(format!("Animasi rusak: {e}")));
        }
        match RgbaImage::from_raw(fw, fh, buf) {
            Some(r) => Some(Ok((r, left, top, dispose))),
            None => Some(Err("Animasi rusak: ukuran frame tidak cocok".into())),
        }
    }

    /// Frame komposit berikutnya (resolusi penuh, sebelum shrink tier),
    /// atau `None` bila aliran habis.
    fn next_frame(&mut self) -> Option<Result<RgbaImage, String>> {
        match self.next_raw() {
            Some(Ok((raw, left, top, dispose))) => {
                Some(Ok(composite_onto(&mut self.canvas, &raw, left, top, dispose)))
            }
            Some(Err(e)) => Some(Err(e)),
            None => None,
        }
    }

    /// Snapshot kanvas saat ini (post-disposal) pada resolusi tier:
    /// inilah checkpoint yang disimpan app untuk stream lanjutan.
    fn canvas_tier(&self, max_side: u32, max_pixels: usize) -> ColorImage {
        to_color_image(shrink(
            DynamicImage::ImageRgba8(self.canvas.clone()),
            max_side,
            max_pixels,
        ))
    }
}

/// Ubah `ColorImage` (egui, alfa premultiplied) menjadi `RgbaImage`
/// beralfa lurus — kembaran helper di app.rs, untuk tugas worker yang
/// menyimpan PNG dari snapshot checkpoint.
fn color_image_to_rgba(img: &ColorImage) -> RgbaImage {
    let w = img.width() as u32;
    let h = img.height() as u32;
    let mut raw = Vec::with_capacity(img.pixels.len() * 4);
    for p in &img.pixels {
        let [r, g, b, a] = p.to_array();
        if a == 0 {
            raw.extend_from_slice(&[0, 0, 0, 0]);
        } else if a == 255 {
            raw.extend_from_slice(&[r, g, b, 255]);
        } else {
            let un = |c: u8| ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8;
            raw.extend_from_slice(&[un(r), un(g), un(b), a]);
        }
    }
    RgbaImage::from_raw(w, h, raw).expect("jumlah piksel cocok dengan dimensi")
}

impl Worker {
    /// Kirim satu pesan hasil dari tengah sebuah stream/tugas.
    fn send_loaded(&self, index: usize, outcome: Outcome) {
        let msg = Loaded {
            session: self.session,
            index,
            outcome,
        };
        if self.tx.send(msg).is_ok() {
            self.ctx.request_repaint();
        }
    }

    /// Kuras perintah yang menunggu; kembalikan `true` bila stream yang
    /// sedang berjalan untuk `index` harus berhenti sekarang (dibatalkan
    /// app atau sesi berganti).
    fn stream_aborted(&mut self, index: usize) -> bool {
        self.drain_commands();
        if self.abort_stream {
            self.abort_stream = false;
            self.stream_key = None;
            let _ = index;
            return true;
        }
        false
    }

    /// Gerbang streaming untuk permintaan utama: `true` = berkas ini GIF
    /// animasi dan sudah tertangani penuh lewat pesan-pesan streaming
    /// (termasuk galatnya); `false` = bukan ranah streaming, jalur
    /// decode biasa yang melanjutkan.
    fn stream_gif_initial(&mut self, job: &Job, bytes: &[u8]) -> bool {
        if image::guess_format(bytes).ok() != Some(ImageFormat::Gif) {
            return false;
        }
        match catch_unwind(AssertUnwindSafe(|| self.stream_gif_initial_inner(job, bytes))) {
            Ok(h) => h,
            Err(_) => {
                self.stream_key = None;
                self.send_loaded(
                    job.index,
                    Outcome::AnimFailed {
                        from_frame: 0,
                        message: "Dekoder gagal memproses berkas ini (data kemungkinan rusak)"
                            .into(),
                    },
                );
                true
            }
        }
    }

    fn stream_gif_initial_inner(&mut self, job: &Job, bytes: &[u8]) -> bool {
        let Some(gi) = scan_gif_index(bytes) else {
            return false;
        };
        let max_side = job.max_side.clamp(1024, 16384) as u32;
        let total = gi.frames.len();
        let (tw, th) = fit_target(gi.canvas[0], gi.canvas[1], max_side, job.max_pixels);
        let cadence = checkpoint_cadence(total, tw as usize * th as usize * 4);
        let mut player = match GifPlayer::open(bytes, &gi, 0) {
            Ok(p) => p,
            Err(e) => {
                self.send_loaded(job.index, Outcome::Failed(e));
                return true;
            }
        };
        self.stream_key = Some((self.session, job.index));
        self.abort_stream = false;
        let mut chunk: Vec<AnimFrame> = Vec::new();
        let mut chunk_start = 1;
        let mut cps: Vec<(usize, Arc<ColorImage>)> = Vec::new();
        for i in 0..total {
            match player.next_frame() {
                Some(Ok(displayed)) => {
                    let frame = AnimFrame {
                        image: Arc::new(to_color_image(shrink(
                            DynamicImage::ImageRgba8(displayed),
                            max_side,
                            job.max_pixels,
                        ))),
                        delay: gi.frames[i].delay,
                    };
                    if i == 0 {
                        let meta = AnimMeta {
                            total,
                            delays: gi.frames.iter().map(|f| f.delay).collect(),
                            disposes: gi.frames.iter().map(|f| f.dispose).collect(),
                            cadence,
                            truncated: gi.truncated,
                        };
                        let px = Pixels {
                            image: frame.image.clone(),
                            orig: gi.canvas,
                            frames: Some(vec![frame]),
                            truncated: false,
                            format: format!(
                                "{} (animasi, {} frame)",
                                format_name(bytes),
                                total
                            ),
                        };
                        self.send_loaded(
                            job.index,
                            Outcome::Preview(
                                Decoded {
                                    pixels: px,
                                    file_size: bytes.len() as u64,
                                },
                                meta,
                            ),
                        );
                    } else {
                        if chunk.is_empty() {
                            chunk_start = i;
                        }
                        chunk.push(frame);
                    }
                    if i % cadence == 0 {
                        cps.push((i, Arc::new(player.canvas_tier(max_side, job.max_pixels))));
                    }
                    let last = i + 1 == total;
                    if !chunk.is_empty() && (chunk.len() >= 4 || last) {
                        let frames = std::mem::take(&mut chunk);
                        let checkpoints = std::mem::take(&mut cps);
                        self.send_loaded(
                            job.index,
                            Outcome::AnimChunk {
                                start: chunk_start,
                                frames,
                                checkpoints,
                            },
                        );
                        if self.stream_aborted(job.index) {
                            return true;
                        }
                    }
                }
                Some(Err(e)) => {
                    self.stream_key = None;
                    if i == 0 {
                        self.send_loaded(job.index, Outcome::Failed(e));
                    } else {
                        self.send_loaded(
                            job.index,
                            Outcome::AnimFailed {
                                from_frame: i,
                                message: e,
                            },
                        );
                    }
                    return true;
                }
                None => {
                    self.stream_key = None;
                    let message =
                        "Animasi rusak: aliran frame berhenti lebih awal".to_string();
                    if i == 0 {
                        self.send_loaded(job.index, Outcome::Failed(message));
                    } else {
                        self.send_loaded(
                            job.index,
                            Outcome::AnimFailed {
                                from_frame: i,
                                message,
                            },
                        );
                    }
                    return true;
                }
            }
        }
        if !cps.is_empty() {
            self.send_loaded(
                job.index,
                Outcome::AnimChunk {
                    start: total,
                    frames: Vec::new(),
                    checkpoints: std::mem::take(&mut cps),
                },
            );
        }
        self.stream_key = None;
        self.send_loaded(job.index, Outcome::AnimDone { total });
        true
    }

    /// Stream lanjutan (seek / putar ulang dari checkpoint): komposit
    /// dilakukan di ruang tier di atas snapshot kanvas dari app.
    fn stream_gif_seek(
        &mut self,
        index: usize,
        start: usize,
        base: Option<Arc<ColorImage>>,
        max_side: usize,
        max_pixels: usize,
        bytes: &[u8],
        gi: &GifIndex,
    ) {
        let max_side_c = max_side.clamp(1024, 16384) as u32;
        let total = gi.frames.len();
        if start >= total {
            self.send_loaded(index, Outcome::AnimDone { total });
            return;
        }
        let (tw, th) = fit_target(gi.canvas[0], gi.canvas[1], max_side_c, max_pixels);
        let cadence = checkpoint_cadence(total, tw as usize * th as usize * 4);
        let mut player = match GifPlayer::open(bytes, gi, start) {
            Ok(p) => p,
            Err(e) => {
                self.send_loaded(
                    index,
                    Outcome::AnimFailed {
                        from_frame: start,
                        message: e,
                    },
                );
                return;
            }
        };
        if let Some(img) = &base {
            // Komposit WAJIB di resolusi penuh: rect GIF teroptimasi
            // penuh piksel transparan (penanda "tidak berubah"), dan
            // mencampur rect yang sudah di-shrink merusak alfa piksel
            // campuran (noise gelap di seluruh kanvas — terbukti di uji
            // GUI). Jadi snapshot dasar tier di-upscale menjadi kanvas
            // penuh; daerah yang dicat ulang frame sesudahnya eksak,
            // daerah statis membawa snapshot tier (satu putaran
            // resample, tak terlihat di skala tier).
            let rgba = color_image_to_rgba(img);
            player.canvas =
                shrink_to(DynamicImage::ImageRgba8(rgba), gi.canvas[0], gi.canvas[1])
                    .into_rgba8();
        }
        self.stream_key = Some((self.session, index));
        self.abort_stream = false;
        let mut chunk: Vec<AnimFrame> = Vec::new();
        let mut chunk_start = start;
        let mut cps: Vec<(usize, Arc<ColorImage>)> = Vec::new();
        for i in start..total {
            match player.next_frame() {
                Some(Ok(displayed)) => {
                    if chunk.is_empty() {
                        chunk_start = i;
                    }
                    chunk.push(AnimFrame {
                        image: Arc::new(to_color_image(shrink(
                            DynamicImage::ImageRgba8(displayed),
                            max_side_c,
                            max_pixels,
                        ))),
                        delay: gi.frames[i].delay,
                    });
                    if i % cadence == 0 {
                        cps.push((i, Arc::new(player.canvas_tier(max_side_c, max_pixels))));
                    }
                    if chunk.len() >= 4 || i + 1 == total {
                        let frames = std::mem::take(&mut chunk);
                        let checkpoints = std::mem::take(&mut cps);
                        self.send_loaded(
                            index,
                            Outcome::AnimChunk {
                                start: chunk_start,
                                frames,
                                checkpoints,
                            },
                        );
                        if self.stream_aborted(index) {
                            return;
                        }
                    }
                }
                Some(Err(e)) => {
                    self.stream_key = None;
                    self.send_loaded(
                        index,
                        Outcome::AnimFailed {
                            from_frame: i,
                            message: e,
                        },
                    );
                    return;
                }
                None => {
                    self.stream_key = None;
                    self.send_loaded(
                        index,
                        Outcome::AnimFailed {
                            from_frame: i,
                            message: "Animasi rusak: aliran frame berhenti lebih awal".into(),
                        },
                    );
                    return;
                }
            }
        }
        self.stream_key = None;
        self.send_loaded(index, Outcome::AnimDone { total });
    }

    fn process_anim_task(&mut self, task: AnimTask) {
        match task {
            AnimTask::Stream {
                index,
                start_frame,
                base,
                max_side,
                max_pixels,
            } => {
                let bytes = match self.read_listing_bytes(index) {
                    Ok(b) => b,
                    Err(e) => {
                        self.send_loaded(
                            index,
                            Outcome::AnimFailed {
                                from_frame: start_frame,
                                message: e,
                            },
                        );
                        return;
                    }
                };
                let Some(gi) = scan_gif_index(&bytes) else {
                    self.send_loaded(
                        index,
                        Outcome::AnimFailed {
                            from_frame: start_frame,
                            message: "Bukan GIF animasi".into(),
                        },
                    );
                    return;
                };
                let r = catch_unwind(AssertUnwindSafe(|| {
                    self.stream_gif_seek(
                        index,
                        start_frame,
                        base,
                        max_side,
                        max_pixels,
                        &bytes,
                        &gi,
                    );
                }));
                if r.is_err() {
                    self.stream_key = None;
                    self.send_loaded(
                        index,
                        Outcome::AnimFailed {
                            from_frame: start_frame,
                            message:
                                "Dekoder gagal memproses berkas ini (data kemungkinan rusak)"
                                    .into(),
                        },
                    );
                }
            }
            AnimTask::SaveFrame {
                index,
                frame,
                dest,
                max_side,
                max_pixels,
            } => {
                let r = catch_unwind(AssertUnwindSafe(|| {
                    self.anim_save_frame(index, frame, &dest, max_side, max_pixels);
                }));
                if r.is_err() {
                    self.send_loaded(
                        index,
                        Outcome::AnimJobFailed {
                            message:
                                "Dekoder gagal memproses berkas ini (data kemungkinan rusak)"
                                    .into(),
                        },
                    );
                }
            }
            AnimTask::Export {
                index,
                dir,
                stem,
                max_side,
                max_pixels,
            } => {
                let r = catch_unwind(AssertUnwindSafe(|| {
                    self.anim_export(index, &dir, &stem, max_side, max_pixels);
                }));
                if r.is_err() {
                    self.send_loaded(
                        index,
                        Outcome::AnimJobFailed {
                            message:
                                "Dekoder gagal memproses berkas ini (data kemungkinan rusak)"
                                    .into(),
                        },
                    );
                }
            }
        }
    }

    /// Simpan satu frame GIF (komposit penuh dari frame 0, pikselnya
    /// persis jalur decode biasa) sebagai PNG.
    fn anim_save_frame(
        &mut self,
        index: usize,
        frame: usize,
        dest: &PathBuf,
        max_side: usize,
        max_pixels: usize,
    ) {
        let fail = |message: String| Outcome::AnimJobFailed { message };
        let bytes = match self.read_listing_bytes(index) {
            Ok(b) => b,
            Err(e) => {
                self.send_loaded(index, fail(e));
                return;
            }
        };
        let Some(gi) = scan_gif_index(&bytes) else {
            self.send_loaded(index, fail("Bukan GIF animasi".into()));
            return;
        };
        if frame >= gi.frames.len() {
            self.send_loaded(index, fail("Frame di luar jangkauan animasi".into()));
            return;
        }
        let mut player = match GifPlayer::open(&bytes, &gi, 0) {
            Ok(p) => p,
            Err(e) => {
                self.send_loaded(index, fail(e));
                return;
            }
        };
        let mut shown: Option<RgbaImage> = None;
        for _ in 0..=frame {
            match player.next_frame() {
                Some(Ok(d)) => shown = Some(d),
                Some(Err(e)) => {
                    self.send_loaded(index, fail(e));
                    return;
                }
                None => {
                    self.send_loaded(
                        index,
                        fail("Animasi rusak: aliran frame berhenti lebih awal".into()),
                    );
                    return;
                }
            }
        }
        let shown = shown.expect("loop di atas selalu mengisi");
        let img = shrink(
            DynamicImage::ImageRgba8(shown),
            max_side.clamp(1024, 16384) as u32,
            max_pixels,
        );
        let rgba = color_image_to_rgba(&to_color_image(img));
        match crate::wallpaper::write_png(&rgba, dest) {
            Ok(()) => self.send_loaded(
                index,
                Outcome::AnimSaved {
                    dest: dest.clone(),
                },
            ),
            Err(e) => self.send_loaded(index, fail(e)),
        }
    }

    /// Ekspor semua frame GIF sebagai PNG: decode penuh berurutan,
    /// satu frame ditulis lalu dilepas — memori tidak menumpuk walau
    /// animasinya panjang.
    fn anim_export(
        &mut self,
        index: usize,
        dir: &PathBuf,
        stem: &str,
        max_side: usize,
        max_pixels: usize,
    ) {
        let fail = |message: String| Outcome::AnimJobFailed { message };
        let bytes = match self.read_listing_bytes(index) {
            Ok(b) => b,
            Err(e) => {
                self.send_loaded(index, fail(e));
                return;
            }
        };
        let Some(gi) = scan_gif_index(&bytes) else {
            self.send_loaded(index, fail("Bukan GIF animasi".into()));
            return;
        };
        let total = gi.frames.len();
        let mut player = match GifPlayer::open(&bytes, &gi, 0) {
            Ok(p) => p,
            Err(e) => {
                self.send_loaded(index, fail(e));
                return;
            }
        };
        let max_side_c = max_side.clamp(1024, 16384) as u32;
        let mut done = 0usize;
        for i in 0..total {
            match player.next_frame() {
                Some(Ok(d)) => {
                    let img = shrink(DynamicImage::ImageRgba8(d), max_side_c, max_pixels);
                    let rgba = color_image_to_rgba(&to_color_image(img));
                    let dest = dir.join(format!("{stem}_frame{:03}.png", i + 1));
                    if crate::wallpaper::write_png(&rgba, &dest).is_err() {
                        break;
                    }
                    done += 1;
                }
                Some(Err(e)) => {
                    self.send_loaded(
                        index,
                        fail(format!("Ekspor berhenti di frame {}: {e}", i + 1)),
                    );
                    return;
                }
                None => {
                    self.send_loaded(
                        index,
                        fail(format!(
                            "Ekspor berhenti di frame {}: aliran frame berakhir",
                            i + 1
                        )),
                    );
                    return;
                }
            }
        }
        self.send_loaded(
            index,
            Outcome::AnimExported {
                done,
                total,
                dir: dir.clone(),
            },
        );
    }
}

/// Dekode progresif: satu kali decode penuh, lalu —
/// - gambar besar (>2500px): pratinjau blur cepat + versi tajam, atau
/// - gambar kecil: langsung versi tajam (tanpa pratinjau).
/// Mengembalikan `(pratinjau, tajam)`.
pub fn decode_progressive(
    bytes: &[u8],
    max_side: usize,
    max_pixels: usize,
) -> Result<Pixels, String> {
    let mut sharp = decode_progressive_inner(bytes, max_side, max_pixels)?;
    let name = format_name(bytes);
    let format = match &sharp.frames {
        Some(f) => format!("{name} (animasi, {} frame)", f.len()),
        None => name,
    };
    sharp.format = format;
    Ok(sharp)
}

fn decode_progressive_inner(
    bytes: &[u8],
    max_side: usize,
    max_pixels: usize,
) -> Result<Pixels, String> {
    let max_side_c = max_side.clamp(1024, 16384) as u32;

    #[cfg(feature = "heif")]
    if is_heif(bytes) {
        return decode_heif(bytes, max_side_c, max_pixels);
    }
    #[cfg(not(feature = "heif"))]
    if looks_like_heif(bytes) {
        return Err("HEIC/AVIF tidak ikut dikompilasi (bangun dengan fitur \"heif\")".into());
    }

    if let Some(p) = decode_animated(bytes, max_side_c, max_pixels)? {
        return Ok(p);
    }
    // Jalur statis (JPEG/PNG/...): satu decode penuh.
    let raw = load_raw(bytes)?;
    finish_raw(raw, max_side_c, max_pixels)
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

/// Dekode berpenjaga untuk jalur sekunder (worker thumbnail, batch convert):
/// pertahanan memori yang sama seperti jalur utama — batas dimensi/alokasi
/// lewat `load_raw`, dan panic dekoder ditangkap menjadi pesan galat. Orientasi
/// EXIF TIDAK diterapkan (sama seperti `image::load_from_memory` yang digantikan
/// fungsi ini), supaya perilaku pemanggil tidak berubah.
pub fn decode_guarded(bytes: &[u8]) -> Result<DynamicImage, String> {
    match catch_unwind(AssertUnwindSafe(|| load_raw(bytes))) {
        Ok(Ok(raw)) => Ok(raw.img),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("Dekoder gagal memproses berkas ini (data kemungkinan rusak)".into()),
    }
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

/// Dekode satu halaman TIFF menjadi RGBA. Hanya tipe warna umum yang
/// didukung (abu/RGB/RGBA 8 & 16 bit); sisanya dianggap halaman rusak.
fn tiff_page_to_rgba(
    dec: &mut tiff::decoder::Decoder<Cursor<&[u8]>>,
) -> Result<RgbaImage, String> {
    use tiff::decoder::DecodingResult;
    let (w, h) = dec
        .dimensions()
        .map_err(|_| "TIFF rusak atau tidak lengkap".to_string())?;
    check_canvas(w, h)?;
    let colortype = dec
        .colortype()
        .map_err(|_| "TIFF rusak atau tidak lengkap".to_string())?;
    let data = dec
        .read_image()
        .map_err(|_| "TIFF rusak atau tidak lengkap".to_string())?;
    let px = w as usize * h as usize;
    let raw: Vec<u8> = match (data, colortype) {
        (DecodingResult::U8(b), tiff::ColorType::Gray(8)) if b.len() == px => {
            b.iter().flat_map(|&g| [g, g, g, 255]).collect()
        }
        (DecodingResult::U8(b), tiff::ColorType::RGB(8)) if b.len() == px * 3 => b
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        (DecodingResult::U8(b), tiff::ColorType::RGBA(8)) if b.len() == px * 4 => b,
        (DecodingResult::U16(b), tiff::ColorType::Gray(16)) if b.len() == px => b
            .iter()
            .flat_map(|&g| {
                let v = (g >> 8) as u8;
                [v, v, v, 255]
            })
            .collect(),
        (DecodingResult::U16(b), tiff::ColorType::RGB(16)) if b.len() == px * 3 => b
            .chunks_exact(3)
            .flat_map(|p| [(p[0] >> 8) as u8, (p[1] >> 8) as u8, (p[2] >> 8) as u8, 255])
            .collect(),
        (DecodingResult::U16(b), tiff::ColorType::RGBA(16)) if b.len() == px * 4 => b
            .chunks_exact(4)
            .flat_map(|p| {
                [(p[0] >> 8) as u8, (p[1] >> 8) as u8, (p[2] >> 8) as u8, (p[3] >> 8) as u8]
            })
            .collect(),
        _ => return Err("Tipe warna TIFF tidak didukung".to_string()),
    };
    RgbaImage::from_raw(w, h, raw).ok_or_else(|| "Dimensi TIFF tidak cocok".to_string())
}

/// Halaman-halaman TIFF multi-page sebagai "animasi" (satu halaman = satu
/// frame). `Ok(None)` = bukan TIFF / 1 halaman / gagal -> biarkan jalur
/// gambar statis yang menangani.
fn decode_tiff_animated(
    bytes: &[u8],
    max_side: u32,
    max_pixels: usize,
) -> Result<Option<Pixels>, String> {
    if image::guess_format(bytes).ok() != Some(ImageFormat::Tiff) {
        return Ok(None);
    }
    let mut dec = tiff::decoder::Decoder::new(Cursor::new(bytes))
        .map_err(|_| "TIFF rusak atau tidak lengkap".to_string())?;

    let mut out: Vec<AnimFrame> = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;
    let mut canvas = [0u32, 0u32];
    loop {
        let buf = match tiff_page_to_rgba(&mut dec) {
            Ok(b) => b,
            Err(_) => {
                if out.is_empty() {
                    return Ok(None); // coba jalur statis
                }
                truncated = true; // pakai halaman yang sudah terbaca
                break;
            }
        };
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
            delay: Duration::from_millis(TIFF_FRAME_DELAY_MS),
        });
        if !dec.more_images() {
            break;
        }
        if dec.next_image().is_err() {
            truncated = true;
            break;
        }
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

/// Animasi GIF / WebP / APNG / TIFF multi-page. `Ok(None)` = bukan animasi
/// (atau hanya 1 frame), biarkan jalur gambar statis yang menangani.
fn decode_animated(bytes: &[u8], max_side: u32, max_pixels: usize) -> Result<Option<Pixels>, String> {
    // TIFF multi-page ditangani terpisah: image::TiffDecoder tidak punya
    // into_frames, jadi iterasi halamannya langsung lewat crate `tiff`.
    if let Some(p) = decode_tiff_animated(bytes, max_side, max_pixels)? {
        return Ok(Some(p));
    }
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
        decode_progressive(bytes, max_side, max_pixels)
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
    fn tiff_multipage_jadi_animasi() {
        use tiff::encoder::{colortype, TiffEncoder};
        let mut out = Cursor::new(Vec::new());
        {
            let mut enc = TiffEncoder::new(&mut out).unwrap();
            for &g in &[50u8, 150u8, 250u8] {
                enc.write_image::<colortype::Gray8>(8, 8, &vec![g; 64]).unwrap();
            }
        }
        let bytes = out.into_inner();
        let p = decode_tiff_animated(&bytes, 16384, 16_000_000)
            .unwrap()
            .expect("3 halaman -> animasi");
        let frames = p.frames.expect("ada frames");
        assert_eq!(frames.len(), 3);
        assert!(
            frames
                .iter()
                .all(|f| f.delay == Duration::from_millis(TIFF_FRAME_DELAY_MS)),
            "semua frame pakai jeda TIFF tetap"
        );
        // Halaman kedua berwarna abu 150 -> RGBA (150,150,150,255).
        let [r, g, b, a] = frames[1].image.pixels[0].to_array();
        assert_eq!((r, g, b, a), (150, 150, 150, 255));
    }

    #[test]
    fn tiff_satu_halaman_bukan_animasi() {
        use tiff::encoder::{colortype, TiffEncoder};
        let mut out = Cursor::new(Vec::new());
        {
            let mut enc = TiffEncoder::new(&mut out).unwrap();
            enc.write_image::<colortype::Gray8>(8, 8, &vec![100u8; 64]).unwrap();
        }
        let bytes = out.into_inner();
        assert!(decode_tiff_animated(&bytes, 16384, 16_000_000).unwrap().is_none());
    }

    #[test]
    fn bukan_tiff_dilewati_decode_tiff() {
        let bytes = png_bytes(8, 8);
        assert!(decode_tiff_animated(&bytes, 16384, 16_000_000).unwrap().is_none());
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
    fn decode_guarded_menolak_png_raksasa_dan_menerima_png_biasa() {
        // Jalur sekunder (thumbnail, batch) memakai decode_guarded: gambar yang
        // mengaku raksasa harus ditolak SEBELUM alokasi, bukan didekode polos.
        let data: &[u8] = include_bytes!("../tests/fixtures/enorme.png");
        assert!(decode_guarded(data).is_err());
        let ok = decode_guarded(&png_bytes(40, 30)).unwrap();
        assert_eq!((ok.width(), ok.height()), (40, 30));
    }

    #[test]
    fn decode_guarded_pesan_galat_spesifik_untuk_batch() {
        // Batch convert meneruskan pesan ini apa adanya ke daftar berkas
        // gagal ("{nama}: {pesan}"), jadi pesannya harus menyebut sebab
        // sebenarnya — bukan "format tidak didukung" yang generik.
        let data: &[u8] = include_bytes!("../tests/fixtures/enorme.png");
        let err = decode_guarded(data).unwrap_err();
        assert!(
            err.contains("terlalu besar"),
            "pesan harus menyebut ukuran berlebih, dapat: {err}"
        );
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
    fn progresif_gambar_besar_langsung_tajam() {
        // >2500px: langsung tajam 2MP, tanpa pratinjau blur.
        let sharp = decode_progressive(&png_bytes(3000, 1500), 16384, 2_000_000).unwrap();
        let spx = sharp.image.size[0] * sharp.image.size[1];
        assert!(spx <= 2_000_000, "tajam {spx}");
        assert_eq!(sharp.orig, [3000, 1500]);
    }

    #[test]
    fn progresif_gambar_kecil_langsung_tajam() {
        // <=2500px: langsung tajam.
        let sharp = decode_progressive(&png_bytes(40, 30), 16384, 2_000_000).unwrap();
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
        // Tanpa pratinjau blur: langsung versi tajam.
        let m1 = out_rx.recv_timeout(Duration::from_secs(60)).unwrap();
        assert!(matches!(m1.outcome, Outcome::Ready(_)), "harus Ready");
        if let Outcome::Ready(d) = m1.outcome {
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

    // ------------------------------------------------ streaming GIF

    #[test]
    fn indeks_gif_menghitung_semua_frame_offset_dan_jeda() {
        let data = gif_bytes(
            &[([255, 0, 0, 255], 50), ([0, 255, 0, 255], 120), ([0, 0, 255, 255], 5)],
            24,
            16,
        );
        let gi = scan_gif_index(&data).expect("GIF 3 frame harus terindeks");
        assert_eq!(gi.canvas, [24, 16]);
        assert_eq!(gi.frames.len(), 3);
        assert!(!gi.truncated);
        assert_eq!(gi.prefix_len, gi.frames[0].offset);
        // Jeda dinormalisasi seperti jalur lama (x10 ms; <20 ms -> 100 ms).
        assert_eq!(gi.frames[0].delay, Duration::from_millis(50));
        assert_eq!(gi.frames[1].delay, Duration::from_millis(120));
        assert_eq!(gi.frames[2].delay, Duration::from_millis(100));
        for (i, f) in gi.frames.iter().enumerate() {
            // Tiap offset menunjuk awal grup blok: ekstensi (0x21) atau
            // deskriptor gambar (0x2C), dan naik tegas.
            assert!(
                matches!(data[f.offset], 0x21 | 0x2C),
                "offset frame {i} menunjuk {:#x}",
                data[f.offset]
            );
            if i > 0 {
                assert!(f.offset > gi.frames[i - 1].offset);
            }
            assert!(f.full_cover, "fixture ini frame penuh semua");
            assert_eq!((f.left, f.top, f.width, f.height), (0, 0, 24, 16));
        }
    }

    #[test]
    fn indeks_gif_menolak_yang_bukan_animasi() {
        // GIF 1 frame -> bukan animasi, jalur statis yang menangani.
        let satu = gif_bytes(&[([10, 20, 30, 255], 100)], 8, 8);
        assert!(scan_gif_index(&satu).is_none());
        // Bukan GIF sama sekali.
        assert!(scan_gif_index(&png_bytes(8, 8)).is_none());
        assert!(scan_gif_index(b"bukan gambar").is_none());
        assert!(scan_gif_index(&[]).is_none());
    }

    #[test]
    fn kadens_checkpoint_tidak_menghabiskan_anggaran() {
        // GIF uji 63 frame @8MB: 5 checkpoint, jarak 13.
        assert_eq!(checkpoint_cadence(63, 8 << 20), 13);
        // Animasi kecil: tiap frame checkpoint pun murah.
        assert_eq!(checkpoint_cadence(3, 1024), 1);
        // Animasi sangat panjang tetap terbatasi jumlah checkpoint-nya.
        let c = checkpoint_cadence(1500, 8 << 20);
        assert!(1500usize.div_ceil(c) <= 8, "cadence {c}");
    }

    /// GIF fixture disposal: A penuh merah (Keep), B sub-rect hijau
    /// (Background), C sub-rect biru (Previous), D penuh dengan indeks
    /// transparan kecuali satu blok hijau (Keep).
    fn gif_disposal_fixture() -> Vec<u8> {
        use std::borrow::Cow;
        let palette: &[u8] = &[255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0];
        let mut out = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut out, 16, 16, palette).unwrap();
            enc.set_repeat(gif::Repeat::Infinite).unwrap();
            let mk = |w: u16,
                      h: u16,
                      left: u16,
                      top: u16,
                      dispose: gif::DisposalMethod,
                      transparent: Option<u8>,
                      px: Vec<u8>| gif::Frame {
                left,
                top,
                width: w,
                height: h,
                delay: 10,
                dispose,
                transparent,
                buffer: Cow::Owned(px),
                ..gif::Frame::default()
            };
            enc.write_frame(&mk(16, 16, 0, 0, gif::DisposalMethod::Keep, None, vec![0u8; 256]))
                .unwrap();
            enc.write_frame(&mk(8, 8, 4, 4, gif::DisposalMethod::Background, None, vec![1u8; 64]))
                .unwrap();
            enc.write_frame(&mk(8, 8, 4, 4, gif::DisposalMethod::Previous, None, vec![2u8; 64]))
                .unwrap();
            let mut d = vec![3u8; 256];
            for y in 8..12 {
                for x in 8..12 {
                    d[y * 16 + x] = 1;
                }
            }
            enc.write_frame(&mk(16, 16, 0, 0, gif::DisposalMethod::Keep, Some(3), d))
                .unwrap();
        }
        out
    }

    #[test]
    fn komposit_streaming_identik_dengan_decoder_lama() {
        let data = gif_disposal_fixture();
        let gi = scan_gif_index(&data).expect("fixture harus terindeks");
        assert_eq!(gi.frames.len(), 4);
        assert_eq!(gi.frames[1].dispose, Dispose::Background);
        assert_eq!(gi.frames[2].dispose, Dispose::Previous);
        assert!(!gi.frames[1].full_cover);
        assert_eq!(gi.frames[3].transparent, Some(3));

        // Jalur lama (crate image) sebagai pembanding kebenaran.
        let legacy = decode_animated(&data, 16384, 16_000_000)
            .unwrap()
            .expect("fixture animasi menurut jalur lama");
        let legacy_frames = legacy.frames.expect("ada frames");
        assert_eq!(legacy_frames.len(), 4);

        let mut player = GifPlayer::open(&data, &gi, 0).unwrap();
        let merah = [255, 0, 0, 255];
        let hijau = [0, 255, 0, 255];
        let biru = [0, 0, 255, 255];
        let tembus = [0, 0, 0, 0];
        for (i, legacy_frame) in legacy_frames.iter().enumerate() {
            let got = player
                .next_frame()
                .expect("masih ada frame")
                .expect("frame terbaca");
            // Seluruh piksel harus sama persis dengan decoder lama.
            for (x, y, px) in got.enumerate_pixels() {
                let lp = legacy_frame.image.pixels[y as usize * 16 + x as usize].to_array();
                assert_eq!(px.0, lp, "frame {i} piksel ({x},{y})");
            }
            // Dan sesuai harapan manual untuk titik-titik kunci.
            let at = |x: u32, y: u32| got.get_pixel(x, y).0;
            match i {
                0 => assert_eq!(at(0, 0), merah),
                1 => {
                    assert_eq!(at(0, 0), merah);
                    assert_eq!(at(6, 6), hijau);
                }
                2 => {
                    assert_eq!(at(0, 0), merah);
                    assert_eq!(at(6, 6), biru);
                }
                3 => {
                    assert_eq!(at(0, 0), merah, "transparan -> kanvas merah");
                    assert_eq!(at(6, 6), tembus, "sesudah Background+Previous: tembus");
                    assert_eq!(at(9, 9), hijau);
                }
                _ => unreachable!(),
            }
        }
        assert!(player.next_frame().is_none(), "aliran habis sesudah 4 frame");
    }

    #[test]
    fn player_dibuka_di_tengah_menghasilkan_frame_yang_benar() {
        // Indeks dua fase: dekoder yang dibuka tepat di offset frame 2
        // (dengan awalan berkas) harus menghasilkan frame yang sama
        // seperti aliran dari awal — di sini untuk frame PENUH sehingga
        // keadaan kanvas sebelumnya tidak berpengaruh.
        let data = gif_bytes(
            &[([255, 0, 0, 255], 50), ([0, 255, 0, 255], 50), ([0, 0, 255, 255], 50)],
            10,
            10,
        );
        let gi = scan_gif_index(&data).unwrap();
        let mut p = GifPlayer::open(&data, &gi, 2).unwrap();
        let f = p.next_frame().unwrap().unwrap();
        assert_eq!(f.get_pixel(5, 5).0, [0, 0, 255, 255], "frame 2 = biru");
        assert!(p.next_frame().is_none());
    }

    /// Tulis GIF animasi ke folder sementara dan muat lewat worker
    /// sungguhan; kumpulkan semua pesan hasilnya.
    fn kumpulkan_pesan_worker(data: &[u8], nama: &str) -> Vec<Loaded> {
        use crate::source::Listing;
        let dir = std::env::temp_dir().join(format!("rv-stream-test-{nama}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{nama}.gif")), data).unwrap();
        let (listing, _) = Listing::open(&dir, true).unwrap();
        let ctx = eframe::egui::Context::default();
        let (main_tx, pref_tx, out_rx) = spawn(ctx).unwrap();
        let listing = std::sync::Arc::new(listing);
        main_tx
            .send(Command::Open {
                session: 7,
                listing: listing.clone(),
            })
            .unwrap();
        pref_tx
            .send(Command::Open {
                session: 7,
                listing,
            })
            .unwrap();
        main_tx
            .send(Command::Load {
                session: 7,
                index: 0,
                primary: true,
                max_side: 16384,
                max_pixels: 2_000_000,
                focus: 0,
                budget: 256 << 20,
                held: 0,
            })
            .unwrap();
        let mut msgs = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let m = out_rx
                .recv_timeout(Duration::from_secs(60))
                .expect("worker harus menjawab");
            let selesai = matches!(
                m.outcome,
                Outcome::AnimDone { .. }
                    | Outcome::AnimFailed { .. }
                    | Outcome::Failed(_)
                    | Outcome::Ready(_)
            );
            msgs.push(m);
            if selesai || std::time::Instant::now() > deadline {
                break;
            }
        }
        std::fs::remove_dir_all(&dir).ok();
        msgs
    }

    /// Jalankan SATU perintah `ExportAnimFrames` lewat worker sungguhan
    /// untuk berkas `data`; kembalikan pesan-pesan hasilnya, folder tujuan
    /// ekspor, dan folder sumber. Keduanya TIDAK dihapus di sini —
    /// pemanggil memeriksa berkas hasil dulu, lalu membersihkan.
    fn ekspor_lewat_worker(
        data: &[u8],
        nama: &str,
    ) -> (Vec<Loaded>, std::path::PathBuf, std::path::PathBuf) {
        use crate::source::Listing;
        let dir = std::env::temp_dir().join(format!("rv-export-test-{nama}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{nama}.gif")), data).unwrap();
        let tujuan = std::env::temp_dir().join(format!("rv-export-test-{nama}-hasil"));
        std::fs::create_dir_all(&tujuan).unwrap();
        let (listing, _) = Listing::open(&dir, true).unwrap();
        let ctx = eframe::egui::Context::default();
        let (main_tx, pref_tx, out_rx) = spawn(ctx).unwrap();
        let listing = std::sync::Arc::new(listing);
        for tx in [&main_tx, &pref_tx] {
            tx.send(Command::Open {
                session: 7,
                listing: listing.clone(),
            })
            .unwrap();
        }
        main_tx
            .send(Command::ExportAnimFrames {
                session: 7,
                index: 0,
                dir: tujuan.clone(),
                stem: nama.to_string(),
                max_side: 16384,
                max_pixels: 2_000_000,
            })
            .unwrap();
        let mut msgs = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let m = out_rx
                .recv_timeout(Duration::from_secs(60))
                .expect("worker harus menjawab perintah ekspor");
            let selesai = matches!(
                m.outcome,
                Outcome::AnimExported { .. } | Outcome::AnimJobFailed { .. }
            );
            msgs.push(m);
            if selesai || std::time::Instant::now() > deadline {
                break;
            }
        }
        (msgs, tujuan, dir)
    }

    #[test]
    fn ekspor_gif_menghasilkan_semua_frame_sebagai_png() {
        // Regresi vonis QA putaran streaming: "Ekspor semua frame" harus
        // benar-benar menulis N berkas PNG untuk GIF N frame, dan worker
        // melaporkannya sebagai AnimExported{done: N, total: N}.
        let data = gif_bytes(
            &[
                ([255, 0, 0, 255], 50),
                ([0, 255, 0, 255], 50),
                ([0, 0, 255, 255], 50),
            ],
            24,
            16,
        );
        let (msgs, tujuan, dir) = ekspor_lewat_worker(&data, "ekspor-ok");
        let hasil = msgs.iter().find_map(|m| match &m.outcome {
            Outcome::AnimExported { done, total, .. } => Some((*done, *total)),
            _ => None,
        });
        assert_eq!(
            hasil,
            Some((3, 3)),
            "ekspor harus melaporkan 3/3: {:?}",
            msgs.iter().map(|m| outcome_name(&m.outcome)).collect::<Vec<_>>()
        );
        for (i, warna) in [[255, 0, 0], [0, 255, 0], [0, 0, 255]].iter().enumerate() {
            let p = tujuan.join(format!("ekspor-ok_frame{:03}.png", i + 1));
            let bytes = std::fs::read(&p)
                .unwrap_or_else(|e| panic!("frame {} harus tertulis: {e}", i + 1));
            assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "frame {} harus PNG sah", i + 1);
            let img = image::load_from_memory(&bytes).unwrap().to_rgba8();
            assert_eq!(img.dimensions(), (24, 16));
            let px = img.get_pixel(12, 8).0;
            assert_eq!(
                [px[0], px[1], px[2]],
                *warna,
                "isi frame {} harus sesuai urutan GIF",
                i + 1
            );
        }
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&tujuan).ok();
    }

    #[test]
    fn ekspor_bukan_gif_melaporkan_gagal_bukan_diam() {
        // Pasangan regresi: sumber BUKAN GIF animasi -> worker WAJIB
        // menjawab AnimJobFailed (menjadi toast galat di UI); kegagalan
        // ekspor tidak boleh hilang diam-diam, dan tidak boleh ada
        // berkas hasil. PNG statis ditulis dengan nama .gif: yang
        // menentukan adalah indeks GIF dari isinya, bukan ekstensi.
        let mut data = Vec::new();
        {
            let img = RgbaImage::from_pixel(24, 16, Rgba([9, 9, 9, 255]));
            let enc = image::codecs::png::PngEncoder::new(&mut data);
            image::ImageEncoder::write_image(
                enc,
                img.as_raw(),
                24,
                16,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        }
        let (msgs, tujuan, dir) = ekspor_lewat_worker(&data, "ekspor-gagal");
        assert!(
            msgs.iter()
                .any(|m| matches!(m.outcome, Outcome::AnimJobFailed { .. })),
            "kegagalan ekspor harus dilaporkan: {:?}",
            msgs.iter().map(|m| outcome_name(&m.outcome)).collect::<Vec<_>>()
        );
        assert!(
            std::fs::read_dir(&tujuan).unwrap().next().is_none(),
            "tidak boleh ada berkas hasil untuk ekspor yang gagal"
        );
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&tujuan).ok();
    }

    /// GIF fixture interior transparan (pola GIF teroptimasi asli):
    /// frame penuh merah, lalu frame rect-penuh yang sebagian besar
    /// indeks transparan dengan satu blok warna — piksel transparan di
    /// DALAM rect berarti "tidak berubah".
    fn gif_interior_transparan_fixture() -> Vec<u8> {
        use std::borrow::Cow;
        let palette: &[u8] = &[255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0];
        let mut out = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut out, 32, 32, palette).unwrap();
            enc.set_repeat(gif::Repeat::Infinite).unwrap();
            let mk = |transparent: Option<u8>, px: Vec<u8>| gif::Frame {
                left: 0,
                top: 0,
                width: 32,
                height: 32,
                delay: 10,
                dispose: gif::DisposalMethod::Keep,
                transparent,
                buffer: Cow::Owned(px),
                ..gif::Frame::default()
            };
            enc.write_frame(&mk(None, vec![0u8; 1024])).unwrap();
            let mut b = vec![3u8; 1024];
            for y in 12..20 {
                for x in 12..20 {
                    b[y * 32 + x] = 1; // blok hijau di tengah
                }
            }
            enc.write_frame(&mk(Some(3), b)).unwrap();
            let mut c = vec![3u8; 1024];
            for y in 2..8 {
                for x in 24..30 {
                    c[y * 32 + x] = 2; // blok biru di pojok kanan atas
                }
            }
            enc.write_frame(&mk(Some(3), c)).unwrap();
        }
        out
    }

    #[test]
    fn stream_lanjutan_rect_interior_transparan_tidak_rusak() {
        // Regresi uji GUI putaran ini: stream lanjutan yang mengomposit
        // rect yang sudah di-shrink merusak GIF interior-transparan
        // (resample mencampur alfa 0/255 menjadi semi-transparan gelap).
        // Jalur yang benar mengomposit di resolusi penuh lalu shrink.
        use crate::source::Listing;
        let data = gif_interior_transparan_fixture();
        let dir = std::env::temp_dir().join("rv-stream-test-interior");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("interior.gif"), &data).unwrap();
        let (listing, _) = Listing::open(&dir, true).unwrap();
        let ctx = eframe::egui::Context::default();
        let (main_tx, _pref_tx, out_rx) = spawn(ctx).unwrap();
        main_tx
            .send(Command::Open {
                session: 7,
                listing: std::sync::Arc::new(listing),
            })
            .unwrap();
        // Tier dipaksa 16x16 (dari kanvas 32x32) supaya resample aktif.
        main_tx
            .send(Command::AnimStream {
                session: 7,
                index: 0,
                start_frame: 0,
                base: None,
                max_side: 16384,
                max_pixels: 256,
            })
            .unwrap();
        let mut frames = Vec::new();
        loop {
            let m = out_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("worker harus menjawab");
            match m.outcome {
                Outcome::AnimChunk {
                    frames: fs,
                    start,
                    ..
                } => {
                    for (k, f) in fs.into_iter().enumerate() {
                        frames.push((start + k, f));
                    }
                }
                Outcome::AnimDone { .. } => break,
                _other => panic!("hasil tak terduga selain AnimChunk/AnimDone"),
            }
        }
        std::fs::remove_dir_all(&dir).ok();
        frames.sort_by_key(|(i, _)| *i);
        assert_eq!(frames.len(), 3);
        let px = |f: &AnimFrame, x: usize, y: usize| {
            f.image.pixels[y * f.image.width() + x].to_array()
        };
        // Frame 1: pojok (daerah interior transparan) tetap merah opak
        // dari kanvas, tengah hijau.
        let f1 = &frames[1].1;
        assert_eq!(f1.image.size, [16, 16]);
        assert_eq!(px(f1, 1, 1), [255, 0, 0, 255], "pojok frame 1 harus merah opak");
        assert_eq!(px(f1, 8, 8), [0, 255, 0, 255], "tengah frame 1 harus hijau");
        // Frame 2: blok biru muncul, hijau dan merah tetap utuh.
        let f2 = &frames[2].1;
        assert_eq!(px(f2, 13, 2), [0, 0, 255, 255], "blok frame 2 harus biru");
        assert_eq!(px(f2, 8, 8), [0, 255, 0, 255], "hijau frame 1 tetap ada");
        assert_eq!(px(f2, 1, 14), [255, 0, 0, 255], "pojok bawah tetap merah opak");
    }

    #[test]
    fn pekerja_mengirim_preview_potongan_lalu_selesai() {
        let data = gif_bytes(
            &[
                ([255, 0, 0, 255], 50),
                ([0, 255, 0, 255], 120),
                ([0, 0, 255, 255], 80),
                ([255, 255, 0, 255], 90),
                ([0, 255, 255, 255], 60),
            ],
            24,
            16,
        );
        let msgs = kumpulkan_pesan_worker(&data, "urutan");
        // Pesan pertama: Preview dengan metadata indeks penuh.
        let Outcome::Preview(d, meta) = &msgs[0].outcome else {
            panic!("pesan pertama harus Preview");
        };
        assert_eq!(meta.total, 5);
        assert_eq!(
            meta.delays,
            vec![
                Duration::from_millis(50),
                Duration::from_millis(120),
                Duration::from_millis(80),
                Duration::from_millis(90),
                Duration::from_millis(60),
            ]
        );
        assert_eq!(d.pixels.orig, [24, 16]);
        assert_eq!(d.pixels.frames.as_ref().unwrap().len(), 1, "preview = frame 0 saja");
        assert!(d.pixels.format.contains("5 frame"), "{}", d.pixels.format);
        // Potongan-potongan menutupi frame 1..5 tepat sekali, berurutan.
        let mut tertutup: Vec<usize> = Vec::new();
        let mut checkpoints = Vec::new();
        for m in &msgs[1..] {
            match &m.outcome {
                Outcome::AnimChunk {
                    start,
                    frames,
                    checkpoints: cps,
                } => {
                    for k in 0..frames.len() {
                        tertutup.push(start + k);
                    }
                    checkpoints.extend(cps.iter().map(|(i, _)| *i));
                }
                Outcome::AnimDone { total } => assert_eq!(*total, 5),
                o => panic!("pesan tak terduga: {}", outcome_name(o)),
            }
        }
        assert_eq!(tertutup, vec![1, 2, 3, 4]);
        assert!(checkpoints.contains(&0), "checkpoint frame 0 harus ada");
        assert!(
            matches!(msgs.last().unwrap().outcome, Outcome::AnimDone { .. }),
            "pesan terakhir harus AnimDone"
        );
    }

    #[test]
    fn pekerja_mengirim_preview_lalu_gagal_untuk_data_frame_rusak() {
        // GIF 2 frame yang sehat, lalu data LZW frame kedua diobok-obok
        // (strukturnya utuh, isinya sampah): indeks tetap terbaca, frame
        // pertama tampil, lalu stream harus gagal secara jujur di frame 1.
        let mut data = gif_bytes(
            &[([255, 0, 0, 255], 50), ([0, 255, 0, 255], 50)],
            16,
            16,
        );
        let offsets = gif_block_offsets(&data).unwrap();
        assert_eq!(offsets.len(), 2);
        // Lompati GCE + deskriptor frame kedua menuju sub-blok datanya.
        let mut pos = offsets[1];
        if data[pos] == 0x21 {
            // Ekstensi (GCE): label + sub-blok.
            pos += 2;
            loop {
                let n = data[pos] as usize;
                pos += 1 + n;
                if n == 0 {
                    break;
                }
            }
        }
        assert_eq!(data[pos], 0x2C, "harus tiba di deskriptor gambar");
        let desc_packed = data[pos + 9];
        pos += 10; // deskriptor
        if desc_packed & 0x80 != 0 {
            // Palet lokal ikut ditulis encoder untuk frame ini.
            pos += 3usize << ((desc_packed & 0x07) + 1);
        }
        pos += 1; // LZW minimum code size
        let n = data[pos] as usize;
        assert!(n > 4, "sub-blok data harus cukup besar untuk dirusak");
        for b in &mut data[pos + 1..pos + 1 + n] {
            *b = 0xFF;
        }
        let msgs = kumpulkan_pesan_worker(&data, "rusak");
        assert!(
            matches!(msgs[0].outcome, Outcome::Preview(..)),
            "frame pertama tetap tampil lebih dulu"
        );
        let gagal = msgs.iter().any(|m| {
            matches!(m.outcome, Outcome::AnimFailed { from_frame: 1, .. })
        });
        assert!(gagal, "stream harus gagal jujur di frame 1: {:?}", msgs.iter().map(|m| outcome_name(&m.outcome)).collect::<Vec<_>>());
    }

    fn outcome_name(o: &Outcome) -> &'static str {
        match o {
            Outcome::Ready(_) => "Ready",
            Outcome::Preview(..) => "Preview",
            Outcome::AnimChunk { .. } => "AnimChunk",
            Outcome::AnimDone { .. } => "AnimDone",
            Outcome::AnimFailed { .. } => "AnimFailed",
            Outcome::AnimExported { .. } => "AnimExported",
            Outcome::AnimSaved { .. } => "AnimSaved",
            Outcome::AnimJobFailed { .. } => "AnimJobFailed",
            Outcome::Failed(_) => "Failed",
            Outcome::Skipped => "Skipped",
        }
    }
}




