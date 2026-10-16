//! Antarmuka Roneyview (egui/eframe): navigasi, zoom, geser, rotasi, layar penuh.

use std::collections::HashMap;
use std::error::Error;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, ColorImage, Context, FontId, Key, Modifiers, PointerButton, Pos2,
    Rect, Sense, TextureHandle, TextureOptions, Vec2, pos2, vec2,
};
use eframe::glow::HasContext;
use image::{ImageDecoder, RgbaImage};

use crate::edit::{self, ToneParams};

use crate::dialogs::{self, Modal, ModalResult, PropsDialog, WallpaperDialog};
use crate::fileinfo::{self, PageFacts};
use crate::i18n::{Lang, tr, tr_fmt, unescape};
use crate::loader::{self, AnimFrame, Command, Loaded, Outcome};
use crate::positions::{PositionStore, key_for};
use crate::settings::{FitMode, Store};
use crate::source::Listing;

const BG: Color32 = Color32::from_gray(22);
/// Gambar yang disimpan di cache: sekian di belakang dan di depan gambar aktif.
const KEEP_BEHIND: usize = 2;
const KEEP_AHEAD: usize = 3;

/// Rentang indeks halaman yang boleh tetap ada di RAM. Mode hemat memori hanya
/// menyimpan halaman yang sedang dilihat (tanpa prefetch).
fn keep_window(index: usize, low_memory: bool, spread: bool) -> (usize, usize) {
    let extra = usize::from(spread);
    if low_memory {
        (index, index + extra)
    } else {
        (index.saturating_sub(KEEP_BEHIND + extra), index + KEEP_AHEAD + 2 * extra)
    }
}

/// Awal "spread" (halaman yang ditampilkan pertama) untuk halaman `t` di mode dua halaman.
/// Dengan sampul tunggal: spread dimulai di 0, 1, 3, 5, ...; tanpa: 0, 2, 4, ...
fn snap_start_for(t: usize, two_page: bool, cover_alone: bool) -> usize {
    if !two_page {
        t
    } else if cover_alone {
        if t == 0 || t % 2 == 1 {
            t
        } else {
            t - 1
        }
    } else {
        t - t % 2
    }
}

/// Awal spread sebelum `s`. Halaman dengan dimensi belum diketahui dianggap potret.
/// Pasangan (s-2, s-1) hanya dibentuk bila keduanya bukan halaman lebar.
fn prev_start_for(s: usize, two_page: bool, cover_alone: bool, wide: &dyn Fn(usize) -> bool) -> Option<usize> {
    if s == 0 {
        return None;
    }
    if !two_page {
        return Some(s - 1);
    }
    let min_pair_start = usize::from(cover_alone);
    if s >= 2 && s - 2 >= min_pair_start && !wide(s - 2) && !wide(s - 1) {
        Some(s - 2)
    } else {
        Some(s - 1)
    }
}

/// Lebar tiap halaman setelah tinggi disamakan ke yang tertinggi (dalam satuan yang
/// sama dengan masukan), beserta tinggi bersama. Dipakai untuk menata dua halaman.
fn pair_widths(dims: &[(f32, f32)]) -> (Vec<f32>, f32) {
    let h = dims.iter().map(|d| d.1).fold(0.0_f32, f32::max).max(1.0);
    let w = dims.iter().map(|d| d.0 * h / d.1.max(1.0)).collect();
    (w, h)
}
const MIN_SCALE: f32 = 0.02;
// Jumlah langkah zoom-in (1.25x) maksimal dari ukuran fit.
const MAX_ZOOM_STEPS: i32 = 8;

/// Tier resolusi decode mengikuti level zoom (skala absolut, 1.0 = ukuran asli):
/// tampilan fit/pas-pasan cukup ~2MP (ringan di CPU/GPU lemah), zoom >= 7x naik
/// ke ~4MP supaya tetap tajam. Pemanggil me-min-kannya dengan max_pixels().
fn tier_pixels(scale: f32) -> usize {
    if scale >= 7.0 {
        4_000_000
    } else {
        2_000_000
    }
}
const ZOOM_STEP: f32 = 1.25;
const PAN_STEP: f32 = 90.0;
/// Roda mouse: satu "notch" dihitung sebesar ini (poin).
const WHEEL_LINE_POINTS: f32 = 60.0;
const FLIP_THRESHOLD: f32 = 40.0;
/// Jumlah folder/arsip yang disimpan di "Folder terkini".
const MAX_RECENT: usize = 20;

/// Ubah masukan teks dialog "Lompat ke halaman" menjadi indeks 0-based.
/// Mengembalikan `None` bila bukan angka 1..=n.
fn parse_page_number(input: &str, n: usize) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let v: usize = input.trim().parse().ok()?;
    if v == 0 || v > n {
        return None;
    }
    Some(v - 1)
}

/// Catat folder/arsip ke daftar "terkini": paling baru di depan, tanpa
/// duplikat, dibatasi `MAX_RECENT`.
fn record_recent(recent: &mut Vec<String>, path: &str) {
    if path.is_empty() {
        return;
    }
    recent.retain(|p| p != path);
    recent.insert(0, path.to_string());
    recent.truncate(MAX_RECENT);
}

/// Indeks frame sesudah melangkah `dir` (+1 maju, -1 mundur), melingkar.
fn step_index(cur: usize, n: usize, dir: i32) -> usize {
    if n == 0 {
        return 0;
    }
    (cur as i32 + dir).rem_euclid(n as i32) as usize
}

/// Ubah `ColorImage` (egui) menjadi `RgbaImage` (crate image) untuk disimpan.
/// `ColorImage` menyimpan alpha premultiplied; PNG butuh straight alpha,
/// jadi di sini dikembalikan dulu (piksel opak/transparan penuh tidak berubah).
fn colorimage_to_rgba(img: &ColorImage) -> RgbaImage {
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

/// Animasi (GIF/WebP/APNG/TIFF): semua frame disimpan di sisi CPU dan diunggah
/// satu per satu ke SATU tekstur, jadi memori GPU tidak berlipat ganda.
struct Anim {
    frames: Vec<AnimFrame>,
    /// Indeks frame yang saat ini ada di tekstur.
    uploaded: AtomicUsize,
    truncated: bool,
}

struct Page {
    tex: TextureHandle,
    orig: [u32; 2],
    /// Dimensi hasil decode (resolusi tekstur). Bisa lebih kecil dari `orig`
    /// karena tier resolusi mengikuti zoom (fit ~2MP, zoom >= 7x ~4MP).
    decoded: [u32; 2],
    file_size: u64,
    bytes: usize,
    anim: Option<Anim>,
    format: String,
}

/// Isi yang sedang tampil: satu halaman, atau dua halaman berdampingan (urutan baca).
struct Shown {
    start: usize,
    slots: Vec<(usize, Arc<Page>)>,
}

/// Pilihan di menu klik kanan.
#[derive(Clone, Copy)]
enum CtxChoice {
    Wallpaper,
    Properties,
    OpenFolder,
    Trash,
    Extract,
    SaveFrame,
    ExportFrames,
}

#[derive(Clone, Copy, PartialEq)]
enum Zoom {
    Fit(FitMode),
    Custom(f32),
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Next,
    Prev,
    First,
    Last,
    OpenFile,
    OpenFolder,
    Fit(FitMode),
    ZoomIn,
    ZoomOut,
    RotateCw,
    RotateCcw,
    PanY(f32),
    ToggleFullscreen,
    ExitFullscreen,
    TogglePlay,
    ToggleBarLock,
    ToggleLowMemory,
    ToggleTwoPage,
    ToggleRtl,
    ToggleCover,
    GoToPage,
    ExtractPage,
    Trash,
    OpenRecent(usize),
    FramePrev,
    FrameNext,
    ToggleToolbar,
    Help,
    About,
    Settings,
    Quit,
}

#[derive(Clone)]
enum DialogKind {
    File,
    Folder,
    SaveImage { index: usize, file_name: String },
    SaveFrame { index: usize, frame: usize, file_name: String },
    ExportFrames { index: usize },
    SaveEdit { file_name: String },
    SavePdf { file_name: String },
}

/// Perkakas edit di toolbar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditTool {
    Crop,
    Resize,
    Rotate,
    Picker,
    Tone,
}

/// Hasil hit-test pointer terhadap seleksi crop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CropHit {
    Handle(CropHandle),
    Inside,
    Outside,
}

/// Pegangan (handle) seleksi crop: 4 sudut + 4 sisi.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CropHandle {
    LeftTop,
    RightTop,
    RightBottom,
    LeftBottom,
    Left,
    Right,
    Top,
    Bottom,
}

/// Mode seret pada overlay crop.
#[derive(Clone, Copy, Debug)]
enum CropDrag {
    /// Buat seleksi baru dari titik awal.
    New { start: (f32, f32) },
    /// Geser seleksi: `rect` saat seret mulai, `grab` titik tekan (koordinat gambar).
    Move { rect: [u32; 4], grab: (f32, f32) },
    /// Ubah ukuran dari pegangan: `rect` saat seret mulai.
    Resize { rect: [u32; 4], handle: CropHandle },
}

/// Satu sesi edit gambar: buffer kerja + state tiap perkakas.
/// Buffer selalu RGBA8 straight-alpha pada resolusi penuh sumber.
struct EditSession {
    name: String,
    buffer: RgbaImage,
    original: RgbaImage,
    tool: EditTool,
    /// Seleksi crop [x, y, w, h] dalam koordinat gambar.
    crop_rect: Option<[u32; 4]>,
    /// Mode seret crop yang sedang berlangsung.
    crop_drag: Option<CropDrag>,
    /// Rasio aspek crop: 0 bebas, 1 = 1:1, 2 = 4:3, 3 = 3:2, 4 = 16:9,
    /// 5 = 2:3, 6 = 3:4, 7 = 9:16.
    crop_aspect: usize,
    /// Resize: dimensi target (piksel) atau persen.
    resize_w: u32,
    resize_h: u32,
    resize_pct: u32,
    resize_by_pct: bool,
    resize_lock: bool,
    resize_filter: usize,
    /// Color picker: piksel terkunci (klik) dan yang di-hover.
    pick: Option<(u32, u32)>,
    pick_hover: Option<(u32, u32)>,
    /// Tone mapping: parameter + pratinjau + buffer HDR linier (bila sumber .hdr).
    tone: ToneParams,
    tone_float: Option<Vec<f32>>,
    tone_preview: Option<RgbaImage>,
    /// Sumber kecil yang di-cache untuk pratinjau cepat saat slider digeser.
    /// Dibangun sekali dari buffer; dibuang saat buffer berubah.
    tone_draft_src: Option<RgbaImage>,
    tone_draft_float: Option<(Vec<f32>, u32, u32)>,
    /// Ada perubahan yang belum disimpan.
    dirty: bool,
}

impl EditSession {
    fn new(name: String, buffer: RgbaImage, float: Option<Vec<f32>>, tool: EditTool) -> Self {
        let (w, h) = (buffer.width(), buffer.height());
        EditSession {
            name,
            buffer: buffer.clone(),
            original: buffer,
            tool,
            crop_rect: None,
            crop_drag: None,
            crop_aspect: 0,
            resize_w: w,
            resize_h: h,
            resize_pct: 100,
            resize_by_pct: false,
            resize_lock: true,
            resize_filter: 2,
            pick: None,
            pick_hover: None,
            tone: ToneParams::default(),
            tone_float: float,
            tone_preview: None,
            tone_draft_src: None,
            tone_draft_float: None,
            dirty: false,
        }
    }

    /// Gambar yang ditampilkan di tampilan edit: pratinjau tone bila ada.
    fn display_image(&self) -> &RgbaImage {
        self.tone_preview.as_ref().unwrap_or(&self.buffer)
    }
}

/// Tujuan pekerjaan bangun PDF: simpan ke berkas atau kirim ke printer.
enum PdfJobKind {
    Save(PathBuf),
    Print,
}

/// Aksi dari panel edit (dikumpulkan saat menggambar panel, dieksekusi sesudahnya).
enum EditPanelAction {
    ApplyCrop,
    ClearCrop,
    CopyCrop,
    ApplyResize,
    RotateCw,
    RotateCcw,
    Rotate180,
    ApplyTone,
    ResetTone,
    ResetAll,
    Save,
    Cancel,
    CopyHex(String),
}

/// Rasio aspek crop untuk indeks pilihan.
fn crop_ratio(idx: usize) -> Option<f32> {
    match idx {
        1 => Some(1.0),
        2 => Some(4.0 / 3.0),
        3 => Some(3.0 / 2.0),
        4 => Some(16.0 / 9.0),
        5 => Some(2.0 / 3.0),
        6 => Some(3.0 / 4.0),
        7 => Some(9.0 / 16.0),
        _ => None,
    }
}

/// Metode resampling resize untuk indeks pilihan.
fn resize_filter(idx: usize) -> image::imageops::FilterType {
    use image::imageops::FilterType;
    match idx {
        0 => FilterType::Nearest,
        1 => FilterType::Triangle,
        3 => FilterType::Lanczos3,
        4 => FilterType::Gaussian,
        _ => FilterType::CatmullRom,
    }
}

#[derive(Default)]
struct FrameInput {
    actions: Vec<Action>,
    dropped: Vec<PathBuf>,
    hovering_files: bool,
    wheel: f32,
    zoom: f32,
}

pub struct RoneyApp {
    store: Store,
    store_dirty: bool,
    last_save: Instant,
    /// Posisi baca terakhir per folder/arsip.
    positions: PositionStore,
    positions_dirty: bool,
    /// Kunci posisi untuk daftar yang sedang dibuka.
    position_key: Option<String>,

    listing: Option<Arc<Listing>>,
    session: u64,
    index: usize,
    dir: i8,

    cache: HashMap<usize, Arc<Page>>,
    /// Batas total ukuran tekstur di cache (byte). Halaman terjauh dibuang dulu.
    budget: usize,
    low_mem_notified: bool,
    /// Mode hemat memori (pilihan pengguna, disimpan antar sesi).
    low_memory_mode: bool,

    /// Indeks yang sedang digeser di slider (belum dibuka); dibuka saat dilepas.
    scrub: Option<usize>,
    bar_locked: bool,
    /// Gambar sedang digeser dengan mouse: sembunyikan overlay navigasi.
    panning: bool,
    /// Mode dua halaman, arah baca kanan-ke-kiri, dan sampul tunggal (preferensi).
    two_page_mode: bool,
    rtl: bool,
    cover_alone: bool,
    /// Dimensi asli tiap halaman yang pernah didekode (untuk mengenali halaman lebar).
    dims: HashMap<usize, [u32; 2]>,
    /// Halaman yang diklik kanan.
    ctx_target: usize,

    modal: Option<Modal>,
    props: Option<PropsDialog>,
    wallpaper: Option<WallpaperDialog>,
    /// Pesan dari thread latar belakang (mis. gagal membuka folder).
    note_tx: Sender<String>,
    note_rx: Receiver<String>,
    pending: HashMap<usize, bool>,
    errors: HashMap<usize, String>,
    shown: Option<Arc<Shown>>,
    placed_for: Option<(u64, usize, usize)>,

    /// Perintah ke worker utama (permintaan utama).
    cmd_tx_main: Sender<Command>,
    /// Perintah ke worker prefetch.
    cmd_tx_pref: Sender<Command>,
    out_rx: Receiver<Loaded>,

    #[cfg(feature = "dialogs")]
    dialog_tx: Sender<Option<PathBuf>>,
    #[cfg(feature = "dialogs")]
    dialog_rx: Receiver<Option<PathBuf>>,
    #[cfg(feature = "dialogs")]
    dialog_busy: bool,
    /// Maksud dialog berkas yang sedang berjalan (buka vs simpan).
    #[cfg(feature = "dialogs")]
    dialog_kind: Option<DialogKind>,

    fit: FitMode,
    zoom: Zoom,
    rotation: u8,
    offset: Vec2,
    land_bottom: bool,
    view_rect: Rect,
    display_scale: f32,
    lang: Lang,
    show_settings: bool,
    flip_accum: f32,
    flip_block_until: Instant,

    anim_key: Option<(u64, usize)>,
    anim_frame: usize,
    anim_next: Instant,
    anim_playing: bool,

    fullscreen: bool,
    show_help: bool,
    /// Jendela About sedang terbuka.
    about_open: bool,
    mipmaps: bool,
    notice: Option<(String, Instant)>,
    title: String,
    /// Jendela "Lompat ke halaman" sedang terbuka.
    goto_open: bool,
    goto_input: String,
    goto_focus: bool,

    /// Sesi edit gambar (crop/resize/picker/tone); None bila tidak mengedit.
    edit: Option<EditSession>,
    /// Tekstur untuk buffer edit (diunggah ulang saat buffer berubah).
    edit_tex: Option<TextureHandle>,
    edit_tex_dirty: bool,
    /// Persegi gambar di tampilan edit (pemetaan koordinat mouse).
    edit_img_rect: Rect,
    edit_scale: f32,
    /// Jendela Cetak / Simpan PDF.
    print_open: bool,
    print_layout: usize,
    print_paper: usize,
    print_landscape: bool,
    print_page: usize,
    print_thumbs: HashMap<usize, TextureHandle>,
    print_status: String,
    /// Pekerjaan bangun PDF di thread latar (channel hasil + tujuan).
    print_job: Option<(Receiver<Result<Vec<u8>, String>>, PdfJobKind)>,
    /// Proyektor (jendela kedua).
    projector_open: bool,
    projector_tex: Option<TextureHandle>,
    projector_key: Option<(u64, usize, usize)>,
    /// Corat-coret sementara di proyektor (tidak disimpan ke berkas).
    projector_strokes: Vec<ProjStroke>,
    projector_pen_color: Color32,
    projector_pen_width: f32,
    projector_eraser: bool,
}

/// Satu goresan pena di proyektor; titik dalam koordinat piksel gambar.
struct ProjStroke {
    color: Color32,
    width: f32,
    pts: Vec<(f32, f32)>,
}

/// Skala tampilan untuk satu mode muat. `img` dan `view` dalam poin.
fn fit_scale(mode: FitMode, img: Vec2, view: Vec2) -> f32 {
    let w = img.x.max(1.0);
    let h = img.y.max(1.0);
    let vw = view.x.max(1.0);
    let vh = view.y.max(1.0);
    match mode {
        FitMode::Fit => (vw / w).min(vh / h).min(1.0),
        FitMode::FitUpscale => (vw / w).min(vh / h),
        FitMode::Width => vw / w,
        FitMode::Height => vh / h,
        FitMode::Original => 1.0,
    }
}

fn clamp_offset(off: Vec2, size: Vec2, view: Vec2) -> Vec2 {
    let mx = ((size.x - view.x) / 2.0).max(0.0);
    let my = ((size.y - view.y) / 2.0).max(0.0);
    vec2(off.x.clamp(-mx, mx), off.y.clamp(-my, my))
}

struct Geo {
    scale: f32,
    size: Vec2,
    offset: Vec2,
}

impl RoneyApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        arg: Option<PathBuf>,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let ctx = cc.egui_ctx.clone();
        ctx.set_theme(egui::Theme::Dark);
        ctx.style_mut(|s| {
            s.visuals.panel_fill = Color32::from_gray(30);
            s.visuals.window_corner_radius = 6.0.into();
        });

        // Mipmap membuat gambar besar yang dikecilkan tampak halus, tetapi
        // butuh OpenGL >= 3.0 (atau GLES >= 2.0); pada GPU lawas dimatikan.
        let mipmaps = cc.gl.as_ref().is_some_and(|gl| {
            let v = gl.version();
            v.major >= 3 || (v.is_embedded && v.major >= 2)
        });

        let (cmd_tx_main, cmd_tx_pref, out_rx) = loader::spawn(ctx.clone())?;
        let (note_tx, note_rx) = mpsc::channel::<String>();
        #[cfg(feature = "dialogs")]
        let (dialog_tx, dialog_rx) = mpsc::channel();

        let store = Store::load();
        let fit = store.state.fit;
        let lang = Lang::from_code(&store.state.lang);
        let bar_locked = store.state.bar_locked;
        let two_page_mode = store.state.two_page;
        let rtl = store.state.rtl;
        let cover_alone = store.state.cover_alone;
        let low_memory_mode = store.state.low_memory;

        let mut app = RoneyApp {
            store,
            store_dirty: false,
            last_save: Instant::now(),
            positions: PositionStore::load(),
            positions_dirty: false,
            position_key: None,
            listing: None,
            session: 0,
            index: 0,
            dir: 1,
            cache: HashMap::new(),
            budget: loader::cache_budget(),
            low_mem_notified: false,
            low_memory_mode,
            scrub: None,
            bar_locked,
            panning: false,
            two_page_mode,
            rtl,
            cover_alone,
            dims: HashMap::new(),
            ctx_target: 0,
            modal: None,
            props: None,
            wallpaper: None,
            note_tx,
            note_rx,
            pending: HashMap::new(),
            errors: HashMap::new(),
            shown: None,
            placed_for: None,
            cmd_tx_main,
            cmd_tx_pref,
            out_rx,
            #[cfg(feature = "dialogs")]
            dialog_tx,
            #[cfg(feature = "dialogs")]
            dialog_rx,
            #[cfg(feature = "dialogs")]
            dialog_busy: false,
            #[cfg(feature = "dialogs")]
            dialog_kind: None,
            fit,
            zoom: Zoom::Fit(fit),
            rotation: 0,
            offset: Vec2::ZERO,
            land_bottom: false,
            view_rect: Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0)),
            display_scale: 1.0,
            lang,
            show_settings: false,
            flip_accum: 0.0,
            flip_block_until: Instant::now(),
            anim_key: None,
            anim_frame: 0,
            anim_next: Instant::now(),
            anim_playing: true,
            fullscreen: false,
            show_help: false,
            about_open: false,
            mipmaps,
            notice: None,
            title: String::new(),
            goto_open: false,
            goto_input: String::new(),
            goto_focus: false,
            edit: None,
            edit_tex: None,
            edit_tex_dirty: false,
            edit_img_rect: Rect::NOTHING,
            edit_scale: 1.0,
            print_open: false,
            print_layout: 0,
            print_paper: 0,
            print_landscape: false,
            print_page: 0,
            print_thumbs: HashMap::new(),
            print_status: String::new(),
            print_job: None,
            projector_open: false,
            projector_tex: None,
            projector_key: None,
            projector_strokes: Vec::new(),
            projector_pen_color: Color32::from_rgb(255, 60, 60),
            projector_pen_width: 6.0,
            projector_eraser: false,
        };

        if app.store.had_legacy_history {
            app.store_dirty = true; // tulis ulang berkas pengaturan tanpa riwayat lama
        }
        if let Some(p) = arg {
            app.open_path(&ctx, &p);
        }
        Ok(app)
    }

    // ---------------------------------------------------------------- membuka

    fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now() + Duration::from_secs(4)));
    }

    fn open_path(&mut self, ctx: &Context, path: &Path) {
        let scan = self.store.state.scan_subfolders;
        let (listing, start) = match Listing::open(path, scan) {
            Ok(v) => v,
            Err(e) => {
                self.notify(e);
                return;
            }
        };
        self.install_listing(ctx, listing, start);
    }

    /// Pasang daftar gambar baru dan tampilkan gambar yang diminta.
    /// `start`: indeks eksplisit (mis. berkas yang diklik dua kali atau halaman
    /// sesudah trash); `None` berarti folder/arsip dibuka langsung -> pulihkan
    /// posisi baca terakhir bila ada.
    fn install_listing(&mut self, ctx: &Context, listing: Listing, start: Option<usize>) {
        let lang = self.lang;
        let last = listing.len().saturating_sub(1);
        let mut index = start.unwrap_or(0).min(last);
        if start.is_none() {
            if let Some(pos) = self.positions.get(&key_for(&listing.origin)) {
                index = self.snap_start(pos.min(last));
            }
        }
        self.session += 1;
        let key = key_for(&listing.origin);
        let listing = Arc::new(listing);
        // Open harus sampai ke KEDUA worker pemuat.
        let open = || Command::Open {
            session: self.session,
            listing: listing.clone(),
        };
        if self.cmd_tx_main.send(open()).is_err() || self.cmd_tx_pref.send(open()).is_err() {
            self.notify(tr(lang, "err_loader_dead"));
            return;
        }
        self.listing = Some(listing);
        self.index = index;
        self.dir = 1;
        self.cache.clear();
        self.pending.clear();
        self.errors.clear();
        self.dims.clear();
        self.shown = None;
        self.placed_for = None;
        self.rotation = 0;
        self.zoom = Zoom::Fit(self.fit);
        self.land_bottom = false;
        self.position_key = Some(key);
        self.record_position();
        if self.store.state.recent_folders {
            record_recent(&mut self.store.state.recent, self.position_key.as_deref().unwrap_or(""));
            self.store_dirty = true;
        }
        self.after_page_change(ctx);
    }

    #[cfg(feature = "dialogs")]
    fn start_dialog(&mut self, ctx: &Context, kind: DialogKind) {
        let lang = self.lang;
        if self.dialog_busy {
            return;
        }
        self.dialog_busy = true;
        self.dialog_kind = Some(kind.clone());
        let start_dir = self
            .listing
            .as_ref()
            .map(|l| {
                if l.is_archive() {
                    l.origin.parent().map(Path::to_path_buf).unwrap_or_default()
                } else {
                    l.origin.clone()
                }
            })
            .filter(|p| p.is_dir());
        let tx = self.dialog_tx.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("roneyview-dialog".into())
            .spawn(move || {
                let picked = std::panic::catch_unwind(|| {
                    let mut d = rfd::FileDialog::new();
                    if let Some(dir) = start_dir {
                        d = d.set_directory(dir);
                    }
                    match kind {
                        DialogKind::File => {
                            let mut exts: Vec<String> = Vec::new();
                            for e in crate::source::IMAGE_EXTS
                                .iter()
                                .chain(crate::source::ARCHIVE_EXTS.iter())
                            {
                                exts.push((*e).to_string());
                                exts.push(e.to_uppercase());
                            }
                            d.set_title("Buka gambar atau arsip")
                                .add_filter(tr(lang, "dlg_open_file_filter"), &exts)
                                .pick_file()
                        }
                        DialogKind::Folder => d.set_title(tr(lang, "dlg_open_folder_title")).pick_folder(),
                        DialogKind::SaveImage { file_name, .. } => {
                            d.set_title(tr(lang, "dlg_save_image_title"))
                                .set_file_name(file_name)
                                .add_filter(
                                    tr(lang, "dlg_image_filter"),
                                    &[
                                        "png", "jpg", "jpeg", "webp", "gif", "bmp", "avif",
                                        "tif", "tiff", "PNG", "JPG", "JPEG", "WEBP", "GIF",
                                        "BMP", "AVIF",
                                    ],
                                )
                                .save_file()
                        }
                        DialogKind::SaveFrame { file_name, .. } => {
                            d.set_title(tr(lang, "dlg_save_frame_title"))
                                .set_file_name(file_name)
                                .add_filter("PNG", &["png", "PNG"])
                                .save_file()
                        }
                        DialogKind::ExportFrames { .. } => {
                            d.set_title(tr(lang, "dlg_export_frames_title")).pick_folder()
                        }
                        DialogKind::SaveEdit { file_name } => {
                            d.set_title(tr(lang, "dlg_save_edit_title"))
                                .set_file_name(file_name)
                                .add_filter("PNG", &["png", "PNG"])
                                .save_file()
                        }
                        DialogKind::SavePdf { file_name } => {
                            d.set_title(tr(lang, "dlg_save_pdf_title"))
                                .set_file_name(file_name)
                                .add_filter("PDF", &["pdf", "PDF"])
                                .save_file()
                        }
                    }
                })
                .unwrap_or(None);
                let _ = tx.send(picked);
                ctx.request_repaint();
            });
        if spawned.is_err() {
            self.dialog_busy = false;
            self.notify(tr(lang, "err_open_dialog"));
        }
    }

    #[cfg(not(feature = "dialogs"))]
    fn start_dialog(&mut self, _ctx: &Context, _kind: DialogKind) {
        let lang = self.lang;
        self.notify(tr(lang, "err_no_file_dialog"));
    }

    fn poll_dialog(&mut self, ctx: &Context) {
        #[cfg(feature = "dialogs")]
        while let Ok(result) = self.dialog_rx.try_recv() {
            self.dialog_busy = false;
            match self.dialog_kind.take() {
                Some(DialogKind::SaveImage { index, .. }) => {
                    if let Some(p) = result {
                        self.extract_page(index, &p);
                    }
                }
                Some(DialogKind::SaveFrame { index, frame, .. }) => {
                    if let Some(p) = result {
                        self.save_frame(index, frame, &p);
                    }
                }
                Some(DialogKind::ExportFrames { index }) => {
                    if let Some(dir) = result {
                        self.export_frames(index, &dir);
                    }
                }
                Some(DialogKind::SaveEdit { .. }) => {
                    if let Some(p) = result {
                        self.save_edit_buffer(&p);
                    }
                }
                Some(DialogKind::SavePdf { .. }) => {
                    if let Some(p) = result {
                        self.build_and_save_pdf(ctx, &p);
                    }
                }
                _ => {
                    if let Some(p) = result {
                        self.open_path(ctx, &p);
                    }
                }
            }
        }
        #[cfg(not(feature = "dialogs"))]
        let _ = ctx;
    }

    /// Buka dialog simpan untuk mengekstrak halaman ke-`index` sebagai berkas gambar.
    fn start_extract_dialog(&mut self, ctx: &Context, index: usize) {
        let lang = self.lang;
        let Some(listing) = self.listing.clone() else {
            return;
        };
        let file_name = listing
            .names
            .get(index)
            .map(|n| {
                Path::new(n)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| n.clone())
            })
            .unwrap_or_else(|| tr(lang, "extract_default_name").to_string());
        self.start_dialog(ctx, DialogKind::SaveImage { index, file_name });
    }

    /// Simpan byte asli halaman ke-`index` ke `dest` (tanpa encode ulang,
    /// jadi kualitasnya 100% sama dengan sumber).
    fn extract_page(&mut self, index: usize, dest: &Path) {
        let lang = self.lang;
        let Some(listing) = self.listing.clone() else {
            return;
        };
        let mut reader = match listing.open_reader() {
            Ok(r) => r,
            Err(e) => {
                self.notify(e);
                return;
            }
        };
        let bytes = match reader.read(index) {
            Ok(b) => b,
            Err(e) => {
                self.notify(e);
                return;
            }
        };
        match std::fs::write(dest, &bytes) {
            Ok(_) => self.notify(tr_fmt(lang, "notify_saved", &[&dest.display()])),
            Err(e) => self.notify(format!("{}: {e}", tr(lang, "notify_save_failed"))),
        }
    }

    /// Nama dasar berkas untuk frame: "nama" dari "folder/nama.gif".
    fn frame_stem(&self, index: usize) -> String {
        self.listing
            .as_ref()
            .and_then(|l| l.names.get(index))
            .map(|n| {
                Path::new(n)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "frame".to_string())
            })
            .unwrap_or_else(|| "frame".to_string())
    }

    /// Buka dialog simpan untuk frame ke-`frame` dari halaman animasi `index`.
    fn start_save_frame_dialog(&mut self, ctx: &Context, index: usize, frame: usize) {
        let stem = self.frame_stem(index);
        let file_name = format!("{stem}_frame{:03}.png", frame + 1);
        self.start_dialog(ctx, DialogKind::SaveFrame { index, frame, file_name });
    }

    /// Simpan satu frame animasi sebagai PNG.
    fn save_frame(&mut self, index: usize, frame: usize, dest: &Path) {
        let lang = self.lang;
        let Some(page) = self.page_for(index) else {
            return;
        };
        let Some(anim) = &page.anim else {
            return;
        };
        let Some(f) = anim.frames.get(frame) else {
            return;
        };
        let rgba = colorimage_to_rgba(&f.image);
        match crate::wallpaper::write_png(&rgba, dest) {
            Ok(_) => self.notify(tr_fmt(lang, "notify_saved", &[&dest.display()])),
            Err(e) => self.notify(e),
        }
    }

    /// Ekspor SEMUA frame animasi halaman `index` sebagai PNG ke `dir`.
    fn export_frames(&mut self, index: usize, dir: &Path) {
        let lang = self.lang;
        let Some(page) = self.page_for(index) else {
            return;
        };
        let Some(anim) = &page.anim else {
            return;
        };
        let total = anim.frames.len();
        let stem = self.frame_stem(index);
        let mut done = 0;
        for (i, f) in anim.frames.iter().enumerate() {
            let dest = dir.join(format!("{stem}_frame{:03}.png", i + 1));
            let rgba = colorimage_to_rgba(&f.image);
            if crate::wallpaper::write_png(&rgba, &dest).is_err() {
                break;
            }
            done += 1;
        }
        if done == total {
            self.notify(tr_fmt(lang, "notify_frames_exported", &[&done, &dir.display()]));
        } else {
            self.notify(tr(lang, "notify_save_failed"));
        }
    }

    /// Maju/mundur satu frame pada animasi yang sedang tampil (`dir` +1/-1).
    /// Otomatis menjeda animasi; melingkar di ujung.
    fn step_frame(&mut self, ctx: &Context, dir: i32) {
        let (shown_index, page) = match self.current_content() {
            Some(sh) if sh.slots.len() == 1 => {
                let (i, p) = sh.slots[0].clone();
                (i, p)
            }
            _ => return,
        };
        let n = match &page.anim {
            Some(a) => a.frames.len(),
            None => return,
        };
        self.anim_playing = false;
        self.anim_frame = step_index(self.anim_frame, n, dir);
        self.upload_frame(&page, self.anim_frame);
        // Selaraskan kunci animasi supaya saat diputar lagi lanjut dari sini.
        self.anim_key = Some((self.session, shown_index));
        ctx.request_repaint();
    }

    /// Panel perkakas kanan di tampilan edit. Mengembalikan aksi + apakah
    /// parameter tone berubah (perlu refresh pratinjau).
    fn draw_edit_panel(&mut self, ctx: &Context, ui: &mut egui::Ui) -> (Option<EditPanelAction>, bool) {
        let lang = self.lang;
        let mut action: Option<EditPanelAction> = None;
        let mut tone_changed = false;
        let Some(e) = self.edit.as_mut() else {
            return (None, false);
        };
        ui.heading(format!("{}: {}", tr(lang, "edit_title"), e.name));
        let (bw, bh) = (e.buffer.width(), e.buffer.height());
        ui.label(format!("{bw} × {bh} px"));
        if e.dirty {
            ui.colored_label(Color32::from_rgb(255, 200, 90), tr(lang, "edit_unsaved"));
        }
        ui.separator();
        match e.tool {
            EditTool::Crop => {
                ui.label(tr(lang, "edit_crop_hint"));
                let aspects = [
                    tr(lang, "edit_aspect_free"),
                    "1:1", "4:3", "3:2", "16:9", "2:3", "3:4", "9:16",
                ];
                egui::ComboBox::from_id_salt("crop_aspect")
                    .selected_text(aspects[e.crop_aspect])
                    .show_ui(ui, |ui| {
                        for (i, a) in aspects.iter().enumerate() {
                            if ui.selectable_value(&mut e.crop_aspect, i, *a).clicked() && e.crop_rect.is_some() {
                                e.crop_rect = e.crop_rect.map(|r| {
                                    edit::apply_aspect(r, crop_ratio(e.crop_aspect), (bw, bh))
                                });
                            }
                        }
                    });
                if let Some(mut rect) = e.crop_rect {
                    ui.separator();
                    ui.label(format!("{}: {} × {}", tr(lang, "edit_selection"), rect[2], rect[3]));
                    let mut changed = false;
                    changed |= ui.add(egui::DragValue::new(&mut rect[0]).range(0..=bw.saturating_sub(1)).prefix("X: ")).changed();
                    changed |= ui.add(egui::DragValue::new(&mut rect[1]).range(0..=bh.saturating_sub(1)).prefix("Y: ")).changed();
                    changed |= ui.add(egui::DragValue::new(&mut rect[2]).range(1..=bw).prefix("W: ")).changed();
                    changed |= ui.add(egui::DragValue::new(&mut rect[3]).range(1..=bh).prefix("H: ")).changed();
                    if changed {
                        rect[2] = rect[2].min(bw.saturating_sub(rect[0])).max(1);
                        rect[3] = rect[3].min(bh.saturating_sub(rect[1])).max(1);
                        e.crop_rect = Some(rect);
                    }
                    ui.horizontal(|ui| {
                        if ui.button(tr(lang, "edit_apply_crop")).clicked() {
                            action = Some(EditPanelAction::ApplyCrop);
                        }
                        if ui.button(tr(lang, "edit_clear_selection")).clicked() {
                            action = Some(EditPanelAction::ClearCrop);
                        }
                    });
                    if ui.button(tr(lang, "edit_copy")).clicked() {
                        action = Some(EditPanelAction::CopyCrop);
                    }
                } else {
                    ui.label(tr(lang, "edit_no_selection"));
                }
            }
            EditTool::Resize => {
                ui.label(tr(lang, "edit_resize_hint"));
                ui.radio_value(&mut e.resize_by_pct, false, tr(lang, "edit_resize_pixels"));
                ui.radio_value(&mut e.resize_by_pct, true, tr(lang, "edit_resize_percent"));
                if e.resize_by_pct {
                    let mut pct = e.resize_pct;
                    if ui.add(egui::DragValue::new(&mut pct).range(1..=400).suffix(" %")).changed() {
                        e.resize_pct = pct;
                        e.resize_w = ((bw as u64 * pct as u64) / 100).max(1) as u32;
                        e.resize_h = ((bh as u64 * pct as u64) / 100).max(1) as u32;
                    }
                } else {
                    let mut w = e.resize_w;
                    if ui.add(egui::DragValue::new(&mut w).range(1..=16384).prefix("W: ")).changed() {
                        if e.resize_lock {
                            e.resize_h = edit::resize_keep_aspect(bw, bh, Some(w), None).1;
                        }
                        e.resize_w = w;
                    }
                    let mut h = e.resize_h;
                    if ui.add(egui::DragValue::new(&mut h).range(1..=16384).prefix("H: ")).changed() {
                        if e.resize_lock {
                            e.resize_w = edit::resize_keep_aspect(bw, bh, None, Some(h)).0;
                        }
                        e.resize_h = h;
                    }
                }
                ui.checkbox(&mut e.resize_lock, tr(lang, "edit_resize_lock"));
                let filters = [
                    tr(lang, "edit_filter_nearest"),
                    tr(lang, "edit_filter_triangle"),
                    tr(lang, "edit_filter_catmull"),
                    tr(lang, "edit_filter_lanczos"),
                    tr(lang, "edit_filter_gaussian"),
                ];
                egui::ComboBox::from_id_salt("resize_filter")
                    .selected_text(filters[e.resize_filter])
                    .show_ui(ui, |ui| {
                        for (i, f) in filters.iter().enumerate() {
                            ui.selectable_value(&mut e.resize_filter, i, *f);
                        }
                    });
                let mp = (e.resize_w as f64 * e.resize_h as f64) / 1_000_000.0;
                ui.label(format!("{}: {} × {} px ({:.2} MP)", tr(lang, "edit_new_size"), e.resize_w, e.resize_h, mp));
                if ui.button(tr(lang, "edit_apply_resize")).clicked() {
                    action = Some(EditPanelAction::ApplyResize);
                }
            }
            EditTool::Picker => {
                ui.label(tr(lang, "edit_picker_hint"));
                let sample = e.pick.or(e.pick_hover);
                if let Some((x, y)) = sample {
                    let px = e.buffer.get_pixel(x, y);
                    let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
                    ui.separator();
                    ui.label(format!("{}: ({x}, {y})", tr(lang, "edit_pixel")));
                    // Contoh warna.
                    let (sw, _) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::hover());
                    ui.painter().rect_filled(sw, 4.0, Color32::from_rgba_unmultiplied(r, g, b, a));
                    let hex = edit::fmt_hex(r, g, b, a, true);
                    ui.label(format!("RGBA: {r}, {g}, {b}, {a}"));
                    ui.label(format!("HEX: {hex}"));
                    if ui.button(tr(lang, "edit_copy_hex")).clicked() {
                        action = Some(EditPanelAction::CopyHex(hex));
                    }
                    let (h, s, l) = edit::rgb_to_hsl(r, g, b);
                    ui.label(format!("HSL: {h:.0}°, {:.0}%, {:.0}%", s * 100.0, l * 100.0));
                    let (h, s, v) = edit::rgb_to_hsv(r, g, b);
                    ui.label(format!("HSV: {h:.0}°, {:.0}%, {:.0}%", s * 100.0, v * 100.0));
                    let (c, m, y, k) = edit::rgb_to_cmyk(r, g, b);
                    ui.label(format!(
                        "CMYK: {:.0}%, {:.0}%, {:.0}%, {:.0}%",
                        c * 100.0,
                        m * 100.0,
                        y * 100.0,
                        k * 100.0
                    ));
                    let (l, la, lb) = edit::rgb_to_lab(r, g, b);
                    ui.label(format!("CIELAB: L {l:.1}, a {la:.1}, b {lb:.1}"));
                    if e.pick.is_none() {
                        ui.label(tr(lang, "edit_picker_hover_note"));
                    }
                } else {
                    ui.label(tr(lang, "edit_picker_empty"));
                }
            }
            EditTool::Rotate => {
                ui.label(tr(lang, "edit_rotate_hint"));
                ui.horizontal(|ui| {
                    if ui.button(tr(lang, "edit_rotate_ccw")).clicked() {
                        action = Some(EditPanelAction::RotateCcw);
                    }
                    if ui.button(tr(lang, "edit_rotate_cw")).clicked() {
                        action = Some(EditPanelAction::RotateCw);
                    }
                    if ui.button(tr(lang, "edit_rotate_180")).clicked() {
                        action = Some(EditPanelAction::Rotate180);
                    }
                });
            }
            EditTool::Tone => {
                ui.label(tr(lang, "edit_tone_hint"));
                if e.tone_float.is_some() {
                    ui.colored_label(Color32::from_rgb(140, 220, 160), tr(lang, "edit_tone_hdr_note"));
                }
                tone_changed |= ui
                    .add(egui::Slider::new(&mut e.tone.exposure, -3.0..=2.0).text(tr(lang, "edit_tone_exposure")).step_by(0.1).max_decimals(2))
                    .changed();
                tone_changed |= ui
                    .add(egui::Slider::new(&mut e.tone.reference_white, 0.5..=2.0).text(tr(lang, "edit_tone_white")).step_by(0.05).max_decimals(2))
                    .changed();
                tone_changed |= ui
                    .add(egui::Slider::new(&mut e.tone.highlight, 0.0..=1.0).text(tr(lang, "edit_tone_highlight")).step_by(0.05).max_decimals(2))
                    .changed();
                tone_changed |= ui
                    .add(egui::Slider::new(&mut e.tone.saturation, 0.0..=2.0).text(tr(lang, "edit_tone_saturation")).step_by(0.05).max_decimals(2))
                    .changed();
                // Batas input ketikan: 2 digit di belakang koma, dijepit ke rentang
                // masing-masing (terutama exposure: -3..=2).
                let clamp2 = |v: f32, lo: f32, hi: f32| ((v * 100.0).round() / 100.0).clamp(lo, hi);
                e.tone.exposure = clamp2(e.tone.exposure, -3.0, 2.0);
                e.tone.reference_white = clamp2(e.tone.reference_white, 0.5, 2.0);
                e.tone.highlight = clamp2(e.tone.highlight, 0.0, 1.0);
                e.tone.saturation = clamp2(e.tone.saturation, 0.0, 2.0);
                ui.horizontal(|ui| {
                    if ui.button(tr(lang, "edit_apply_tone")).clicked() {
                        action = Some(EditPanelAction::ApplyTone);
                    }
                    if ui.button(tr(lang, "edit_reset_tone")).clicked() {
                        action = Some(EditPanelAction::ResetTone);
                    }
                });
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(tr(lang, "edit_save")).clicked() {
                action = Some(EditPanelAction::Save);
            }
            if ui.button(tr(lang, "edit_reset")).clicked() {
                action = Some(EditPanelAction::ResetAll);
            }
            if ui.button(tr(lang, "edit_cancel")).clicked() {
                action = Some(EditPanelAction::Cancel);
            }
        });
        let _ = ctx;
        (action, tone_changed)
    }

    /// Eksekusi aksi dari panel edit.
    fn handle_edit_action(&mut self, ctx: &Context, a: EditPanelAction) {
        let lang = self.lang;
        match a {
            EditPanelAction::ApplyCrop => self.apply_crop(),
            EditPanelAction::ClearCrop => {
                if let Some(e) = &mut self.edit {
                    e.crop_rect = None;
                }
            }
            EditPanelAction::CopyCrop => self.copy_crop(),
            EditPanelAction::ApplyResize => self.apply_resize(),
            EditPanelAction::RotateCw => self.rotate_edit_buffer(true),
            EditPanelAction::RotateCcw => self.rotate_edit_buffer(false),
            EditPanelAction::Rotate180 => {
                self.rotate_edit_buffer(true);
                self.rotate_edit_buffer(true);
            }
            EditPanelAction::ApplyTone => self.apply_tone(),
            EditPanelAction::ResetTone => {
                if let Some(e) = &mut self.edit {
                    e.tone = ToneParams::default();
                    e.tone_preview = None;
                }
                self.edit_tex_dirty = true;
            }
            EditPanelAction::ResetAll => {
                if let Some(e) = &mut self.edit {
                    e.buffer = e.original.clone();
                    e.crop_rect = None;
                    e.tone = ToneParams::default();
                    e.tone_preview = None;
                    e.tone_draft_src = None;
                    e.tone_draft_float = None;
                    e.dirty = false;
                }
                self.edit_tex_dirty = true;
            }
            EditPanelAction::Save => {
                let file_name = self
                    .edit
                    .as_ref()
                    .map(|e| {
                        let stem = Path::new(&e.name)
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "gambar".to_string());
                        format!("{stem}_edit.png")
                    })
                    .unwrap_or_else(|| "gambar_edit.png".to_string());
                self.start_dialog(ctx, DialogKind::SaveEdit { file_name });
            }
            EditPanelAction::Cancel => self.close_edit(),
            EditPanelAction::CopyHex(hex) => {
                ctx.copy_text(hex);
                self.notify(tr(lang, "edit_copied"));
            }
        }
    }

    /// Tampilan edit: area gambar (fit) + panel perkakas kanan.
    fn draw_edit_view(&mut self, ctx: &Context, ui: &mut egui::Ui) {
        let lang = self.lang;
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.close_edit();
            return;
        }
        if self.edit_tex_dirty {
            self.edit_tex_dirty = false;
            if let Some(e) = &self.edit {
                let img = e.display_image();
                let ci = ColorImage::from_rgba_unmultiplied(
                    [img.width() as usize, img.height() as usize],
                    img.as_raw(),
                );
                self.edit_tex = Some(ctx.load_texture("edit", ci, TextureOptions::LINEAR));
            }
        }
        let mut action: Option<EditPanelAction> = None;
        // Tangkap area panel pusat SEBELUM layout horizontal: available_height()
        // di dalam layout horizontal hanya mengembalikan tinggi satu baris.
        let avail = ui.available_rect_before_wrap();
        let img_size = vec2((avail.width() - 292.0).max(120.0), avail.height());
        ui.horizontal(|ui| {
            let (img_rect, img_resp) =
                ui.allocate_exact_size(img_size, Sense::click_and_drag());
            let painter = ui.painter_at(img_rect);
            painter.rect_filled(img_rect, 0.0, BG);
            let tool = self.edit.as_ref().map(|e| e.tool);
            if let (Some(e), Some(tex)) = (&self.edit, &self.edit_tex) {
                let img = e.display_image();
                let scale = fit_scale(
                    FitMode::Fit,
                    vec2(img.width() as f32, img.height() as f32),
                    img_rect.size(),
                );
                let size = vec2(img.width() as f32 * scale, img.height() as f32 * scale);
                let rect = Rect::from_center_size(img_rect.center(), size);
                self.edit_img_rect = rect;
                self.edit_scale = scale;
                paint_page(&painter, tex.id(), rect, 0);
                match tool {
                    Some(EditTool::Crop) => self.draw_crop_overlay(&painter, &img_resp),
                    Some(EditTool::Picker) => self.draw_picker_overlay(&painter, &img_resp, tex.id()),
                    _ => {}
                }
            }
            ui.separator();
            ui.vertical(|ui| {
                ui.set_width(280.0);
                let (a, tone_changed) = self.draw_edit_panel(ctx, ui);
                action = a;
                if tone_changed {
                    self.refresh_tone_preview();
                }
            });
        });
        if let Some(a) = action {
            self.handle_edit_action(ctx, a);
        }
        let _ = lang;
    }

/// Koordinat layar -> koordinat gambar di tampilan edit.
fn screen_to_image(rect: Rect, scale: f32, pos: Pos2) -> (f32, f32) {
    let s = scale.max(1e-6);
    ((pos.x - rect.min.x) / s, (pos.y - rect.min.y) / s)
}

    /// Overlay crop: buat seleksi baru, geser seleksi, atau ubah ukuran lewat pegangan.
    fn draw_crop_overlay(&mut self, painter: &egui::Painter, resp: &egui::Response) {
        let (img_rect, img_scale) = (self.edit_img_rect, self.edit_scale);
        let Some(e) = &mut self.edit else {
            return;
        };
        let (bw, bh) = (e.buffer.width(), e.buffer.height());
        let to_img = |pos: Pos2| Self::screen_to_image(img_rect, img_scale, pos);
        // Radius tangkap pegangan (koordinat gambar).
        let grab_r = 10.0 / img_scale.max(0.01);

        // Kursor hover mengikuti posisi pointer.
        if !resp.dragged() {
            if let (Some(rect), Some(pos)) =
                (e.crop_rect, resp.hover_pos().or_else(|| resp.interact_pointer_pos()))
            {
                let icon = match Self::crop_hit(rect, to_img(pos), grab_r) {
                    CropHit::Handle(CropHandle::LeftTop) | CropHit::Handle(CropHandle::RightBottom) => {
                        egui::CursorIcon::ResizeNwSe
                    }
                    CropHit::Handle(CropHandle::RightTop) | CropHit::Handle(CropHandle::LeftBottom) => {
                        egui::CursorIcon::ResizeNeSw
                    }
                    CropHit::Handle(CropHandle::Left) | CropHit::Handle(CropHandle::Right) => {
                        egui::CursorIcon::ResizeHorizontal
                    }
                    CropHit::Handle(CropHandle::Top) | CropHit::Handle(CropHandle::Bottom) => {
                        egui::CursorIcon::ResizeVertical
                    }
                    CropHit::Inside => egui::CursorIcon::Move,
                    CropHit::Outside => egui::CursorIcon::Crosshair,
                };
                resp.ctx.set_cursor_icon(icon);
            }
        }

        // Mulai seret: tentukan mode dari posisi tekan.
        if resp.drag_started() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let p = to_img(pos);
                e.crop_drag = Some(match e.crop_rect {
                    Some(rect) => match Self::crop_hit(rect, p, grab_r) {
                        CropHit::Handle(handle) => CropDrag::Resize { rect, handle },
                        CropHit::Inside => CropDrag::Move { rect, grab: p },
                        CropHit::Outside => CropDrag::New { start: p },
                    },
                    None => CropDrag::New { start: p },
                });
            }
        }
        if resp.dragged() {
            if let (Some(drag), Some(pos)) = (e.crop_drag, resp.interact_pointer_pos()) {
                let cur = to_img(pos);
                let ratio = crop_ratio(e.crop_aspect);
                match drag {
                    CropDrag::New { start } => {
                        let mut r = edit::drag_rect(start, cur, (bw, bh));
                        if let Some(rr) = &mut r {
                            *rr = edit::apply_aspect(*rr, ratio, (bw, bh));
                        }
                        e.crop_rect = r;
                    }
                    CropDrag::Move { rect, grab } => {
                        e.crop_rect =
                            Some(edit::move_rect(rect, (cur.0 - grab.0, cur.1 - grab.1), (bw, bh)));
                    }
                    CropDrag::Resize { rect, handle } => {
                        let (x, y, w, h) =
                            (rect[0] as f32, rect[1] as f32, rect[2] as f32, rect[3] as f32);
                        e.crop_rect = Some(match handle {
                            CropHandle::LeftTop => {
                                edit::resize_crop_corner((x + w, y + h), cur, ratio, (bw, bh))
                            }
                            CropHandle::RightTop => {
                                edit::resize_crop_corner((x, y + h), cur, ratio, (bw, bh))
                            }
                            CropHandle::RightBottom => {
                                edit::resize_crop_corner((x, y), cur, ratio, (bw, bh))
                            }
                            CropHandle::LeftBottom => {
                                edit::resize_crop_corner((x + w, y), cur, ratio, (bw, bh))
                            }
                            CropHandle::Left => {
                                edit::resize_crop_edge(rect, edit::CropEdge::Left, cur, ratio, (bw, bh))
                            }
                            CropHandle::Right => {
                                edit::resize_crop_edge(rect, edit::CropEdge::Right, cur, ratio, (bw, bh))
                            }
                            CropHandle::Top => {
                                edit::resize_crop_edge(rect, edit::CropEdge::Top, cur, ratio, (bw, bh))
                            }
                            CropHandle::Bottom => {
                                edit::resize_crop_edge(rect, edit::CropEdge::Bottom, cur, ratio, (bw, bh))
                            }
                        });
                    }
                }
            }
        }
        if resp.drag_stopped() {
            e.crop_drag = None;
        }
        // Gambar seleksi.
        if let Some(rect) = e.crop_rect {
            let r = self.edit_img_rect;
            let s = self.edit_scale;
            let sel = Rect::from_min_size(
                pos2(r.min.x + rect[0] as f32 * s, r.min.y + rect[1] as f32 * s),
                vec2(rect[2] as f32 * s, rect[3] as f32 * s),
            );
            // Gelapkan luar seleksi.
            let dim = Color32::from_black_alpha(110);
            painter.rect_filled(Rect::from_min_max(r.min, pos2(sel.max.x, sel.min.y)), 0.0, dim);
            painter.rect_filled(Rect::from_min_max(pos2(r.min.x, sel.min.y), pos2(sel.min.x, sel.max.y)), 0.0, dim);
            painter.rect_filled(Rect::from_min_max(pos2(sel.max.x, sel.min.y), pos2(r.max.x, sel.max.y)), 0.0, dim);
            painter.rect_filled(Rect::from_min_max(pos2(sel.min.x, sel.max.y), r.max), 0.0, dim);
            painter.rect_stroke(sel, 0.0, egui::Stroke::new(1.5_f32, Color32::WHITE), egui::StrokeKind::Inside);
            // Pegangan sudut + tengah sisi (interaktif).
            for c in [
                sel.left_top(),
                pos2(sel.center().x, sel.top()),
                sel.right_top(),
                pos2(sel.right(), sel.center().y),
                sel.right_bottom(),
                pos2(sel.center().x, sel.bottom()),
                sel.left_bottom(),
                pos2(sel.left(), sel.center().y),
            ] {
                painter.rect_filled(Rect::from_center_size(c, vec2(8.0, 8.0)), 1.0, Color32::WHITE);
            }
        }
    }

    /// Hit-test pointer (koordinat gambar) terhadap seleksi crop.
    fn crop_hit(rect: [u32; 4], p: (f32, f32), rad: f32) -> CropHit {
        let (x, y, w, h) = (rect[0] as f32, rect[1] as f32, rect[2] as f32, rect[3] as f32);
        let cx = x + w / 2.0;
        let cy = y + h / 2.0;
        let near = |qx: f32, qy: f32| (p.0 - qx).abs() <= rad && (p.1 - qy).abs() <= rad;
        // Sudut dulu (prioritas), lalu tengah sisi.
        if near(x, y) {
            return CropHit::Handle(CropHandle::LeftTop);
        }
        if near(x + w, y) {
            return CropHit::Handle(CropHandle::RightTop);
        }
        if near(x + w, y + h) {
            return CropHit::Handle(CropHandle::RightBottom);
        }
        if near(x, y + h) {
            return CropHit::Handle(CropHandle::LeftBottom);
        }
        if near(cx, y) {
            return CropHit::Handle(CropHandle::Top);
        }
        if near(cx, y + h) {
            return CropHit::Handle(CropHandle::Bottom);
        }
        if near(x, cy) {
            return CropHit::Handle(CropHandle::Left);
        }
        if near(x + w, cy) {
            return CropHit::Handle(CropHandle::Right);
        }
        if p.0 >= x && p.0 <= x + w && p.1 >= y && p.1 <= y + h {
            CropHit::Inside
        } else {
            CropHit::Outside
        }
    }

    /// Overlay color picker: sampel hover/klik + kaca pembesar.
    fn draw_picker_overlay(&mut self, painter: &egui::Painter, resp: &egui::Response, tex_id: egui::TextureId) {
        let (img_rect, img_scale) = (self.edit_img_rect, self.edit_scale);
        let Some(e) = &mut self.edit else {
            return;
        };
        let (bw, bh) = (e.buffer.width(), e.buffer.height());
        if let Some(pos) = resp.hover_pos().or_else(|| resp.interact_pointer_pos()) {
            let (fx, fy) = Self::screen_to_image(img_rect, img_scale, pos);
            let (ix, iy) = (fx as i32, fy as i32);
            if ix >= 0 && iy >= 0 && (ix as u32) < bw && (iy as u32) < bh {
                let p = (ix as u32, iy as u32);
                e.pick_hover = Some(p);
                // Kaca pembesar 9x dari daerah 25x25 di sekitar kursor.
                let r = self.edit_img_rect;
                let uv = Rect::from_min_max(
                    pos2((p.0.saturating_sub(12)) as f32 / bw as f32, (p.1.saturating_sub(12)) as f32 / bh as f32),
                    pos2((p.0 + 13).min(bw) as f32 / bw as f32, (p.1 + 13).min(bh) as f32 / bh as f32),
                );
                let mut mag = Rect::from_min_size(pos + vec2(16.0, 16.0), vec2(150.0, 150.0));
                if mag.max.x > r.max.x {
                    mag = mag.translate(vec2(-(mag.width() + 32.0), 0.0));
                }
                if mag.max.y > r.max.y {
                    mag = mag.translate(vec2(0.0, -(mag.height() + 32.0)));
                }
                painter.rect_filled(mag, 4.0, Color32::BLACK);
                painter.image(tex_id, mag.shrink(2.0), uv, Color32::WHITE);
                painter.rect_stroke(mag, 4.0, egui::Stroke::new(1.0_f32, Color32::WHITE), egui::StrokeKind::Inside);
                // Garis bidik di tengah kaca pembesar.
                let c = mag.center();
                painter.line_segment([pos2(c.x - 8.0, c.y), pos2(c.x + 8.0, c.y)], egui::Stroke::new(1.0_f32, Color32::WHITE));
                painter.line_segment([pos2(c.x, c.y - 8.0), pos2(c.x, c.y + 8.0)], egui::Stroke::new(1.0_f32, Color32::WHITE));
            } else {
                e.pick_hover = None;
            }
        }
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let (fx, fy) = Self::screen_to_image(img_rect, img_scale, pos);
                let (ix, iy) = (fx as i32, fy as i32);
                if ix >= 0 && iy >= 0 && (ix as u32) < bw && (iy as u32) < bh {
                    e.pick = Some((ix as u32, iy as u32));
                }
            }
        }
        // Tandai piksel terkunci.
        if let Some(p) = e.pick {
            let r = self.edit_img_rect;
            let s = self.edit_scale;
            let c = pos2(r.min.x + (p.0 as f32 + 0.5) * s, r.min.y + (p.1 as f32 + 0.5) * s);
            painter.circle_stroke(c, 7.0, egui::Stroke::new(1.5_f32, Color32::WHITE));
            painter.circle_stroke(c, 8.5, egui::Stroke::new(1.0_f32, Color32::BLACK));
        }
    }

    /// Dekode sumber gambar untuk sesi edit pada resolusi penuh (dengan batas
    /// alokasi). Mengembalikan (buffer RGBA8, buffer HDR linier bila .hdr).
    fn decode_edit_source(bytes: &[u8]) -> Result<(RgbaImage, Option<Vec<f32>>), String> {
        use image::metadata::Orientation;
        use image::{DynamicImage, ImageReader, Limits};
        let mut reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| format!("Gagal membaca berkas: {e}"))?;
        let mut limits = Limits::default();
        limits.max_alloc = Some(512 << 20);
        limits.max_image_width = Some(65535);
        limits.max_image_height = Some(65535);
        reader.limits(limits);
        let mut decoder = reader
            .into_decoder()
            .map_err(|_| "Format tidak dikenali atau berkas rusak".to_string())?;
        let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
        let mut img = DynamicImage::from_decoder(decoder)
            .map_err(|_| "Gambar rusak atau tidak lengkap".to_string())?;
        img.apply_orientation(orientation);
        if matches!(img, DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_)) {
            let rgb = img.to_rgb32f();
            let (w, h) = (rgb.width(), rgb.height());
            let float = rgb.into_raw();
            let u8img = edit::tone_map_linear_buffer(&float, w, h, &ToneParams::default());
            return Ok((u8img, Some(float)));
        }
        Ok((img.to_rgba8(), None))
    }

    /// Mulai sesi edit untuk halaman saat ini dengan perkakas `tool`.
    /// Ditolak untuk gambar di dalam arsip (harus disimpan dulu).
    fn start_edit(&mut self, tool: EditTool) {
        let lang = self.lang;
        let Some(listing) = self.listing.clone() else {
            return;
        };
        if listing.is_archive() {
            self.notify(tr(lang, "edit_archive_blocked"));
            return;
        }
        let index = self.index;
        let name = listing.names.get(index).cloned().unwrap_or_default();
        let mut reader = match listing.open_reader() {
            Ok(r) => r,
            Err(e) => {
                self.notify(e);
                return;
            }
        };
        let bytes = match reader.read(index) {
            Ok(b) => b,
            Err(e) => {
                self.notify(e);
                return;
            }
        };
        match Self::decode_edit_source(&bytes) {
            Ok((buffer, float)) => {
                self.edit = Some(EditSession::new(name, buffer, float, tool));
                self.edit_tex = None;
                self.edit_tex_dirty = true;
            }
            Err(e) => self.notify(e),
        }
    }

    /// Beralih perkakas di dalam sesi edit yang sedang berjalan (buffer tetap).
    fn switch_edit_tool(&mut self, tool: EditTool) {
        if let Some(e) = &mut self.edit {
            e.tool = tool;
            if tool != EditTool::Tone {
                e.tone_preview = None;
            }
        }
    }

    /// Putar buffer edit 90° (`cw` = searah jarum jam), langsung ke data gambar
    /// sehingga bisa disimpan ke berkas. Membuka sesi edit bila belum ada.
    fn rotate_edit_buffer(&mut self, cw: bool) {
        if self.edit.is_none() {
            self.start_edit(EditTool::Rotate);
        }
        self.switch_edit_tool(EditTool::Rotate);
        if let Some(e) = &mut self.edit {
            e.buffer = if cw {
                image::imageops::rotate90(&e.buffer)
            } else {
                image::imageops::rotate270(&e.buffer)
            };
            e.crop_rect = None;
            e.tone_draft_src = None;
            e.tone_draft_float = None;
            e.dirty = true;
        }
        self.edit_tex_dirty = true;
    }

    /// Tutup sesi edit, membuang perubahan yang belum disimpan.
    fn close_edit(&mut self) {
        self.edit = None;
        self.edit_tex = None;
    }

    /// Simpan buffer edit saat ini sebagai PNG (dialog "Simpan").
    fn save_edit_buffer(&mut self, dest: &Path) {
        let lang = self.lang;
        let Some(e) = &self.edit else {
            return;
        };
        let img = e.display_image().clone();
        match crate::wallpaper::write_png(&img, dest) {
            Ok(_) => {
                if let Some(e) = &mut self.edit {
                    e.dirty = false;
                }
                self.notify(tr_fmt(lang, "notify_saved", &[&dest.display()]));
            }
            Err(e) => self.notify(e),
        }
    }

    /// Terapkan crop: buffer menjadi daerah seleksi.
    fn apply_crop(&mut self) {
        let Some(e) = &mut self.edit else {
            return;
        };
        let Some(rect) = e.crop_rect else {
            return;
        };
        e.buffer = edit::crop_image(&e.buffer, rect);
        e.crop_rect = None;
        e.tone_float = None; // buffer berubah -> sumber HDR linier tidak lagi cocok
        e.tone_draft_src = None;
        e.tone_draft_float = None;
        e.dirty = true;
        self.edit_tex_dirty = true;
    }

    /// Terapkan resize: buffer diubah ke dimensi target.
    fn apply_resize(&mut self) {
        let (w, h, filter_idx) = match &self.edit {
            Some(e) => (e.resize_w, e.resize_h, e.resize_filter),
            None => return,
        };
        let Some(e) = &mut self.edit else {
            return;
        };
        e.buffer = edit::resize_image(&e.buffer, w.max(1), h.max(1), resize_filter(filter_idx));
        e.tone_float = None;
        e.tone_draft_src = None;
        e.tone_draft_float = None;
        e.dirty = true;
        self.edit_tex_dirty = true;
    }

    /// Terapkan tone mapping: buffer menjadi hasil tone full-res.
    /// Pratinjau saat slider digeser memang versi kecil (cepat); di sini
    /// dihitung ulang full-res supaya hasil akhir tajam.
    fn apply_tone(&mut self) {
        let Some(e) = &mut self.edit else {
            return;
        };
        let p = e.tone;
        if let Some(float) = e.tone_float.take() {
            let (w, h) = (e.buffer.width(), e.buffer.height());
            e.buffer = edit::tone_map_linear_buffer(&float, w, h, &p);
        } else {
            let src = e.buffer.clone();
            e.buffer = edit::tone_map_srgb(&src, &p);
        }
        e.tone_preview = None;
        e.tone_draft_src = None;
        e.tone_draft_float = None;
        e.tone = ToneParams::default();
        e.dirty = true;
        self.edit_tex_dirty = true;
    }

    /// Salin hasil crop (atau seluruh buffer) ke clipboard sebagai gambar.
    fn copy_crop(&mut self) {
        let lang = self.lang;
        let Some(e) = &self.edit else {
            return;
        };
        let img = match e.crop_rect {
            Some(rect) => edit::crop_image(&e.buffer, rect),
            None => e.buffer.clone(),
        };
        let data = arboard::ImageData {
            width: img.width() as usize,
            height: img.height() as usize,
            bytes: std::borrow::Cow::Owned(img.into_raw()),
        };
        match arboard::Clipboard::new().and_then(|mut c| c.set_image(data)) {
            Ok(_) => self.notify(tr(lang, "edit_copied")),
            Err(e) => self.notify(format!("{}: {e}", tr(lang, "notify_save_failed"))),
        }
    }

    /// Perbarui pratinjau tone mapping: versi KECIL (cepat) untuk tampilan
    /// saat slider digeser. Sumber kecil di-cache (buffer tidak berubah saat
    /// slider digeser), jadi tiap tick cuma tone mapping ~0.6MP.
    /// Versi full-res dihitung saat Apply ditekan.
    fn refresh_tone_preview(&mut self) {
        let Some(e) = &mut self.edit else {
            return;
        };
        let p = e.tone;
        if let Some(float) = &e.tone_float {
            if e.tone_draft_float.is_none() {
                let (w, h) = (e.buffer.width(), e.buffer.height());
                e.tone_draft_float = Some(edit::shrink_rgb32f(float, w, h, edit::TONE_PREVIEW_MAX_SIDE));
            }
            let (small, sw, sh) = e.tone_draft_float.as_ref().unwrap();
            e.tone_preview = Some(edit::tone_map_linear_buffer(small, *sw, *sh, &p));
        } else {
            if e.tone_draft_src.is_none() {
                e.tone_draft_src = Some(edit::shrink_rgba(&e.buffer, edit::TONE_PREVIEW_MAX_SIDE));
            }
            let src = e.tone_draft_src.as_ref().unwrap();
            e.tone_preview = Some(edit::tone_map_srgb(src, &p));
        }
        self.edit_tex_dirty = true;
    }

    // ------------------------------------------------------------ cetak / PDF

    /// Minta worker mendekode indeks-indeks ini (pratinjau cetak).
    fn request_indices(&mut self, ctx: &Context, indices: &[usize]) {
        let Some(l) = self.listing.clone() else {
            return;
        };
        let max_side = Self::max_side(ctx);
        let max_pixels = tier_pixels(self.display_scale).min(loader::max_pixels());
        for &i in indices {
            if i >= l.len() || self.cache.contains_key(&i) || self.errors.contains_key(&i) || self.pending.contains_key(&i) {
                continue;
            }
            self.pending.insert(i, false);
            let held: usize = self.cache.values().map(|p| p.bytes).sum();
            let cmd = Command::Load {
                session: self.session,
                index: i,
                primary: false,
                max_side,
                max_pixels,
                focus: self.index,
                budget: self.budget,
                held,
            };
            if self.cmd_tx_pref.send(cmd).is_err() {
                self.pending.remove(&i);
            }
        }
    }

    fn open_print_window(&mut self) {
        self.print_open = true;
        self.print_page = 0;
        self.print_status.clear();
        self.print_thumbs.clear();
    }

    /// Ukuran kertas dalam point (1/72 inci) untuk pilihan kertas/orientasi.
    fn print_paper_size(&self) -> (f32, f32) {
        let (w, h) = if self.print_paper == 1 { (612.0, 792.0) } else { (595.0, 842.0) };
        if self.print_landscape { (h, w) } else { (w, h) }
    }

    /// Grid tata letak: (kolom, baris). 0 = halaman penuh, 1 = 2x2, 2 = 3x3, 3 = 4x4.
    fn print_grid(&self) -> (usize, usize) {
        match self.print_layout {
            1 => (2, 2),
            2 => (3, 3),
            3 => (4, 4),
            _ => (1, 1),
        }
    }

    fn print_page_count(&self) -> usize {
        let n = self.listing.as_ref().map_or(0, |l| l.len());
        let (c, r) = self.print_grid();
        let per = (c * r).max(1);
        (n + per - 1) / per
    }

    /// Indeks gambar pada halaman cetak ke-`page`.
    fn print_page_indices(&self, page: usize) -> Vec<usize> {
        let n = self.listing.as_ref().map_or(0, |l| l.len());
        let (c, r) = self.print_grid();
        let per = (c * r).max(1);
        let start = page * per;
        (start..(start + per).min(n)).collect()
    }

    /// Jendela Cetak / Simpan PDF: tata letak, pratinjau, simpan/cetak.
    /// Jendela About: judul "ver <versi>", nama, "Developed by Tengkoru",
    /// dan daftar pustaka yang dipakai (seperti dialog About FireAlpaca).
    fn draw_about_window(&mut self, ctx: &Context) {
        let lang = self.lang;
        let mut open = self.about_open;
        let mut close = false;
        let ver = env!("CARGO_PKG_VERSION");
        // Pustaka langsung yang dipakai Roneyview.
        let libs = [
            ("arboard", "https://crates.io/crates/arboard"),
            ("eframe", "https://crates.io/crates/eframe"),
            ("egui", "https://crates.io/crates/egui"),
            ("fast_image_resize", "https://crates.io/crates/fast_image_resize"),
            ("image", "https://crates.io/crates/image"),
            ("libheif-rs", "https://crates.io/crates/libheif-rs"),
            ("rfd", "https://crates.io/crates/rfd"),
            ("serde", "https://crates.io/crates/serde"),
            ("serde_json", "https://crates.io/crates/serde_json"),
            ("tiff", "https://crates.io/crates/tiff"),
            ("trash", "https://crates.io/crates/trash"),
            ("unrar", "https://crates.io/crates/unrar"),
            ("x11rb", "https://crates.io/crates/x11rb"),
            ("zip", "https://crates.io/crates/zip"),
        ];
        egui::Window::new(format!("ver {ver}"))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_size([400.0, 380.0])
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("Roneyview");
                    ui.label(format!("ver {ver}"));
                    ui.label(tr(lang, "about_developed"));
                });
                ui.separator();
                ui.label(tr(lang, "about_libraries"));
                egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                    for (name, url) in libs {
                        ui.label(format!("{name} - {url}"));
                    }
                });
                ui.separator();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("OK").clicked() {
                        close = true;
                    }
                });
            });
        if close {
            open = false;
        }
        self.about_open = open;
    }

    fn draw_print_window(&mut self, ctx: &Context) {
        let lang = self.lang;
        let mut open = self.print_open;
        let total_pages = self.print_page_count().max(1);
        if self.print_page >= total_pages {
            self.print_page = total_pages - 1;
        }
        let indices = self.print_page_indices(self.print_page);
        self.request_indices(ctx, &indices);
        let mut save_pdf = false;
        let mut do_print = false;
        let mut close = false;
        egui::Window::new(tr(lang, "print_title"))
            .open(&mut open)
            .resizable(true)
            .default_size([560.0, 560.0])
            .show(ctx, |ui| {
                let layouts = [
                    tr(lang, "print_layout_full"),
                    "2 × 2",
                    "3 × 3",
                    "4 × 4",
                ];
                egui::ComboBox::from_id_salt("print_layout")
                    .selected_text(layouts[self.print_layout])
                    .show_ui(ui, |ui| {
                        for (i, l) in layouts.iter().enumerate() {
                            if ui.selectable_value(&mut self.print_layout, i, *l).clicked() {
                                self.print_page = 0;
                            }
                        }
                    });
                let papers = ["A4", "Letter"];
                egui::ComboBox::from_id_salt("print_paper")
                    .selected_text(papers[self.print_paper])
                    .show_ui(ui, |ui| {
                        for (i, p) in papers.iter().enumerate() {
                            ui.selectable_value(&mut self.print_paper, i, *p);
                        }
                    });
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.print_landscape, false, tr(lang, "print_portrait"));
                    ui.radio_value(&mut self.print_landscape, true, tr(lang, "print_landscape"));
                });
                let n = self.listing.as_ref().map_or(0, |l| l.len());
                ui.label(format!("{}: {n}, {}: {total_pages}", tr(lang, "print_images"), tr(lang, "print_pages")));
                // Navigasi halaman pratinjau.
                ui.horizontal(|ui| {
                    if ui.button("◀").clicked() {
                        self.print_page = self.print_page.saturating_sub(1);
                    }
                    ui.label(format!("{} {} / {total_pages}", tr(lang, "print_page"), self.print_page + 1));
                    if ui.button("▶").clicked() {
                        self.print_page = (self.print_page + 1).min(total_pages - 1);
                    }
                });
                // Pratinjau: kertas putih + slot gambar.
                // Sisakan ruang untuk baris tombol di bawah pratinjau.
                let (pw, ph) = self.print_paper_size();
                let avail = ui.available_rect_before_wrap();
                let preview_h = (avail.height() - 44.0).max(80.0);
                let scale = (avail.width() / pw).min(preview_h / ph).max(0.05);
                let preview = Rect::from_min_size(avail.min, vec2(pw * scale, ph * scale));
                let (pr, _) = ui.allocate_exact_size(vec2(preview.width(), preview.height()), Sense::hover());
                let painter = ui.painter_at(pr);
                painter.rect_filled(pr, 2.0, Color32::WHITE);
                let (cols, rows) = self.print_grid();
                let margin = 36.0 * scale;
                let gap = 12.0 * scale;
                let cw = (pr.width() - margin * 2.0 - gap * (cols - 1) as f32) / cols as f32;
                let ch = (pr.height() - margin * 2.0 - gap * (rows - 1) as f32) / rows as f32;
                for (cell, &idx) in indices.iter().enumerate() {
                    let (cx, cy) = (cell % cols, cell / cols);
                    let cell_rect = Rect::from_min_size(
                        pos2(pr.min.x + margin + cx as f32 * (cw + gap), pr.min.y + margin + cy as f32 * (ch + gap)),
                        vec2(cw, ch),
                    );
                    if let Some(page) = self.cache.get(&idx) {
                        let tex = page.tex.id();
                        let (tw, th) = (page.decoded[0] as f32, page.decoded[1] as f32);
                        let s = (cell_rect.width() / tw.max(1.0)).min(cell_rect.height() / th.max(1.0));
                        let r = Rect::from_center_size(cell_rect.center(), vec2(tw * s, th * s));
                        painter.image(tex, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    } else {
                        painter.rect_filled(cell_rect, 2.0, Color32::from_gray(220));
                        painter.rect_stroke(cell_rect, 2.0, egui::Stroke::new(1.0_f32, Color32::from_gray(160)), egui::StrokeKind::Inside);
                        let name = self.listing.as_ref().and_then(|l| l.names.get(idx)).map_or("", String::as_str);
                        painter.text(cell_rect.center(), egui::Align2::CENTER_CENTER, name, egui::FontId::proportional(11.0), Color32::from_gray(90));
                    }
                }
                if !self.print_status.is_empty() {
                    ui.colored_label(Color32::from_rgb(255, 200, 90), &self.print_status);
                }
                let busy = self.print_job.is_some();
                ui.horizontal(|ui| {
                    if ui.add_enabled(!busy, egui::Button::new(tr(lang, "print_save_pdf"))).clicked() {
                        save_pdf = true;
                    }
                    if ui.add_enabled(!busy, egui::Button::new(tr(lang, "print_print"))).clicked() {
                        do_print = true;
                    }
                    if ui.button(tr(lang, "print_close")).clicked() {
                        close = true;
                    }
                });
                if busy {
                    ui.label(tr(lang, "print_generating"));
                }
            });
        self.print_open = open;
        if close {
            self.print_open = false;
        }
        if save_pdf {
            let file_name = self
                .listing
                .as_ref()
                .map(|l| {
                    let stem = l
                        .origin
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "roneyview".to_string());
                    format!("{stem}.pdf")
                })
                .unwrap_or_else(|| "roneyview.pdf".to_string());
            self.start_dialog(ctx, DialogKind::SavePdf { file_name });
        }
        if do_print {
            self.start_pdf_job(ctx, PdfJobKind::Print);
        }
    }

    /// Mulai pekerjaan bangun PDF di thread latar.
    fn start_pdf_job(&mut self, ctx: &Context, kind: PdfJobKind) {
        let lang = self.lang;
        if self.print_job.is_some() {
            return;
        }
        let Some(listing) = self.listing.clone() else {
            return;
        };
        let (layout, paper, landscape) = (self.print_layout, self.print_paper, self.print_landscape);
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::Builder::new()
            .name("roneyview-pdf".into())
            .spawn(move || {
                let res = build_pdf_for_listing(&listing, layout, paper, landscape);
                let _ = tx.send(res);
                // Bangunkan UI agar poll_pdf_job berjalan (app idle tanpa ini).
                repaint.request_repaint();
            })
            .ok();
        self.print_job = Some((rx, kind));
        self.print_status = tr(lang, "print_generating").to_string();
    }

    /// Poll hasil pekerjaan PDF: simpan ke berkas atau kirim ke printer.
    fn poll_pdf_job(&mut self) {
        let lang = self.lang;
        let Some((rx, _)) = self.print_job.as_ref() else {
            return;
        };
        // Satu kali recv: recv ganda akan membuang hasil PDF.
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.print_job.take();
                self.print_status = tr(lang, "print_failed").to_string();
                return;
            }
        };
        let Some((_, kind)) = self.print_job.take() else {
            return;
        };
        match result {
            Ok(pdf) => match kind {
                PdfJobKind::Save(dest) => match std::fs::write(&dest, &pdf) {
                    Ok(_) => {
                        self.print_status.clear();
                        self.notify(tr_fmt(lang, "notify_saved", &[&dest.display()]));
                    }
                    Err(e) => self.print_status = format!("{}: {e}", tr(lang, "notify_save_failed")),
                },
                PdfJobKind::Print => match print_pdf_lpr(&pdf) {
                    Ok(_) => {
                        self.print_status.clear();
                        self.notify(tr(lang, "print_sent"));
                    }
                    Err(e) => self.print_status = e,
                },
            },
            Err(e) => self.print_status = e,
        }
    }

    /// Bangun PDF dari daftar, lalu simpan ke `dest` (via pekerjaan latar).
    fn build_and_save_pdf(&mut self, ctx: &Context, dest: &Path) {
        // Pekerjaan latar mengambil daftar saat ini; tujuan disimpan di kind.
        self.start_pdf_job(ctx, PdfJobKind::Save(dest.to_path_buf()));
    }

    // -------------------------------------------------------------- proyektor

    /// Dekode piksel halaman untuk proyektor (maks ~2MP, orientasi diterapkan).
    fn decode_projector_pixels(&self, index: usize) -> Option<ColorImage> {
        let listing = self.listing.clone()?;
        let mut reader = listing.open_reader().ok()?;
        let bytes = reader.read(index).ok()?;
        use image::metadata::Orientation;
        use image::{DynamicImage, ImageReader};
        let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
        let mut decoder = reader.into_decoder().ok()?;
        let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
        let mut img = DynamicImage::from_decoder(decoder).ok()?;
        img.apply_orientation(orientation);
        let (w, h) = (img.width(), img.height());
        let scale = (2048.0 / w.max(h) as f32).min(1.0);
        let (tw, th) = ((w as f32 * scale).round() as u32, (h as f32 * scale).round() as u32);
        let rgba = if scale < 1.0 {
            img.resize(tw.max(1), th.max(1), image::imageops::FilterType::Triangle).to_rgba8()
        } else {
            img.to_rgba8()
        };
        Some(ColorImage::from_rgba_unmultiplied(
            [rgba.width() as usize, rgba.height() as usize],
            rgba.as_raw(),
        ))
    }

    /// Jendela proyektor (viewport kedua): gambar saat ini + corat-coret sementara.
    fn draw_projector(&mut self, ctx: &Context) {
        let lang = self.lang;
        let index = self.index;
        let session = self.session;
        let key = (session, index, self.anim_frame);
        // Decode hanya bila gambar/frame berubah; sebelumnya decode full tiap frame.
        let need_pixels = self.projector_key != Some(key);
        let pixels: Option<ColorImage> = if need_pixels {
            self.page_for(index).and_then(|page| {
                if let Some(anim) = &page.anim {
                    anim.frames.get(self.anim_frame).map(|f| (*f.image).clone())
                } else {
                    self.decode_projector_pixels(index)
                }
            })
        } else {
            None
        };
        let mut closed = false;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("roneyview-projector"),
            egui::ViewportBuilder::default()
                .with_title(tr(lang, "projector_title"))
                .with_inner_size([960.0, 600.0]),
            |pctx, _class| {
                if pctx.input(|i| i.viewport().close_requested()) {
                    closed = true;
                    return;
                }
                // Shortcut subtool projector: P/E/W/V/C (abaikan saat mengetik).
                let (sc_pen, sc_eraser, sc_color, sc_width, sc_clear) =
                    if pctx.wants_keyboard_input() {
                        (false, false, false, false, false)
                    } else {
                        pctx.input_mut(|i| {
                            let none = Modifiers::NONE;
                            (
                                i.consume_key(none, Key::P),
                                i.consume_key(none, Key::E),
                                i.consume_key(none, Key::W),
                                i.consume_key(none, Key::V),
                                i.consume_key(none, Key::C),
                            )
                        })
                    };
                if sc_pen {
                    self.projector_eraser = false;
                }
                if sc_eraser {
                    self.projector_eraser = true;
                }
                if sc_clear {
                    self.projector_strokes.clear();
                }
                if self.projector_key != Some(key) {
                    self.projector_key = Some(key);
                    self.projector_tex = pixels.as_ref().map(|ci| {
                        pctx.load_texture("projector", ci.clone(), TextureOptions::LINEAR)
                    });
                    // Ganti gambar -> coretan sementara ikut hilang.
                    self.projector_strokes.clear();
                }
                // Bilah alat corat-coret.
                egui::TopBottomPanel::top("projector_tools").show(pctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(!self.projector_eraser, tr(lang, "projector_pen"))
                            .on_hover_text(tr(lang, "projector_pen_tip"))
                            .clicked()
                            || sc_pen
                        {
                            self.projector_eraser = false;
                        }
                        if ui
                            .selectable_label(self.projector_eraser, tr(lang, "projector_eraser"))
                            .on_hover_text(tr(lang, "projector_eraser_tip"))
                            .clicked()
                            || sc_eraser
                        {
                            self.projector_eraser = true;
                        }
                        ui.separator();
                        // Tombol warna custom (ID eksplisit) agar shortcut W bisa
                        // membuka/menutup popup color picker.
                        let color_id = ui.make_persistent_id("projector_pen_color");
                        let (crect, cresp) =
                            ui.allocate_exact_size(vec2(30.0, 22.0), Sense::click());
                        {
                            let p = ui.painter();
                            p.rect_filled(crect, 4.0, self.projector_pen_color);
                            p.rect_stroke(
                                crect,
                                4.0,
                                egui::Stroke::new(1.0_f32, Color32::from_gray(120)),
                                egui::StrokeKind::Middle,
                            );
                        }
                        let cresp = cresp.on_hover_text(tr(lang, "projector_color_tip"));
                        egui::Popup::from_response(&cresp)
                            .id(color_id.with("popup"))
                            .open_memory(
                                (cresp.clicked() || sc_color)
                                    .then_some(egui::SetOpenCommand::Toggle),
                            )
                            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                            .show(|ui| {
                                egui::color_picker::color_picker_color32(
                                    ui,
                                    &mut self.projector_pen_color,
                                    egui::color_picker::Alpha::Opaque,
                                );
                            });
                        let wresp = ui
                            .add(
                                egui::Slider::new(&mut self.projector_pen_width, 1.0..=24.0)
                                    .text(tr(lang, "projector_width"))
                                    .max_decimals(0),
                            )
                            .on_hover_text(tr(lang, "projector_width_tip"));
                        if sc_width {
                            wresp.request_focus();
                        }
                        ui.separator();
                        if ui
                            .button(tr(lang, "projector_clear"))
                            .on_hover_text(tr(lang, "projector_clear_tip"))
                            .clicked()
                            || sc_clear
                        {
                            self.projector_strokes.clear();
                        }
                    });
                });
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(Color32::BLACK))
                    .show(pctx, |ui| {
                        let Some(tex) = &self.projector_tex else {
                            return;
                        };
                        let avail = ui.available_rect_before_wrap();
                        let (tw, th) = (tex.size()[0] as f32, tex.size()[1] as f32);
                        let scale = fit_scale(FitMode::FitUpscale, vec2(tw, th), avail.size());
                        let rect =
                            Rect::from_center_size(avail.center(), vec2(tw * scale, th * scale));
                        paint_page(&ui.painter_at(avail), tex.id(), rect, 0);
                        // Interaksi corat-coret di atas gambar.
                        let resp = ui.interact(
                            rect,
                            ui.id().with("projector_canvas"),
                            egui::Sense::drag(),
                        );
                        let to_tex =
                            |pos: Pos2| ((pos.x - rect.min.x) / scale, (pos.y - rect.min.y) / scale);
                        if resp.drag_started() && !self.projector_eraser {
                            if let Some(pos) = resp.interact_pointer_pos() {
                                let (tx, ty) = to_tex(pos);
                                // Abaikan bila di luar gambar.
                                if tx >= 0.0 && ty >= 0.0 && tx < tw && ty < th {
                                    self.projector_strokes.push(ProjStroke {
                                        color: self.projector_pen_color,
                                        width: self.projector_pen_width,
                                        pts: vec![(tx, ty)],
                                    });
                                }
                            }
                        }
                        if resp.dragged() {
                            if let Some(pos) = resp.interact_pointer_pos() {
                                let (tx, ty) = to_tex(pos);
                                if self.projector_eraser {
                                    // Penghapus: buang goresan yang tersentuh.
                                    let r = self.projector_pen_width.max(8.0);
                                    let r2 = r * r;
                                    self.projector_strokes.retain(|s| {
                                        !s.pts.iter().any(|&(qx, qy)| {
                                            let (dx, dy) = (qx - tx, qy - ty);
                                            dx * dx + dy * dy < r2
                                        })
                                    });
                                } else if let Some(st) = self.projector_strokes.last_mut() {
                                    // Tambah titik bila cukup jauh dari titik terakhir.
                                    let add = match st.pts.last() {
                                        Some(&(lx, ly)) => {
                                            let (dx, dy) = (tx - lx, ty - ly);
                                            dx * dx + dy * dy > 4.0
                                        }
                                        None => true,
                                    };
                                    if add && tx >= 0.0 && ty >= 0.0 && tx < tw && ty < th {
                                        st.pts.push((tx, ty));
                                    }
                                }
                            }
                        }
                        // Gambar semua goresan: satu polyline per stroke (jauh lebih
                        // ringan daripada ratusan line_segment + circle per frame).
                        let painter = ui.painter_at(avail);
                        for st in &self.projector_strokes {
                            let wpx = (st.width * scale).max(1.0);
                            let pts: Vec<Pos2> = st
                                .pts
                                .iter()
                                .map(|&(x, y)| pos2(rect.min.x + x * scale, rect.min.y + y * scale))
                                .collect();
                            if pts.len() == 1 {
                                // Titik tunggal: gambar sebagai lingkaran.
                                painter.circle_filled(pts[0], wpx / 2.0, st.color);
                            } else if pts.len() > 1 {
                                let stroke = egui::Stroke::new(wpx, st.color);
                                painter.add(egui::epaint::PathShape::line(pts.clone(), stroke));
                                // Ujung bulat: lingkaran kecil di awal/akhir saja.
                                painter.circle_filled(pts[0], wpx / 2.0, st.color);
                                if let Some(&last) = pts.last() {
                                    painter.circle_filled(last, wpx / 2.0, st.color);
                                }
                            }
                        }
                        // Kursor.
                        if resp.hovered() {
                            resp.ctx.set_cursor_icon(if self.projector_eraser {
                                egui::CursorIcon::Cell
                            } else {
                                egui::CursorIcon::Crosshair
                            });
                        }
                    });
                pctx.request_repaint_after(Duration::from_millis(250));
            },
        );
        if closed {
            self.projector_open = false;
            self.projector_tex = None;
            self.projector_key = None;
            self.projector_strokes.clear();
        }
    }

    // --------------------------------------------------------------- pemuatan

    fn max_side(ctx: &Context) -> usize {
        ctx.input(|i| i.max_texture_side)
    }

    fn after_page_change(&mut self, ctx: &Context) {
        if self.listing.is_none() {
            return;
        }
        let (lo, hi) = keep_window(self.index, self.low_memory_mode, self.two_page_mode);
        self.cache.retain(|i, _| (lo..=hi).contains(i));
        self.request_pages(ctx);
        self.update_title(ctx);
        ctx.request_repaint();
    }

    /// Buang halaman terjauh dari gambar aktif sampai total ukuran cache muat
    /// dalam anggaran. Gambar yang sedang dilihat tidak pernah dibuang.
    fn enforce_budget(&mut self) {
        loop {
            let total: usize = self.cache.values().map(|p| p.bytes).sum();
            if total <= self.budget {
                return;
            }
            let victim = self
                .cache
                .keys()
                .copied()
                .filter(|i| *i != self.index)
                .max_by_key(|i| i.abs_diff(self.index));
            match victim {
                Some(v) => {
                    self.cache.remove(&v);
                }
                None => return,
            }
        }
    }

    fn request_pages(&mut self, ctx: &Context) {
        let lang = self.lang;
        let Some(l) = self.listing.clone() else {
            return;
        };
        let n = l.len() as isize;
        let max_side = Self::max_side(ctx);
        // Resolusi decode mengikuti zoom saat ini: fit cukup 2MP (jauh lebih
        // ringan), zoom >= 7x naik ke 4MP via refine otomatis.
        let max_pixels = tier_pixels(self.display_scale).min(loader::max_pixels());
        let cur = self.index as isize;
        let (a, b) = if self.dir >= 0 { (1, -1) } else { (-1, 1) };
        let mut wanted: Vec<(usize, bool)> = vec![(self.index, true)];
        let spread = self.spread_wanted(self.index);
        if spread {
            wanted.push((self.index + 1, false)); // pasangan harus siap bersama
        }

        // Pengaman: saat RAM sistem hampir habis, lepaskan cache dan matikan prefetch
        // supaya Roneyview tidak ikut menyeret sistem ke OOM / swap-thrash.
        let system_low = loader::available_now().is_some_and(|b| b < loader::LOW_MEMORY);
        if system_low {
            let partner = self.index + usize::from(spread);
            self.cache.retain(|i, _| *i == self.index || *i == partner);
            if !self.low_mem_notified {
                self.low_mem_notified = true;
                self.notify(tr(lang, "notify_lowmem"));
            }
        } else if self.low_memory_mode {
            self.low_mem_notified = false; // mode hemat: hanya halaman aktif, tanpa prefetch
        } else {
            self.low_mem_notified = false;
            let offsets: [isize; 3] = if !self.two_page_mode {
                [a, 2 * a, b]
            } else if self.dir >= 0 {
                [2, 3, -1]
            } else {
                [-1, -2, 2]
            };
            for off in offsets {
                let i = cur + off;
                if i >= 0 && i < n {
                    wanted.push((i as usize, false));
                }
            }
        }
        for (i, primary) in wanted {
            if self.cache.contains_key(&i) || self.errors.contains_key(&i) {
                continue;
            }
            if let Some(&was_primary) = self.pending.get(&i) {
                if was_primary || !primary {
                    continue;
                }
            }
            self.pending.insert(i, primary);
            // Worker memakai anggaran untuk melewatkan prefetch yang hasilnya
            // pasti dibuang (decode-nya bisa makan 8 detik CPU dengan sia-sia).
            let held: usize = self.cache.values().map(|p| p.bytes).sum();
            let cmd = Command::Load {
                session: self.session,
                index: i,
                primary,
                max_side,
                max_pixels,
                focus: self.index,
                budget: self.budget,
                held,
            };
            // Permintaan utama ke worker utama; prefetch ke worker prefetch.
            // Bila suatu indeks naik jadi permintaan utama, batalkan salinan
            // prefetch-nya yang mungkin masih antre di worker prefetch supaya
            // tidak didekode dua kali.
            let ok = if primary {
                let _ = self.cmd_tx_pref.send(Command::Forget {
                    session: self.session,
                    index: i,
                });
                self.cmd_tx_main.send(cmd).is_ok()
            } else {
                self.cmd_tx_pref.send(cmd).is_ok()
            };
            if !ok {
                self.pending.remove(&i);
            }
        }
    }

    /// Animasi tanpa mipmap: membangun ulang mipmap di setiap frame terlalu mahal.
    fn texture_options(&self, animated: bool) -> TextureOptions {
        TextureOptions {
            magnification: egui::TextureFilter::Linear,
            minification: egui::TextureFilter::Linear,
            wrap_mode: egui::TextureWrapMode::ClampToEdge,
            mipmap_mode: (self.mipmaps && !animated).then_some(egui::TextureFilter::Linear),
        }
    }

    fn poll_loader(&mut self, ctx: &Context) {
        let mut got_current = false;
        while let Ok(msg) = self.out_rx.try_recv() {
            if msg.session != self.session {
                continue;
            }
            self.pending.remove(&msg.index);
            match msg.outcome {
                Outcome::Ready(d) => {
                    let (lo, hi) = keep_window(self.index, self.low_memory_mode, self.two_page_mode);
                    if !(lo..=hi).contains(&msg.index) {
                        // Buang juga pratinjaunya kalau sempat masuk: kalau tidak,
                        // gambar blur basi bisa tampil selamanya tanpa versi tajam.
                        self.cache.remove(&msg.index);
                        continue; // sudah terlalu jauh, buang
                    }
                    let px = d.pixels;
                    self.dims.insert(msg.index, px.orig);
                    let format = px.format.clone();
                    let animated = px.frames.is_some();
                    // Mipmap menambah ~1/3 memori GPU.
                    let mip_factor = |b: usize| if self.mipmaps && !animated { b * 4 / 3 } else { b };
                    let mut bytes = mip_factor(px.image.size[0] * px.image.size[1] * 4);
                    let anim = px.frames.map(|frames| {
                        // frames[0] berbagi data dengan `image`; hitung sisanya.
                        bytes = frames
                            .iter()
                            .map(|f| f.image.size[0] * f.image.size[1] * 4)
                            .sum();
                        Anim {
                            frames,
                            uploaded: AtomicUsize::new(0),
                            truncated: px.truncated,
                        }
                    });
                    if msg.index != self.index {
                        let held: usize = self.cache.values().map(|p| p.bytes).sum();
                        if held + bytes > self.budget {
                            continue; // prefetch tidak muat: jangan alokasikan tekstur GPU
                        }
                    }
                    let decoded = [px.image.size[0] as u32, px.image.size[1] as u32];
                    let tex = ctx.load_texture(
                        format!("pg{}-{}", msg.session, msg.index),
                        px.image,
                        self.texture_options(animated),
                    );
                    let page = Arc::new(Page {
                        tex,
                        orig: px.orig,
                        decoded,
                        file_size: d.file_size,
                        bytes,
                        anim,
                        format,
                    });
                    got_current |= msg.index == self.index;
                    self.cache.insert(msg.index, page);
                    self.enforce_budget();
                }
                Outcome::Failed(e) => {
                    self.errors.insert(msg.index, e);
                    got_current |= msg.index == self.index;
                }
                Outcome::Skipped => {}
            }
        }
        if got_current {
            ctx.request_repaint();
        }
    }

    // -------------------------------------------------------------- navigasi

    fn count(&self) -> usize {
        self.listing.as_ref().map_or(0, |l| l.len())
    }

    fn is_wide(&self, i: usize) -> bool {
        self.dims.get(&i).is_some_and(|d| d[0] > d[1])
    }

    /// Apakah halaman `s` dan `s+1` perlu ditampilkan berdampingan (belum memeriksa lebar).
    fn spread_wanted(&self, s: usize) -> bool {
        self.two_page_mode && !(self.cover_alone && s == 0) && s + 1 < self.count()
    }

    /// Banyak halaman yang sedang tampil (untuk lompat ke spread berikutnya).
    fn next_span(&self) -> usize {
        match &self.shown {
            Some(sh) if sh.start == self.index && sh.slots.len() == 2 => 2,
            _ => 1,
        }
    }

    fn snap_start(&self, t: usize) -> usize {
        snap_start_for(t, self.two_page_mode, self.cover_alone)
    }

    fn go_next(&mut self, ctx: &Context) {
        let lang = self.lang;
        let n = self.count();
        if n == 0 {
            return;
        }
        let target = self.index + self.next_span();
        if target >= n {
            self.notify(tr(lang, "notify_last"));
        } else {
            self.go_to(ctx, target, false);
        }
    }

    fn go_prev(&mut self, ctx: &Context, land_bottom: bool) {
        let lang = self.lang;
        let wide = |i: usize| self.is_wide(i);
        match prev_start_for(self.index, self.two_page_mode, self.cover_alone, &wide) {
            None => self.notify(tr(lang, "notify_first")),
            Some(t) => self.go_to(ctx, t, land_bottom),
        }
    }

    /// Apakah gambar saat ini masih dalam proses loading.
    fn is_loading(&self) -> bool {
        self.listing.is_some()
            && !self.cache.contains_key(&self.index)
            && !self.errors.contains_key(&self.index)
    }

    /// Catat posisi baca saat ini untuk folder/arsip yang sedang dibuka.
    fn record_position(&mut self) {
        if let Some(key) = self.position_key.clone() {
            self.positions.set(key, self.index);
            self.positions_dirty = true;
        }
    }

    fn go_to(&mut self, ctx: &Context, target: usize, land_bottom: bool) {
        if target >= self.count() || target == self.index {
            return;
        }
        // Kunci navigasi selama gambar saat ini masih loading, supaya tidak
        // loncat ke 3.jpg/4.jpg padahal 2.jpg belum tampil.
        if self.is_loading() {
            return;
        }
        self.dir = if target > self.index { 1 } else { -1 };
        self.index = target;
        self.rotation = 0;
        self.zoom = Zoom::Fit(self.fit);
        self.land_bottom = land_bottom;
        self.record_position();
        self.after_page_change(ctx);
    }

    /// Buka jendela "Lompat ke halaman".
    fn open_goto(&mut self) {
        if self.count() == 0 {
            return;
        }
        self.goto_open = true;
        self.goto_input.clear();
        self.goto_focus = true;
    }

    /// Proses masukan dialog "Lompat ke halaman".
    fn confirm_goto(&mut self, ctx: &Context) {
        let lang = self.lang;
        let n = self.count();
        match parse_page_number(&self.goto_input, n) {
            Some(t) => {
                let t = if self.two_page_mode { self.snap_start(t) } else { t };
                self.goto_open = false;
                self.goto_input.clear();
                self.go_to(ctx, t, false);
            }
            None => self.notify(tr(lang, "goto_invalid")),
        }
    }

    fn update_title(&mut self, ctx: &Context) {
        let Some(l) = &self.listing else {
            return;
        };
        let first = l.names.get(self.index).map_or("", String::as_str);
        let pair = self
            .shown
            .as_ref()
            .filter(|sh| sh.start == self.index && sh.slots.len() == 2)
            .map(|sh| sh.slots[1].0);
        let (name, pos) = match pair {
            Some(j) => (
                format!("{first} + {}", l.names.get(j).map_or("", String::as_str)),
                format!("{}-{}/{}", self.index + 1, j + 1, l.len()),
            ),
            None => (first.to_string(), format!("{}/{}", self.index + 1, l.len())),
        };
        let title = if l.is_archive() {
            let arc = l
                .origin
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            format!("{name} [{arc}] ({pos}) - Roneyview")
        } else {
            format!("{name} ({pos}) - Roneyview")
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    // ------------------------------------------------------------ geometri

    fn page_dims(&self, page: &Page, ppp: f32) -> Vec2 {
        let unit = 1.0 / ppp;
        let [w, h] = page.orig;
        let (w, h) = (w as f32 * unit, h as f32 * unit);
        if self.rotation % 2 == 1 {
            vec2(h, w)
        } else {
            vec2(w, h)
        }
    }

    /// Ukuran (poin) isi yang tampil: satu halaman, atau dua halaman dengan tinggi disamakan.
    fn shown_dims(&self, ctx: &Context, sh: &Shown) -> Vec2 {
        let ppp = ctx.pixels_per_point();
        if sh.slots.len() == 1 {
            return self.page_dims(&sh.slots[0].1, ppp);
        }
        let u = 1.0 / ppp;
        let dims: Vec<(f32, f32)> = sh
            .slots
            .iter()
            .map(|(_, p)| (p.orig[0] as f32 * u, p.orig[1] as f32 * u))
            .collect();
        let (w, h) = pair_widths(&dims);
        vec2(w.iter().sum(), h)
    }

    /// Persegi tiap halaman di dalam `composite`, dalam urutan visual kiri ke kanan.
    /// Arah baca kanan-ke-kiri menaruh halaman pertama di sebelah kanan.
    fn pair_rects(&self, ctx: &Context, sh: &Shown, composite: Rect) -> Vec<(usize, Rect)> {
        let u = 1.0 / ctx.pixels_per_point();
        let dims: Vec<(f32, f32)> = sh
            .slots
            .iter()
            .map(|(_, p)| (p.orig[0] as f32 * u, p.orig[1] as f32 * u))
            .collect();
        let (widths, _) = pair_widths(&dims);
        let total: f32 = widths.iter().sum::<f32>().max(1.0);
        let k = composite.width() / total;
        let mut order: Vec<usize> = (0..sh.slots.len()).collect();
        if self.rtl {
            order.reverse();
        }
        let mut x = composite.left();
        let mut out = Vec::with_capacity(order.len());
        for i in order {
            let w = widths[i] * k;
            out.push((
                sh.slots[i].0,
                Rect::from_min_size(pos2(x, composite.top()), vec2(w, composite.height())),
            ));
            x += w;
        }
        out
    }

    fn geometry(&self, ctx: &Context, dims: Vec2, view: Rect) -> Geo {
        let _ = ctx;
        let scale = match self.zoom {
            Zoom::Custom(z) => z,
            Zoom::Fit(m) => fit_scale(m, dims, view.size()),
        }
        .clamp(MIN_SCALE, self.max_zoom_scale(dims, view));
        let size = dims * scale;
        Geo {
            scale,
            size,
            offset: clamp_offset(self.offset, size, view.size()),
        }
    }

    /// Batas zoom: maksimal 8 langkah (1.25x) dari ukuran fit.
    /// Di atas itu layar bisa hitam di GPU lama.
    fn max_zoom_scale(&self, dims: Vec2, view: Rect) -> f32 {
        let fit = fit_scale(self.fit, dims, view.size());
        (fit * ZOOM_STEP.powi(MAX_ZOOM_STEPS)).max(MIN_SCALE)
    }

    /// Ganti bahasa UI dan simpan ke pengaturan.
    fn set_lang(&mut self, lang: Lang) {
        if self.lang != lang {
            self.lang = lang;
            self.store.state.lang = lang.code().to_string();
            self.store_dirty = true;
        }
    }

    fn zoom_by(&mut self, ctx: &Context, factor: f32, anchor: Option<Pos2>) {
        let Some(sh) = self.shown.clone() else {
            return;
        };
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let view = self.view_rect;
        let dims = self.shown_dims(ctx, &sh);
        let geo = self.geometry(ctx, dims, view);
        let max = self.max_zoom_scale(dims, view);
        let new_scale = (geo.scale * factor).clamp(MIN_SCALE, max);
        if (new_scale - geo.scale).abs() < 1e-6 {
            return;
        }
        let anchor = anchor.unwrap_or_else(|| view.center());
        let center = view.center() + geo.offset;
        let new_center = anchor - (anchor - center) * (new_scale / geo.scale);
        self.offset = new_center - view.center();
        self.zoom = Zoom::Custom(new_scale);
        // Naik tier resolusi -> decode ulang lebih tajam di background;
        // tampilan lama tetap dipakai sampai yang baru siap.
        if tier_pixels(new_scale) > tier_pixels(geo.scale) {
            self.refine_current(ctx, tier_pixels(new_scale));
        }
    }

    /// Decode ulang halaman aktif pada tier resolusi yang lebih tinggi
    /// (mis. pengguna zoom >= 7x). Berjalan di background; hasilnya menggantikan
    /// cache saat siap. Hanya naik tier, tidak pernah turun (bolak-balik zoom
    /// tidak memicu decode berulang).
    fn refine_current(&mut self, ctx: &Context, tier_px: usize) {
        let s = self.index;
        if self.pending.contains_key(&s) {
            return; // sudah ada permintaan berjalan untuk halaman ini
        }
        let Some(page) = self.cache.get(&s) else {
            return;
        };
        let decoded_px = page.decoded[0] as usize * page.decoded[1] as usize;
        if decoded_px * 10 >= tier_px * 9 {
            return; // sudah cukup tajam
        }
        let orig_px = page.orig[0] as usize * page.orig[1] as usize;
        if orig_px <= tier_px {
            return; // gambar aslinya memang sekecil ini
        }
        let max_side = Self::max_side(ctx);
        let max_pixels = tier_px.min(loader::max_pixels());
        let held: usize = self.cache.values().map(|p| p.bytes).sum();
        self.pending.insert(s, true);
        let cmd = Command::Load {
            session: self.session,
            index: s,
            primary: true,
            max_side,
            max_pixels,
            focus: s,
            budget: self.budget,
            held,
        };
        if self.cmd_tx_main.send(cmd).is_err() {
            self.pending.remove(&s);
        }
    }

    /// Tentukan isi yang tampil untuk halaman aktif. Bila pasangan masih dimuat, isi
    /// sebelumnya dipertahankan supaya layar tidak berkedip.
    fn current_content(&mut self) -> Option<Arc<Shown>> {
        let s = self.index;
        match self.cache.get(&s).cloned() {
            None => {
                if self.errors.contains_key(&s) {
                    self.shown = None;
                }
            }
            Some(p0) => {
                let single = |p: Arc<Page>| Some(Arc::new(Shown { start: s, slots: vec![(s, p)] }));
                let wide = |p: &Page| p.orig[0] > p.orig[1];
                if !self.spread_wanted(s) || wide(&p0) {
                    self.shown = single(p0);
                } else {
                    match self.cache.get(&(s + 1)).cloned() {
                        Some(p1) if !wide(&p1) => {
                            self.shown = Some(Arc::new(Shown {
                                start: s,
                                slots: vec![(s, p0), (s + 1, p1)],
                            }));
                        }
                        Some(_) => self.shown = single(p0),
                        None => {
                            if self.errors.contains_key(&(s + 1)) {
                                self.shown = single(p0);
                            } // else: pasangan sedang dimuat, pertahankan isi sebelumnya
                        }
                    }
                }
            }
        }
        self.shown.clone()
    }

    // ------------------------------------------------------------- aksi

    fn perform(&mut self, ctx: &Context, action: Action) {
        match action {
            Action::Next => self.go_next(ctx),
            Action::Prev => self.go_prev(ctx, false),
            Action::First => self.go_to(ctx, 0, false),
            Action::Last => {
                let n = self.count();
                if n > 0 {
                    let t = self.snap_start(n - 1);
                    self.go_to(ctx, t, false);
                }
            }
            Action::OpenFile => self.start_dialog(ctx, DialogKind::File),
            Action::OpenFolder => self.start_dialog(ctx, DialogKind::Folder),
            Action::GoToPage => self.open_goto(),
            Action::ExtractPage => {
                let idx = self.index;
                self.start_extract_dialog(ctx, idx);
            }
            Action::Trash => self.confirm_trash(self.index),
            Action::FramePrev => self.step_frame(ctx, -1),
            Action::FrameNext => self.step_frame(ctx, 1),
            Action::ToggleToolbar => {
                self.store.state.show_toolbar = !self.store.state.show_toolbar;
                self.store_dirty = true;
            }
            Action::OpenRecent(i) => {
                if let Some(p) = self.store.state.recent.get(i).cloned() {
                    self.open_path(ctx, Path::new(&p));
                }
            }
            Action::Fit(m) => {
                self.fit = m;
                self.zoom = Zoom::Fit(m);
                self.placed_for = None;
                self.store.state.fit = m;
                self.store_dirty = true;
            }
            Action::ZoomIn => self.zoom_by(ctx, ZOOM_STEP, None),
            Action::ZoomOut => self.zoom_by(ctx, 1.0 / ZOOM_STEP, None),
            Action::RotateCw => self.rotate_checked(1),
            Action::RotateCcw => self.rotate_checked(3),
            Action::PanY(dy) => self.offset.y += dy,
            Action::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
            }
            Action::ExitFullscreen => {
                if self.fullscreen {
                    self.fullscreen = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
            }
            Action::TogglePlay => {
                self.anim_playing = !self.anim_playing;
                if self.anim_playing {
                    // Lanjutkan dengan jeda penuh frame yang sedang tampil.
                    self.anim_next = Instant::now();
                }
            }
            Action::ToggleBarLock => self.set_bar_lock(!self.bar_locked),
            Action::ToggleTwoPage => self.set_two_page(ctx, !self.two_page_mode),
            Action::ToggleRtl => self.set_rtl(ctx, !self.rtl),
            Action::ToggleCover => self.set_cover(ctx, !self.cover_alone),
            Action::ToggleLowMemory => self.set_low_memory(ctx, !self.low_memory_mode),
            Action::Help => self.show_help = !self.show_help,
            Action::About => self.about_open = true,
            Action::Settings => self.show_settings = true,
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
        ctx.request_repaint();
    }

    fn rotate_checked(&mut self, quarter_turns: u8) {
        let lang = self.lang;
        if self.shown.as_ref().is_some_and(|s| s.slots.len() == 2) {
            self.notify(tr(lang, "notify_no_rotate"));
        } else {
            self.rotate(quarter_turns);
        }
    }

    fn rotate(&mut self, quarter_turns: u8) {
        self.rotation = (self.rotation + quarter_turns) % 4;
        // Pertahankan mode zoom, tetapi tempatkan ulang gambar.
        if let Zoom::Custom(_) = self.zoom {
            self.zoom = Zoom::Fit(self.fit);
        }
        self.placed_for = None;
    }

    fn read_input(&self, ctx: &Context) -> FrameInput {
        let fullscreen = self.fullscreen;
        let rtl = self.rtl;
        let goto_open = self.goto_open;
        let modal_open = self.modal.is_some();
        ctx.input_mut(|i| {
            let mut out = FrameInput {
                zoom: 1.0,
                ..Default::default()
            };
            let none = Modifiers::NONE;
            let ctrl = Modifiers::CTRL;
            let shift = Modifiers::SHIFT;
            {
                let mut take = |m: Modifiers, k: Key, a: Action, i: &mut egui::InputState| {
                    if i.consume_key(m, k) {
                        out.actions.push(a);
                    }
                };
                take(ctrl | shift, Key::O, Action::OpenFolder, i);
                take(ctrl, Key::O, Action::OpenFile, i);
                take(ctrl, Key::Q, Action::Quit, i);
                take(ctrl, Key::Comma, Action::Settings, i);
                take(ctrl, Key::G, Action::GoToPage, i);
                take(shift, Key::R, Action::RotateCcw, i);
                take(none, Key::R, Action::RotateCw, i);
                take(shift, Key::Space, Action::Prev, i);
                take(none, Key::Space, Action::Next, i);
                take(none, Key::ArrowRight, if rtl { Action::Prev } else { Action::Next }, i);
                take(none, Key::PageDown, Action::Next, i);
                take(none, Key::ArrowLeft, if rtl { Action::Next } else { Action::Prev }, i);
                take(none, Key::PageUp, Action::Prev, i);
                take(none, Key::Backspace, Action::Prev, i);
                take(none, Key::Home, Action::First, i);
                take(none, Key::End, Action::Last, i);
                take(none, Key::ArrowUp, Action::PanY(PAN_STEP), i);
                take(none, Key::ArrowDown, Action::PanY(-PAN_STEP), i);
                take(none, Key::Plus, Action::ZoomIn, i);
                take(none, Key::Equals, Action::ZoomIn, i);
                take(none, Key::Minus, Action::ZoomOut, i);
                take(none, Key::Num0, Action::Fit(FitMode::Original), i);
                take(none, Key::Num1, Action::Fit(FitMode::Original), i);
                take(shift, Key::F, Action::Fit(FitMode::FitUpscale), i);
                take(none, Key::F, Action::Fit(FitMode::Fit), i);
                take(none, Key::W, Action::Fit(FitMode::Width), i);
                take(none, Key::H, Action::Fit(FitMode::Height), i);
                take(none, Key::P, Action::TogglePlay, i);
                take(none, Key::Comma, Action::FramePrev, i);
                take(none, Key::Period, Action::FrameNext, i);
                take(none, Key::L, Action::ToggleBarLock, i);
                take(none, Key::D, Action::ToggleTwoPage, i);
                take(none, Key::K, Action::ToggleRtl, i);
                take(none, Key::M, Action::ToggleLowMemory, i);
                take(none, Key::T, Action::ToggleToolbar, i);
                // Enter jangan dicaplok saat dialog "Lompat ke halaman" atau
                // kotak konfirmasi/modal terbuka: Enter dipakai untuk konfirmasi di sana.
                if !goto_open && !modal_open {
                    take(none, Key::Enter, Action::ToggleFullscreen, i);
                }
                take(none, Key::Delete, Action::Trash, i);
                take(none, Key::F11, Action::ToggleFullscreen, i);
                take(none, Key::F1, Action::Help, i);
                if fullscreen {
                    take(none, Key::Escape, Action::ExitFullscreen, i);
                }
            }

            for ev in &i.events {
                if let egui::Event::MouseWheel {
                    unit,
                    delta,
                    modifiers,
                } = ev
                {
                    if modifiers.ctrl || modifiers.command {
                        continue; // ditangani sebagai zoom_delta
                    }
                    out.wheel += match unit {
                        egui::MouseWheelUnit::Point => delta.y,
                        egui::MouseWheelUnit::Line => delta.y * WHEEL_LINE_POINTS,
                        egui::MouseWheelUnit::Page => delta.y * 600.0,
                    };
                }
            }
            out.zoom = i.zoom_delta();
            out.hovering_files = !i.raw.hovered_files.is_empty();
            out.dropped = i
                .raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect();
            out
        })
    }

    fn wheel_flip(&mut self, ctx: &Context, delta: f32) {
        let now = Instant::now();
        if now < self.flip_block_until {
            self.flip_accum = 0.0;
            return;
        }
        if self.flip_accum != 0.0 && self.flip_accum.signum() != delta.signum() {
            self.flip_accum = 0.0;
        }
        self.flip_accum += delta;
        if self.flip_accum.abs() >= FLIP_THRESHOLD {
            let forward = self.flip_accum < 0.0;
            self.flip_accum = 0.0;
            self.flip_block_until = now + Duration::from_millis(220);
            if forward {
                self.go_next(ctx);
            } else {
                self.go_prev(ctx, true);
            }
        }
    }

    // ------------------------------------------------------------ gambar

    fn menu_bar(&self, ui: &mut egui::Ui, acts: &mut Vec<Action>) {
        let lang = self.lang;
        egui::MenuBar::new().ui(ui, |ui| {
            let item = |ui: &mut egui::Ui, text: &str, short: &str, acts: &mut Vec<Action>, a: Action| {
                let mut b = egui::Button::new(text);
                if !short.is_empty() {
                    b = b.shortcut_text(short);
                }
                if ui.add(b).clicked() {
                    acts.push(a);
                }
            };
            ui.menu_button(tr(lang, "menu_file"), |ui| {
                item(ui, tr(lang, "menu_open_file"), "Ctrl+O", acts, Action::OpenFile);
                item(ui, tr(lang, "menu_open_folder"), "Ctrl+Shift+O", acts, Action::OpenFolder);
                if self.store.state.recent_folders && !self.store.state.recent.is_empty() {
                    ui.menu_button(tr(lang, "menu_recent"), |ui| {
                        for (i, r) in self.store.state.recent.iter().enumerate() {
                            let label = Path::new(r)
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| r.clone());
                            if ui.button(label).on_hover_text(r).clicked() {
                                acts.push(Action::OpenRecent(i));
                                ui.close();
                            }
                        }
                    });
                }
                item(ui, tr(lang, "ctx_extract"), "", acts, Action::ExtractPage);
                item(ui, tr(lang, "modal_trash_title"), "Delete", acts, Action::Trash);
                ui.separator();
                item(ui, tr(lang, "menu_settings"), "Ctrl+,", acts, Action::Settings);
                ui.separator();
                item(ui, tr(lang, "menu_quit"), "Ctrl+Q", acts, Action::Quit);
            });
            ui.menu_button(tr(lang, "menu_view"), |ui| {
                let current = match self.zoom {
                    Zoom::Fit(m) => Some(m),
                    Zoom::Custom(_) => None,
                };
                for (m, label, short) in [
                    (FitMode::Fit, tr(lang, "view_fit"), "F"),
                    (FitMode::FitUpscale, tr(lang, "view_fit_upscale"), "Shift+F"),
                    (FitMode::Width, tr(lang, "view_fit_w"), "W"),
                    (FitMode::Height, tr(lang, "view_fit_h"), "H"),
                    (FitMode::Original, tr(lang, "view_orig"), "0"),
                ] {
                    let b = egui::Button::new(label)
                        .selected(current == Some(m))
                        .shortcut_text(short);
                    if ui.add(b).clicked() {
                        acts.push(Action::Fit(m));
                    }
                }
                ui.separator();
                item(ui, tr(lang, "view_zoomin"), "+", acts, Action::ZoomIn);
                item(ui, tr(lang, "view_zoomout"), "-", acts, Action::ZoomOut);
                ui.separator();
                item(ui, tr(lang, "view_rotate_cw"), "R", acts, Action::RotateCw);
                item(ui, tr(lang, "view_rotate_ccw"), "Shift+R", acts, Action::RotateCcw);
                ui.separator();
                item(ui, tr(lang, "view_anim"), "P", acts, Action::TogglePlay);
                item(ui, tr(lang, "view_frame_prev"), ",", acts, Action::FramePrev);
                item(ui, tr(lang, "view_frame_next"), ".", acts, Action::FrameNext);
                let lock = egui::Button::new(tr(lang, "view_lockbar"))
                    .selected(self.bar_locked)
                    .shortcut_text("L");
                if ui.add(lock).clicked() {
                    acts.push(Action::ToggleBarLock);
                }
                let toolbar = egui::Button::new(tr(lang, "view_toolbar"))
                    .selected(self.store.state.show_toolbar)
                    .shortcut_text("T");
                if ui.add(toolbar).clicked() {
                    acts.push(Action::ToggleToolbar);
                }
                let saver = egui::Button::new(tr(lang, "view_lowmem"))
                    .selected(self.low_memory_mode)
                    .shortcut_text("M");
                let two = egui::Button::new(tr(lang, "view_twopage"))
                    .selected(self.two_page_mode)
                    .shortcut_text("D");
                if ui.add(two).clicked() {
                    acts.push(Action::ToggleTwoPage);
                }
                let rtl = egui::Button::new(tr(lang, "view_rtl"))
                    .selected(self.rtl)
                    .shortcut_text("K");
                if ui.add(rtl).clicked() {
                    acts.push(Action::ToggleRtl);
                }
                let cover = egui::Button::new(tr(lang, "view_cover")).selected(self.cover_alone);
                if ui.add(cover).clicked() {
                    acts.push(Action::ToggleCover);
                }
                if ui
                    .add(saver)
                    .on_hover_text(tr(lang, "view_lowmem_tip"))
                    .clicked()
                {
                    acts.push(Action::ToggleLowMemory);
                }
                ui.separator();
                item(ui, tr(lang, "view_fullscreen"), "Enter", acts, Action::ToggleFullscreen);
            });
            ui.menu_button(tr(lang, "menu_navigate"), |ui| {
                let (kp, kn) = if self.rtl { ("Kanan", "Kiri") } else { ("Kiri", "Kanan") };
                item(ui, tr(lang, "nav_prev"), kp, acts, Action::Prev);
                item(ui, tr(lang, "nav_next"), kn, acts, Action::Next);
                item(ui, tr(lang, "nav_first"), "Home", acts, Action::First);
                item(ui, tr(lang, "nav_last"), "End", acts, Action::Last);
                ui.separator();
                item(ui, tr(lang, "nav_goto"), "Ctrl+G", acts, Action::GoToPage);
            });
            ui.menu_button(tr(lang, "menu_help"), |ui| {
                item(ui, tr(lang, "help_title"), "F1", acts, Action::Help);
                ui.separator();
                item(ui, tr(lang, "menu_about"), "", acts, Action::About);
            });
        });
    }

    /// Toolbar perkakas di bawah menu utama (ikon, tanpa label).
    fn draw_toolbar(&mut self, ctx: &Context, ui: &mut egui::Ui) {
        let lang = self.lang;
        let has_page = self.listing.is_some();
        let is_archive = self.listing.as_ref().is_some_and(|l| l.is_archive());
        let animated = self.page_for(self.index).is_some_and(|p| p.anim.is_some());
        let editing = self.edit.is_some();
        let edit_tool = self.edit.as_ref().map(|e| e.tool);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
            // Grup edit (perlu berkas asli, bukan arsip).
            let edit_tip = if is_archive { tr(lang, "toolbar_archive_tip") } else { "" };
            let crop_tip = if is_archive { edit_tip } else { tr(lang, "toolbar_crop_tip") };
            if toolbar_button(ui, ToolIcon::Crop, has_page && !is_archive, edit_tool == Some(EditTool::Crop), crop_tip) {
                self.edit_tool_clicked(EditTool::Crop);
            }
            let resize_tip = if is_archive { edit_tip } else { tr(lang, "toolbar_resize_tip") };
            if toolbar_button(ui, ToolIcon::Resize, has_page && !is_archive, edit_tool == Some(EditTool::Resize), resize_tip) {
                self.edit_tool_clicked(EditTool::Resize);
            }
            let rotate_tip = if is_archive { edit_tip } else { tr(lang, "toolbar_rotate_tip") };
            if toolbar_button(ui, ToolIcon::RotateCcw, has_page && !is_archive, edit_tool == Some(EditTool::Rotate), rotate_tip) {
                self.rotate_edit_buffer(false);
            }
            if toolbar_button(ui, ToolIcon::RotateCw, has_page && !is_archive, edit_tool == Some(EditTool::Rotate), rotate_tip) {
                self.rotate_edit_buffer(true);
            }
            ui.separator();
            // Grup navigasi frame (animasi, termasuk di dalam arsip).
            if toolbar_button(ui, ToolIcon::FramePrev, animated && !editing, false, tr(lang, "toolbar_frame_prev_tip")) {
                self.step_frame(ctx, -1);
            }
            let play_icon = if self.anim_playing { ToolIcon::Pause } else { ToolIcon::Play };
            if toolbar_button(ui, play_icon, animated && !editing, false, tr(lang, "toolbar_anim_tip")) {
                self.perform(ctx, Action::TogglePlay);
            }
            if toolbar_button(ui, ToolIcon::FrameNext, animated && !editing, false, tr(lang, "toolbar_frame_next_tip")) {
                self.step_frame(ctx, 1);
            }
            ui.separator();
            // Grup ambil warna & tone (perlu berkas asli, bukan arsip).
            let picker_tip = if is_archive { edit_tip } else { tr(lang, "toolbar_picker_tip") };
            if toolbar_button(ui, ToolIcon::Picker, has_page && !is_archive, edit_tool == Some(EditTool::Picker), picker_tip) {
                self.edit_tool_clicked(EditTool::Picker);
            }
            let tone_tip = if is_archive { edit_tip } else { tr(lang, "toolbar_tone_tip") };
            if toolbar_button(ui, ToolIcon::Tone, has_page && !is_archive, edit_tool == Some(EditTool::Tone), tone_tip) {
                self.edit_tool_clicked(EditTool::Tone);
            }
            ui.separator();
            // Grup cetak & proyektor (boleh di arsip).
            if toolbar_button(ui, ToolIcon::Print, has_page, self.print_open, tr(lang, "toolbar_print_tip")) {
                self.open_print_window();
            }
            if toolbar_button(ui, ToolIcon::Projector, has_page, self.projector_open, tr(lang, "toolbar_projector_tip")) {
                self.projector_open = !self.projector_open;
                if self.projector_open {
                    self.projector_tex = None;
                    self.projector_key = None;
                }
            }
            // Tombol sembunyikan toolbar di kanan.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").on_hover_text(tr(lang, "toolbar_hide_tip")).clicked() {
                    self.store.state.show_toolbar = false;
                    self.store_dirty = true;
                }
            });
        });
    }

    /// Klik tombol edit di toolbar: mulai sesi baru atau ganti perkakas.
    fn edit_tool_clicked(&mut self, tool: EditTool) {
        if let Some(e) = &self.edit {
            if e.tool == tool {
                // Klik kedua pada ikon tool yang sama: deselect (tutup sesi edit).
                self.close_edit();
            } else {
                self.switch_edit_tool(tool);
            }
        } else {
            self.start_edit(tool);
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        let lang = self.lang;
        ui.horizontal(|ui| {
            let Some(l) = &self.listing else {
                ui.label(tr(lang, "status_ready"));
                return;
            };
            let name = l.names.get(self.index).map_or("", String::as_str);
            let pair = self
                .shown
                .as_ref()
                .filter(|sh| sh.start == self.index && sh.slots.len() == 2)
                .map(|sh| sh.slots[1].0);
            match pair {
                Some(j) => ui.label(format!("{}-{}/{}", self.index + 1, j + 1, l.len())),
                None => ui.label(format!("{}/{}", self.index + 1, l.len())),
            };
            ui.separator();
            if let Some(sh) = self.shown.as_ref().filter(|sh| sh.start == self.index) {
                {
                    let page = &sh.slots[0].1;
                    let dims_txt = sh
                        .slots
                        .iter()
                        .map(|(_, p)| format!("{} x {}", p.orig[0], p.orig[1]))
                        .collect::<Vec<_>>()
                        .join(" + ");
                    let total: u64 = sh.slots.iter().map(|(_, p)| p.file_size).sum();
                    ui.label(format!("{dims_txt} px"));
                    ui.separator();
                    ui.label(fileinfo::human_size(total));
                    ui.separator();
                    let scale = self.display_scale;
                    ui.label(format!("{:.0}%", scale * 100.0));
                    ui.separator();
                    if let Some(a) = &page.anim {
                        let state = if self.anim_playing { "" } else { ", jeda" };
                        let cut = if a.truncated { ", dipotong" } else { "" };
                        ui.label(format!(
                            "animasi {}/{}{state}{cut}",
                            self.anim_frame + 1,
                            a.frames.len()
                        ));
                        ui.separator();
                    }
                }
            }
            if self.errors.contains_key(&self.index) {
                ui.colored_label(Color32::from_rgb(235, 110, 110), tr(lang, "status_failed"));
                ui.separator();
            } else if !self.cache.contains_key(&self.index) {
                ui.label(tr(lang, "status_loading"));
                ui.separator();
            }
            ui.add(egui::Label::new(name).truncate());
        });
    }
}

impl RoneyApp {
    fn draw_view(&mut self, ctx: &Context, ui: &mut egui::Ui, inp: &FrameInput) {
        let lang = self.lang;
        let rect = ui.available_rect_before_wrap();
        self.view_rect = rect;
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        self.panning = resp.dragged_by(PointerButton::Primary);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, BG);

        let Some(sh) = self.current_content() else {
            self.draw_placeholder(&painter, rect);
            self.draw_loading_badge(&painter, rect);
            return;
        };
        let shown_index = sh.start;
        let dims = self.shown_dims(ctx, &sh);
        let hover = ctx.pointer_hover_pos().filter(|p| rect.contains(*p));

        // Penempatan awal tiap kali gambar (atau rotasi/mode) berubah.
        let key = (self.session, shown_index, sh.slots.len());
        if self.placed_for != Some(key) {
            self.placed_for = Some(key);
            let geo = self.geometry(ctx, dims, rect);
            let max_y = ((geo.size.y - rect.height()) / 2.0).max(0.0);
            let y = if self.land_bottom {
                -max_y
            } else if matches!(self.zoom, Zoom::Fit(FitMode::Width)) {
                max_y
            } else {
                0.0
            };
            self.land_bottom = false;
            self.offset = vec2(0.0, y);
        }

        // Geser dengan seret kiri.
        if resp.dragged_by(PointerButton::Primary) {
            self.offset += resp.drag_delta();
        }
        if resp.double_clicked() {
            self.perform(ctx, Action::ToggleFullscreen);
        }

        // Zoom (Ctrl+roda / cubit di touchpad), berjangkar di kursor.
        if (inp.zoom - 1.0).abs() > 1e-4 && hover.is_some() {
            self.zoom_by(ctx, inp.zoom, hover);
        }

        // Roda mouse: geser vertikal bila gambar melebihi jendela,
        // kalau sudah mentok (atau muat) pindah halaman.
        if inp.wheel != 0.0 && resp.hovered() {
            let geo = self.geometry(ctx, dims, rect);
            let max_y = ((geo.size.y - rect.height()) / 2.0).max(0.0);
            let overflow = max_y > 0.5;
            let at_limit = overflow
                && ((inp.wheel > 0.0 && geo.offset.y >= max_y - 0.5)
                    || (inp.wheel < 0.0 && geo.offset.y <= -max_y + 0.5));
            if overflow && !at_limit {
                self.offset = vec2(
                    geo.offset.x,
                    (geo.offset.y + inp.wheel).clamp(-max_y, max_y),
                );
                self.flip_accum = 0.0;
            } else {
                self.wheel_flip(ctx, inp.wheel);
            }
        }

        let geo = self.geometry(ctx, dims, rect);
        self.offset = geo.offset;
        self.display_scale = geo.scale;

        let ppp = ctx.pixels_per_point();
        let snap = |v: f32| (v * ppp).round() / ppp;
        let center = rect.center() + geo.offset;
        let mut img_rect = Rect::from_center_size(center, geo.size);
        if (geo.scale - 1.0).abs() < 1e-4 {
            // Ukuran asli: sejajarkan ke piksel fisik agar tajam.
            img_rect = Rect::from_min_size(pos2(snap(img_rect.min.x), snap(img_rect.min.y)), geo.size);
        }
        let mut page_rects: Vec<(usize, Rect)> = Vec::new();
        if sh.slots.len() == 1 {
            paint_page(&painter, sh.slots[0].1.tex.id(), img_rect, self.rotation);
            page_rects.push((sh.slots[0].0, img_rect));
        } else {
            for (idx, r) in self.pair_rects(ctx, &sh, img_rect) {
                if let Some((_, p)) = sh.slots.iter().find(|(i, _)| *i == idx) {
                    paint_page(&painter, p.tex.id(), r, 0);
                }
                page_rects.push((idx, r));
            }
        }
        if resp.secondary_clicked() {
            let nearest = resp.interact_pointer_pos().and_then(|pp| {
                page_rects
                    .iter()
                    .min_by(|a, b| a.1.distance_to_pos(pp).total_cmp(&b.1.distance_to_pos(pp)))
                    .map(|(i, _)| *i)
            });
            self.ctx_target = nearest.unwrap_or(shown_index);
        }

        // Badge status loading di atas gambar: tr(lang, "Memuat...") kalau gambarnya
        // belum ada.
        self.draw_loading_badge(&painter, rect);

        // Menu klik kanan.
        let mut chosen: Option<CtxChoice> = None;
        // Item frame hanya untuk halaman animasi yang diklik kanan.
        let animated = self
            .page_for(self.ctx_target)
            .is_some_and(|p| p.anim.is_some());
        resp.context_menu(|ui| {
            if ui.button(tr(lang, "ctx_wallpaper_title")).clicked() {
                chosen = Some(CtxChoice::Wallpaper);
                ui.close();
            }
            if ui.button(tr(lang, "ctx_properties")).clicked() {
                chosen = Some(CtxChoice::Properties);
                ui.close();
            }
            ui.menu_button(tr(lang, "ctx_actions"), |ui| {
                if ui.button(tr(lang, "ctx_open_folder")).clicked() {
                    chosen = Some(CtxChoice::OpenFolder);
                    ui.close();
                }
                if ui.button(tr(lang, "ctx_extract")).clicked() {
                    chosen = Some(CtxChoice::Extract);
                    ui.close();
                }
                if animated {
                    if ui.button(tr(lang, "ctx_save_frame")).clicked() {
                        chosen = Some(CtxChoice::SaveFrame);
                        ui.close();
                    }
                    if ui.button(tr(lang, "ctx_export_frames")).clicked() {
                        chosen = Some(CtxChoice::ExportFrames);
                        ui.close();
                    }
                }
                if ui.button(tr(lang, "modal_trash_title")).clicked() {
                    chosen = Some(CtxChoice::Trash);
                    ui.close();
                }
            });
        });
        if let Some(c) = chosen {
            let target = self.ctx_target;
            self.handle_ctx(ctx, c, target);
        }

        let can_pan = geo.size.x > rect.width() + 0.5 || geo.size.y > rect.height() + 0.5;
        if can_pan && resp.hovered() {
            ctx.set_cursor_icon(if resp.dragged() {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            });
        }
    }

    /// Pil status loading di tengah atas area gambar.
    fn draw_loading_badge(&self, painter: &egui::Painter, rect: Rect) {
        // Jangan tampilkan badge kalau belum ada berkas/folder/arsip yang dibuka.
        if self.listing.is_none() {
            return;
        }
        let lang = self.lang;
        let badge = if self.errors.contains_key(&self.index) {
            None
        } else if !self.cache.contains_key(&self.index) {
            Some(tr(lang, "badge_loading"))
        } else {
            None
        };
        let Some(text) = badge else { return };
        let galley = painter.layout_no_wrap(
            text.to_owned(),
            egui::FontId::proportional(14.0),
            Color32::WHITE,
        );
        let pad = vec2(14.0, 7.0);
        let size = galley.size() + pad * 2.0;
        let br = Rect::from_center_size(rect.center_top() + vec2(0.0, 12.0), size);
        painter.rect_filled(br, 10.0, Color32::from_black_alpha(170));
        painter.galley(br.min + pad, galley, Color32::WHITE);
    }

    fn draw_placeholder(&self, painter: &egui::Painter, rect: Rect) {
        let lang = self.lang;
        let (text, color, size) = match (&self.listing, self.errors.get(&self.index)) {
            (None, _) => (
                unescape(tr(lang, "empty_hint")),
                Color32::from_gray(150),
                18.0,
            ),
            (Some(l), Some(err)) => {
                let name = l.names.get(self.index).map_or("", String::as_str);
                (
                    format!("Tidak dapat menampilkan\n{name}\n\n{err}"),
                    Color32::from_rgb(235, 130, 130),
                    16.0,
                )
            }
            (Some(_), None) => (tr(lang, "Memuat...").to_string(), Color32::from_gray(130), 16.0),
        };
        let galley = painter.layout(
            text,
            FontId::proportional(size),
            color,
            (rect.width() - 48.0).max(80.0),
        );
        let pos = rect.center() - galley.size() / 2.0;
        painter.galley(pos, galley, color);
    }

    fn draw_overlays(&mut self, ctx: &Context, hovering_files: bool) {
        let lang = self.lang;
        if hovering_files {
            egui::Area::new(egui::Id::new("drop_hint"))
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.heading(tr(lang, "drop_hint"));
                    });
                });
        }
        if let Some((text, until)) = &self.notice {
            let now = Instant::now();
            if now < *until {
                let text = text.clone();
                egui::Area::new(egui::Id::new("notice"))
                    .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -40.0))
                    .interactable(false)
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.label(text);
                        });
                    });
                ctx.request_repaint_after(*until - now);
            } else {
                self.notice = None;
            }
        }
        if self.show_help {
            let lang = self.lang;
            let mut open = true;
            egui::Window::new(tr(lang, "help_title"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    egui::Grid::new("help_grid").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                        for (k, d) in [
                            (tr(lang, "help_next_keys"), tr(lang, "help_next_img")),
                            (tr(lang, "help_prev_keys"), tr(lang, "help_prev_img")),
                            (tr(lang, "help_home_end"), tr(lang, "help_first_last")),
                            (tr(lang, "help_wheel_title"), tr(lang, "help_wheel")),
                            (tr(lang, "help_up_down"), tr(lang, "help_pan_y")),
                            (tr(lang, "help_drag"), tr(lang, "help_pan")),
                            ("Ctrl + roda / + / -", tr(lang, "help_zoom")),
                            (tr(lang, "help_fit"), tr(lang, "help_fit_desc")),
                            ("W / H", tr(lang, "help_fit_wh")),
                            (tr(lang, "shortcut_orig_size"), tr(lang, "help_orig")),
                            (tr(lang, "help_rotate_keys"), tr(lang, "help_rotate")),
                            ("P", tr(lang, "help_anim")),
                            (",", tr(lang, "view_frame_prev")),
                            (".", tr(lang, "view_frame_next")),
                            ("L", tr(lang, "help_lock")),
                            ("M", tr(lang, "help_lowmem")),
                            ("T", tr(lang, "view_toolbar")),
                            ("D", tr(lang, "view_twopage")),
                            ("K", tr(lang, "help_rtl")),
                            (tr(lang, "help_rightclick"), tr(lang, "help_ctx")),
                            (tr(lang, "help_mouse_edge"), tr(lang, "help_nav_btns")),
                            (tr(lang, "help_mouse_bottom"), tr(lang, "help_bar")),
                            (tr(lang, "help_fullscreen"), tr(lang, "help_fullscreen_desc")),
                            ("Ctrl+O", tr(lang, "help_open_file")),
                            ("Ctrl+Shift+O", tr(lang, "help_open_folder")),
                            ("Ctrl+G", tr(lang, "help_goto")),
                            ("Delete", tr(lang, "modal_trash_title")),
                            ("Ctrl+Q", tr(lang, "menu_quit")),
                        ] {
                            ui.monospace(k);
                            ui.label(d);
                            ui.end_row();
                        }
                    });
                });
            self.show_help = open;
        }
        if self.show_settings {
            let lang = self.lang;
            let mut open = true;
            egui::Window::new(tr(lang, "settings_title"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.heading(tr(lang, "settings_language"));
                    for l in [Lang::Id, Lang::En] {
                        if ui.radio(self.lang == l, l.display_name()).clicked() {
                            self.set_lang(l);
                        }
                    }
                    ui.add_space(8.0);
                    ui.heading(tr(lang, "settings_folders"));
                    let mut scan = self.store.state.scan_subfolders;
                    if ui
                        .checkbox(&mut scan, tr(lang, "settings_scan_subfolders"))
                        .changed()
                    {
                        self.store.state.scan_subfolders = scan;
                        self.store_dirty = true;
                    }
                    // Kategori pengaturan lain bisa ditambah di sini.
                    ui.add_space(8.0);
                    let mut recent = self.store.state.recent_folders;
                    if ui
                        .checkbox(&mut recent, tr(lang, "settings_recent_folders"))
                        .changed()
                    {
                        self.store.state.recent_folders = recent;
                        self.store_dirty = true;
                    }
                });
            self.show_settings = open;
        }
        if self.goto_open {
            let lang = self.lang;
            let mut open = true;
            let mut close = false;
            let n = self.count();
            egui::Window::new(tr(lang, "goto_title"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(tr_fmt(lang, "goto_prompt", &[&n, &(self.index + 1)]));
                    let id = ui.make_persistent_id("goto_input");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.goto_input)
                            .id(id)
                            .desired_width(120.0),
                    );
                    if self.goto_focus {
                        ui.memory_mut(|m| m.request_focus(id));
                        self.goto_focus = false;
                    }
                    let enter = resp.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if ui.button(tr(lang, "goto_go")).clicked() || enter {
                            self.confirm_goto(ctx);
                        }
                        if ui.button(tr(lang, "btn_cancel")).clicked() {
                            close = true;
                        }
                    });
                    if !close && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                });
            if !open || close {
                self.goto_open = false;
                self.goto_input.clear();
            }
        }
    }

    /// Majukan frame animasi bila waktunya. Tidak menjadwalkan repaint saat dijeda
    /// atau saat halaman bukan animasi, jadi CPU tetap idle untuk gambar statis.
    fn tick_animation(&mut self, ctx: &Context) {
        let Some(sh) = self.current_content() else {
            return;
        };
        // Animasi hanya diputar bila yang tampil satu halaman.
        let (shown_index, page) = match sh.slots.as_slice() {
            [(i, p)] => (*i, p.clone()),
            _ => {
                self.anim_key = None;
                return;
            }
        };
        let Some(anim) = &page.anim else {
            self.anim_key = None;
            return;
        };
        let now = Instant::now();
        let key = Some((self.session, shown_index));
        let first_delay = anim.frames[0].delay;
        if self.anim_key != key {
            // Halaman animasi baru (atau kembali ke halaman lama): mulai dari frame 0.
            self.anim_key = key;
            self.anim_frame = 0;
            self.anim_next = now + first_delay;
            if anim.uploaded.load(Ordering::Relaxed) != 0 {
                self.upload_frame(&page, 0);
            }
        } else if self.anim_playing && now >= self.anim_next {
            self.anim_frame = (self.anim_frame + 1) % anim.frames.len();
            self.upload_frame(&page, self.anim_frame);
            let delay = anim.frames[self.anim_frame].delay;
            // Setelah stall panjang, jangan mengejar ketinggalan frame.
            self.anim_next = if now.saturating_duration_since(self.anim_next) > delay {
                now + delay
            } else {
                self.anim_next + delay
            };
        }
        if self.anim_playing {
            ctx.request_repaint_after(self.anim_next.saturating_duration_since(Instant::now()));
        }
    }

    fn upload_frame(&self, page: &Page, frame: usize) {
        let Some(anim) = &page.anim else {
            return;
        };
        let Some(f) = anim.frames.get(frame) else {
            return;
        };
        let mut tex = page.tex.clone();
        tex.set(f.image.clone(), self.texture_options(true));
        anim.uploaded.store(frame, Ordering::Relaxed);
    }

    fn save_state(&mut self) {
        if self.store_dirty {
            // Galat penyimpanan sengaja diabaikan: bukan alasan untuk mengganggu pengguna.
            let _ = self.store.save();
            self.store_dirty = false;
        }
        if self.positions_dirty {
            let _ = self.positions.save();
            self.positions_dirty = false;
        }
        self.last_save = Instant::now();
    }
}

impl eframe::App for RoneyApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Sinkronkan status layar penuh bila diubah oleh window manager.
        if let Some(f) = ctx.input(|i| i.viewport().fullscreen) {
            self.fullscreen = f;
        }

        self.poll_loader(ctx);
        self.poll_dialog(ctx);
        self.poll_pdf_job();

        let inp = self.read_input(ctx);
        if let Some(p) = inp.dropped.first() {
            self.open_path(ctx, p);
        }
        // Saat kotak konfirmasi/modal atau jendela "Lompat ke halaman" terbuka,
        // pintasan keyboard utama tidak berlaku. Saat mengedit gambar,
        // pintasan navigasi utama juga dimatikan (Esc menutup sesi edit).
        if self.modal.is_none() && !self.goto_open && self.edit.is_none() {
            for a in &inp.actions {
                self.perform(ctx, *a);
            }
        }

        self.tick_animation(ctx);

        let mut acts: Vec<Action> = Vec::new();
        if !self.fullscreen {
            egui::TopBottomPanel::top("menu").show(ctx, |ui| self.menu_bar(ui, &mut acts));
            if self.store.state.show_toolbar {
                egui::TopBottomPanel::top("toolbar").show(ctx, |ui| self.draw_toolbar(ctx, ui));
            }
            egui::TopBottomPanel::bottom("status").show(ctx, |ui| self.status_bar(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG))
            .show(ctx, |ui| {
                if self.edit.is_some() {
                    self.draw_edit_view(ctx, ui);
                } else {
                    self.draw_view(ctx, ui, &inp);
                }
            });
        self.update_title(ctx);
        if self.edit.is_none() {
            self.draw_nav_overlays(ctx);
        }
        for a in acts {
            self.perform(ctx, a);
        }

        self.draw_overlays(ctx, inp.hovering_files);
        self.draw_dialogs(ctx);
        if self.print_open {
            self.draw_print_window(ctx);
        }
        if self.about_open {
            self.draw_about_window(ctx);
        }
        if self.projector_open {
            self.draw_projector(ctx);
        }

        if self.store_dirty || self.positions_dirty {
            if self.last_save.elapsed() >= Duration::from_millis(1500) {
                self.save_state();
            } else {
                ctx.request_repaint_after(Duration::from_millis(1600));
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_state();
    }
}

/// Dekode satu gambar untuk PDF: orientasi EXIF diterapkan, dibatasi 1600px.
fn decode_for_pdf(bytes: &[u8]) -> Result<RgbaImage, String> {
    use image::metadata::Orientation;
    use image::{DynamicImage, ImageReader};
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("Gagal membaca berkas: {e}"))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| "Format tidak dikenali".to_string())?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder)
        .map_err(|_| "Gambar rusak".to_string())?;
    img.apply_orientation(orientation);
    let (w, h) = (img.width(), img.height());
    let scale = (1600.0 / w.max(h) as f32).min(1.0);
    if scale < 1.0 {
        let (tw, th) = ((w as f32 * scale).round() as u32, (h as f32 * scale).round() as u32);
        Ok(img
            .resize(tw.max(1), th.max(1), image::imageops::FilterType::Triangle)
            .to_rgba8())
    } else {
        Ok(img.to_rgba8())
    }
}

/// Bangun PDF untuk seluruh daftar (maks 500 gambar): halaman penuh atau
/// contact sheet sesuai `layout` (0 penuh, 1 = 2x2, 2 = 3x3, 3 = 4x4).
fn build_pdf_for_listing(
    listing: &Listing,
    layout: usize,
    paper: usize,
    landscape: bool,
) -> Result<Vec<u8>, String> {
    let n = listing.len();
    if n == 0 {
        return Err("Tidak ada gambar".to_string());
    }
    if n > 500 {
        return Err("Terlalu banyak gambar untuk PDF (maks 500)".to_string());
    }
    let (mut pw, mut ph) = if paper == 1 { (612.0, 792.0) } else { (595.0, 842.0) };
    if landscape {
        std::mem::swap(&mut pw, &mut ph);
    }
    let (cols, rows) = match layout {
        1 => (2, 2),
        2 => (3, 3),
        3 => (4, 4),
        _ => (1, 1),
    };
    let per = cols * rows;
    let margin = 36.0;
    let gap = 12.0;
    let mut reader = listing.open_reader()?;
    let mut pages = Vec::new();
    for start in (0..n).step_by(per) {
        let mut slots = Vec::new();
        for (cell, idx) in (start..(start + per).min(n)).enumerate() {
            let bytes = reader.read(idx)?;
            let img = decode_for_pdf(&bytes)?;
            let jpeg = edit::jpeg_encode(&img, 85)?;
            let (cx, cy) = (cell % cols, cell / cols);
            let cw = (pw - margin * 2.0 - gap * (cols - 1) as f32) / cols as f32;
            let ch = (ph - margin * 2.0 - gap * (rows - 1) as f32) / rows as f32;
            let s = (cw / img.width() as f32).min(ch / img.height() as f32);
            let (w, h) = (img.width() as f32 * s, img.height() as f32 * s);
            let cell_x = margin + cx as f32 * (cw + gap);
            let cell_y_top = margin + cy as f32 * (ch + gap);
            let x = cell_x + (cw - w) / 2.0;
            let y = ph - (cell_y_top + (ch - h) / 2.0) - h; // atas-kiri -> bawah-kiri
            slots.push(edit::PdfSlot {
                x,
                y,
                w,
                h,
                jpeg,
                img_w: img.width(),
                img_h: img.height(),
            });
        }
        pages.push(edit::PdfPage { width: pw, height: ph, slots });
    }
    Ok(edit::build_pdf(&pages))
}

/// Kirim dokumen PDF ke printer default via `lpr` (CUPS).
fn print_pdf_lpr(pdf: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new("lpr")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Perintah lpr tidak ditemukan: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(pdf).map_err(|e| format!("Gagal mengirim ke lpr: {e}"))?;
    }
    let status = child.wait().map_err(|e| format!("lpr gagal: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("Printer menolak dokumen (lpr)".to_string())
    }
}

fn paint_page(painter: &egui::Painter, tex: egui::TextureId, rect: Rect, rotation: u8) {    let uv = [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)];
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    let r = (rotation % 4) as usize;
    let mut mesh = egui::Mesh::with_texture(tex);
    for (k, pos) in corners.iter().enumerate() {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: *pos,
            uv: uv[(k + 4 - r) % 4],
            color: Color32::WHITE,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}

// ------------------------------------------------------- overlay navigasi

/// Tinggi zona bawah yang memunculkan bar navigasi (poin).
const BAR_ZONE_H: f32 = 110.0;
const BAR_GAP: f32 = 14.0;
const FADE_SECS: f32 = 0.18;
const ACCENT: Color32 = Color32::from_rgb(90, 160, 255);

fn side_zone_width(view_w: f32) -> f32 {
    (view_w * 0.15).clamp(70.0, 170.0)
}

/// (zona kiri, zona kanan, zona bawah) untuk area gambar `view`.
fn nav_zones(view: Rect) -> (Rect, Rect, Rect) {
    let w = side_zone_width(view.width());
    let left = Rect::from_min_max(view.min, pos2(view.min.x + w, view.max.y));
    let right = Rect::from_min_max(pos2(view.max.x - w, view.min.y), view.max);
    let top = (view.max.y - BAR_ZONE_H).max(view.min.y);
    let bottom = Rect::from_min_max(pos2(view.min.x, top), view.max);
    (left, right, bottom)
}

/// Posisi x di slider -> indeks gambar (0-based), dijepit ke ujung.
fn scrub_index(x: f32, left: f32, width: f32, n: usize) -> usize {
    if n <= 1 || width <= 0.0 || !x.is_finite() {
        return 0;
    }
    let t = ((x - left) / width).clamp(0.0, 1.0);
    ((t * (n - 1) as f32).round() as usize).min(n - 1)
}

/// Kebalikan `scrub_index`: posisi x pegangan slider untuk indeks tertentu.
fn scrub_x(idx: usize, left: f32, width: f32, n: usize) -> f32 {
    if n <= 1 {
        return left;
    }
    left + width * (idx.min(n - 1) as f32 / (n - 1) as f32)
}

/// Pendekkan nama panjang di tengah ("foto_aaaa...zzzz.jpg"), hitung per karakter.
fn truncate_middle(name: &str, max: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= max || max < 8 {
        return name.to_string();
    }
    let head = (max - 3) / 2 + (max - 3) % 2;
    let tail = (max - 3) / 2;
    let mut out: String = chars[..head].iter().collect();
    out.push_str("...");
    out.extend(chars[chars.len() - tail..].iter());
    out
}

#[derive(Clone, Copy)]
enum Chevron {
    Left,
    Right,
}

fn paint_chevron(painter: &egui::Painter, rect: Rect, dir: Chevron, color: Color32, width: f32) {
    let c = rect.center();
    let h = rect.height().min(rect.width() * 1.6) * 0.26;
    let w = h * 0.55;
    let (a, b, d) = match dir {
        Chevron::Left => (pos2(c.x + w, c.y - h), pos2(c.x - w, c.y), pos2(c.x + w, c.y + h)),
        Chevron::Right => (pos2(c.x - w, c.y - h), pos2(c.x + w, c.y), pos2(c.x - w, c.y + h)),
    };
    painter.add(egui::Shape::line(vec![a, b, d], egui::Stroke::new(width, color)));
}

/// Ikon perkakas di toolbar, digambar sendiri dengan primitif egui.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolIcon {
    Crop,
    Resize,
    RotateCw,
    RotateCcw,
    FramePrev,
    FrameNext,
    Play,
    Pause,
    Picker,
    Tone,
    Print,
    Projector,
}

fn paint_tool_icon(painter: &egui::Painter, rect: Rect, icon: ToolIcon, color: Color32) {
    let stroke = egui::Stroke::new(1.8_f32, color);
    let x = rect.min.x;
    let y = rect.min.y;
    let w = rect.width();
    let h = rect.height();
    let p = |fx: f32, fy: f32| pos2(x + w * fx, y + h * fy);
    match icon {
        ToolIcon::Crop => {
            // Dua braket sudut: kiri-atas dan kanan-bawah.
            painter.add(egui::Shape::line(vec![p(0.18, 0.42), p(0.18, 0.18), p(0.42, 0.18)], stroke));
            painter.add(egui::Shape::line(vec![p(0.82, 0.58), p(0.82, 0.82), p(0.58, 0.82)], stroke));
        }
        ToolIcon::Resize => {
            painter.rect_stroke(Rect::from_min_max(p(0.22, 0.22), p(0.78, 0.78)), 1.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment([p(0.22, 0.78), p(0.78, 0.22)], stroke);
            painter.add(egui::Shape::line(vec![p(0.58, 0.22), p(0.78, 0.22), p(0.78, 0.42)], stroke));
        }
        ToolIcon::RotateCw => {
            // Panah melingkar searah jarum jam.
            let c = p(0.5, 0.52);
            let r = 0.28 * w.min(h);
            let mut pts = Vec::with_capacity(20);
            for i in 0..=18 {
                let a: f32 = -0.6 + i as f32 * 4.6 / 18.0;
                pts.push(pos2(c.x + r * a.cos(), c.y + r * a.sin()));
            }
            painter.add(egui::Shape::line(pts, stroke));
            let ae: f32 = -0.6 + 4.6;
            let tip = pos2(c.x + r * ae.cos(), c.y + r * ae.sin());
            let back = pos2(c.x + r * (ae - 0.5_f32).cos(), c.y + r * (ae - 0.5_f32).sin());
            let dir = pos2(tip.x - back.x, tip.y - back.y);
            let n = (dir.x * dir.x + dir.y * dir.y).sqrt().max(0.001);
            let (ux, uy) = (dir.x / n, dir.y / n);
            let s = 0.09 * w.min(h);
            painter.line_segment([tip, pos2(tip.x - ux * s - uy * s * 0.6, tip.y - uy * s + ux * s * 0.6)], stroke);
            painter.line_segment([tip, pos2(tip.x - ux * s + uy * s * 0.6, tip.y - uy * s - ux * s * 0.6)], stroke);
        }
        ToolIcon::RotateCcw => {
            // Panah melingkar berlawanan jarum jam (cermin dari CW).
            let c = p(0.5, 0.52);
            let r = 0.28 * w.min(h);
            let mut pts = Vec::with_capacity(20);
            for i in 0..=18 {
                let a: f32 = 0.6 - i as f32 * 4.6 / 18.0;
                pts.push(pos2(c.x + r * a.cos(), c.y + r * a.sin()));
            }
            painter.add(egui::Shape::line(pts, stroke));
            let ae: f32 = 0.6 - 4.6;
            let tip = pos2(c.x + r * ae.cos(), c.y + r * ae.sin());
            let back = pos2(c.x + r * (ae + 0.5_f32).cos(), c.y + r * (ae + 0.5_f32).sin());
            let dir = pos2(tip.x - back.x, tip.y - back.y);
            let n = (dir.x * dir.x + dir.y * dir.y).sqrt().max(0.001);
            let (ux, uy) = (dir.x / n, dir.y / n);
            let s = 0.09 * w.min(h);
            painter.line_segment([tip, pos2(tip.x - ux * s - uy * s * 0.6, tip.y - uy * s + ux * s * 0.6)], stroke);
            painter.line_segment([tip, pos2(tip.x - ux * s + uy * s * 0.6, tip.y - uy * s - ux * s * 0.6)], stroke);
        }
        ToolIcon::FramePrev => {
            painter.line_segment([p(0.28, 0.25), p(0.28, 0.75)], stroke);
            painter.add(egui::Shape::convex_polygon(
                vec![p(0.72, 0.25), p(0.38, 0.5), p(0.72, 0.75)],
                color,
                egui::Stroke::NONE,
            ));
        }
        ToolIcon::FrameNext => {
            painter.line_segment([p(0.72, 0.25), p(0.72, 0.75)], stroke);
            painter.add(egui::Shape::convex_polygon(
                vec![p(0.28, 0.25), p(0.62, 0.5), p(0.28, 0.75)],
                color,
                egui::Stroke::NONE,
            ));
        }
        ToolIcon::Play => {
            painter.add(egui::Shape::convex_polygon(
                vec![p(0.32, 0.25), p(0.72, 0.5), p(0.32, 0.75)],
                color,
                egui::Stroke::NONE,
            ));
        }
        ToolIcon::Pause => {
            painter.rect_filled(Rect::from_min_max(p(0.32, 0.25), p(0.45, 0.75)), 1.0, color);
            painter.rect_filled(Rect::from_min_max(p(0.55, 0.25), p(0.68, 0.75)), 1.0, color);
        }
        ToolIcon::Picker => {
            // Pipet: garis diagonal + ujung + pegangan.
            painter.line_segment([p(0.30, 0.70), p(0.62, 0.38)], stroke);
            painter.circle_filled(p(0.28, 0.72), 2.2, color);
            painter.add(egui::Shape::line(vec![p(0.62, 0.38), p(0.78, 0.22), p(0.82, 0.30)], stroke));
            painter.line_segment([p(0.62, 0.38), p(0.70, 0.46)], stroke);
        }
        ToolIcon::Tone => {
            // Matahari + garis gelombang (tone curve).
            painter.circle_stroke(p(0.38, 0.38), w * 0.16, stroke);
            painter.add(egui::Shape::line(
                vec![p(0.18, 0.70), p(0.32, 0.62), p(0.50, 0.74), p(0.68, 0.60), p(0.82, 0.68)],
                stroke,
            ));
        }
        ToolIcon::Print => {
            // Printer: badan + kertas.
            painter.rect_stroke(Rect::from_min_max(p(0.20, 0.28), p(0.80, 0.60)), 1.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment([p(0.30, 0.28), p(0.30, 0.18)], stroke);
            painter.line_segment([p(0.70, 0.28), p(0.70, 0.18)], stroke);
            painter.line_segment([p(0.30, 0.18), p(0.70, 0.18)], stroke);
            painter.rect_stroke(Rect::from_min_max(p(0.32, 0.60), p(0.68, 0.82)), 1.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment([p(0.40, 0.70), p(0.60, 0.70)], stroke);
        }
        ToolIcon::Projector => {
            // Layar + sinar proyeksi.
            painter.rect_stroke(Rect::from_min_max(p(0.18, 0.30), p(0.55, 0.60)), 1.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment([p(0.55, 0.38), p(0.82, 0.28)], stroke);
            painter.line_segment([p(0.55, 0.52), p(0.82, 0.62)], stroke);
            painter.line_segment([p(0.36, 0.60), p(0.36, 0.72)], stroke);
            painter.line_segment([p(0.28, 0.72), p(0.45, 0.72)], stroke);
        }
    }
}

/// Tombol ikon toolbar 28x28: latar saat hover/aktif, ikon digambar sendiri.
fn toolbar_button(
    ui: &mut egui::Ui,
    icon: ToolIcon,
    enabled: bool,
    active: bool,
    tooltip: &str,
) -> bool {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), sense);
    let hovered = enabled && resp.hovered();
    let pressed = enabled && resp.is_pointer_button_down_on();
    let painter = ui.painter();
    let bg = if active {
        Color32::from_rgba_unmultiplied(90, 160, 255, 110)
    } else if pressed {
        Color32::from_black_alpha(120)
    } else if hovered {
        Color32::from_black_alpha(70)
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(rect, 5.0, bg);
    if active {
        painter.rect_stroke(rect, 5.0, egui::Stroke::new(1.0_f32, ACCENT), egui::StrokeKind::Inside);
    }
    let col = if enabled { Color32::WHITE } else { Color32::from_white_alpha(70) };
    paint_tool_icon(painter, rect.shrink(3.0), icon, col);
    resp.on_hover_text(tooltip).clicked()
}

/// Tombol dengan panah yang digambar sendiri (tanpa bergantung pada glyph font).
fn icon_button(
    ui: &mut egui::Ui,
    size: Vec2,
    dir: Chevron,
    enabled: bool,
    round: f32,
    base_alpha: u8,
) -> egui::Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let hovered = enabled && resp.hovered();
    let pressed = enabled && resp.is_pointer_button_down_on();
    let alpha = if pressed {
        225
    } else if hovered {
        200
    } else {
        base_alpha
    };
    let painter = ui.painter();
    painter.rect_filled(rect, round, Color32::from_black_alpha(alpha));
    let col = if enabled {
        Color32::WHITE
    } else {
        Color32::from_white_alpha(70)
    };
    paint_chevron(painter, rect, dir, col, 2.6);
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

struct BarInput {
    n: usize,
    index: usize,
    scrub: Option<usize>,
    locked: bool,
    /// Lebar isi (di dalam margin bingkai).
    width: f32,
    /// Arah baca kanan-ke-kiri: slider dicerminkan.
    rtl: bool,
    can_left: bool,
    can_right: bool,
    /// Halaman terakhir pada spread dua halaman yang sedang tampil.
    pair_end: Option<usize>,
    lang: Lang,
}

#[derive(Default)]
struct BarOutput {
    left: bool,
    right: bool,
    locked: bool,
    /// Indeks di bawah pointer selama tombol mouse ditahan pada slider.
    scrub: Option<usize>,
    /// Tombol mouse sedang ditahan pada slider.
    active: bool,
    slider: Option<Rect>,
}

fn draw_bar(ui: &mut egui::Ui, inp: &BarInput) -> BarOutput {
    let lang = inp.lang;
    let mut out = BarOutput {
        locked: inp.locked,
        ..Default::default()
    };
    let last = inp.n.saturating_sub(1);
    let btn = vec2(32.0, 28.0);
    let label_w = 84.0;
    let lock_w = 78.0;
    let gap = 8.0;
    let slider_w = (inp.width - 2.0 * btn.x - label_w - lock_w - 4.0 * gap).max(60.0);

    ui.spacing_mut().item_spacing.x = gap;
    ui.horizontal(|ui| {
        out.left = icon_button(ui, btn, Chevron::Left, inp.can_left, 6.0, 90).clicked();

        // Slider kustom: nilai mengikuti pointer selama tombol ditahan.
        let sense = if inp.n > 1 {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        };
        let (rect, resp) = ui.allocate_exact_size(vec2(slider_w, btn.y), sense);
        let pad = 9.0;
        let track_left = rect.left() + pad;
        let track_w = (rect.width() - 2.0 * pad).max(1.0);
        let down = inp.n > 1 && resp.is_pointer_button_down_on();
        if down {
            if let Some(p) = resp.interact_pointer_pos() {
                // Dicerminkan untuk arah baca kanan-ke-kiri.
                let x = if inp.rtl { 2.0 * track_left + track_w - p.x } else { p.x };
                out.scrub = Some(scrub_index(x, track_left, track_w, inp.n));
            }
            out.active = true;
        }
        let shown = out
            .scrub
            .or(inp.scrub)
            .unwrap_or(inp.index)
            .min(last);
        let hx_ltr = scrub_x(shown, track_left, track_w, inp.n);
        let hx = if inp.rtl { 2.0 * track_left + track_w - hx_ltr } else { hx_ltr };
        let cy = rect.center().y;
        let painter = ui.painter();
        let track = Rect::from_min_max(pos2(track_left, cy - 2.0), pos2(track_left + track_w, cy + 2.0));
        painter.rect_filled(track, 2.0, Color32::from_white_alpha(55));
        let fill = if inp.rtl {
            Rect::from_min_max(pos2(hx, track.min.y), track.max)
        } else {
            Rect::from_min_max(track.min, pos2(hx, track.max.y))
        };
        painter.rect_filled(fill, 2.0, ACCENT);
        let r = if down {
            9.0
        } else if resp.hovered() {
            8.0
        } else {
            7.0
        };
        painter.circle_filled(pos2(hx, cy), r, Color32::WHITE);
        if inp.n > 1 {
            let _ = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
        out.slider = Some(rect);

        out.right = icon_button(ui, btn, Chevron::Right, inp.can_right, 6.0, 90).clicked();

        ui.add_sized(
            [label_w, btn.y],
            egui::Label::new(
                egui::RichText::new(match (inp.scrub.or(out.scrub), inp.pair_end) {
                    (None, Some(j)) => format!("{}-{}/{}", shown + 1, j + 1, inp.n),
                    _ => format!("{}/{}", shown + 1, inp.n),
                })
                    .monospace()
                    .color(Color32::WHITE),
            ),
        );

        let mut locked = inp.locked;
        let cb = ui
            .add_sized([lock_w, btn.y], egui::Checkbox::new(&mut locked, tr(lang, "bar_lock")))
            .on_hover_text(tr(lang, "bar_lock_tip"));
        if cb.changed() {
            out.locked = locked;
        }
    });
    out
}

impl RoneyApp {
    fn page_for(&self, index: usize) -> Option<Arc<Page>> {
        self.cache
            .get(&index)
            .cloned()
            .or_else(|| {
                self.shown
                    .as_ref()
                    .and_then(|sh| sh.slots.iter().find(|(i, _)| *i == index).map(|(_, p)| p.clone()))
            })
    }

    /// Buka dialog konfirmasi pindah ke sampah untuk berkas pada `index`
    /// (dipakai tombol Delete dan menu klik kanan).
    fn confirm_trash(&mut self, index: usize) {
        let Some(listing) = self.listing.clone() else {
            return;
        };
        if self.modal.is_some() {
            return;
        }
        if listing.is_archive() {
            self.modal = Some(Modal::ArchiveWarning {
                archive: listing.origin.display().to_string(),
            });
            return;
        }
        let Some(name) = listing.names.get(index).cloned() else {
            return;
        };
        let path = listing.origin.join(&name);
        self.modal = Some(Modal::ConfirmTrash { index, path, name });
    }

    fn handle_ctx(&mut self, ctx: &Context, choice: CtxChoice, index: usize) {
        let lang = self.lang;
        let Some(listing) = self.listing.clone() else {
            return;
        };
        let Some(page) = self.page_for(index) else {
            return;
        };
        match choice {
            CtxChoice::Properties => {
                let facts = PageFacts {
                    width: page.orig[0],
                    height: page.orig[1],
                    file_size: page.file_size,
                    format: page.format.clone(),
                };
                self.props = Some(PropsDialog {
                    rows: fileinfo::properties_rows(&listing, index, &facts, self.lang),
                });
            }
            CtxChoice::Wallpaper => {
                let ppp = ctx.pixels_per_point();
                let screen = ctx
                    .input(|i| i.viewport().monitor_size)
                    .map_or([1920.0, 1080.0], |m| [(m.x * ppp).max(320.0), (m.y * ppp).max(240.0)]);
                self.wallpaper = Some(WallpaperDialog::new(
                    listing,
                    index,
                    page.tex.id(),
                    page.orig,
                    screen,
                    self.store.state.wallpaper.clone(),
                ));
            }
            CtxChoice::Extract => self.start_extract_dialog(ctx, index),
            CtxChoice::SaveFrame => {
                let frame = self.anim_frame;
                self.start_save_frame_dialog(ctx, index, frame);
            }
            CtxChoice::ExportFrames => self.start_dialog(ctx, DialogKind::ExportFrames { index }),
            CtxChoice::OpenFolder => {
                if listing.is_archive() {
                    self.modal = Some(Modal::ArchiveWarning {
                        archive: listing.origin.display().to_string(),
                    });
                    return;
                }
                let Some(name) = listing.names.get(index).cloned() else {
                    return;
                };
                let path = listing.origin.join(&name);
                dialogs::open_in_folder(&path, self.note_tx.clone(), ctx.clone());
                self.notify(tr(lang, "notify_open_folder"));
            }
            CtxChoice::Trash => self.confirm_trash(index),
        }
    }

    /// Kosongkan tampilan (mis. folder tidak berisi gambar lagi).
    fn clear_listing(&mut self, ctx: &Context) {
        self.session += 1;
        self.listing = None;
        self.index = 0;
        self.cache.clear();
        self.pending.clear();
        self.errors.clear();
        self.dims.clear();
        self.shown = None;
        self.placed_for = None;
        self.title = String::new();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title("Roneyview".into()));
    }

    fn after_trash(&mut self, ctx: &Context, index: usize, name: &str) {
        let Some(listing) = self.listing.clone() else {
            return;
        };
        let scan = self.store.state.scan_subfolders;
        match Listing::open(&listing.origin, scan) {
            Ok((l, _)) => self.install_listing(ctx, l, Some(index)),
            Err(_) => self.clear_listing(ctx),
        }
        self.notify(format!("Dipindahkan ke Tempat Sampah: {name}"));
    }

    fn draw_dialogs(&mut self, ctx: &Context) {
        let lang = self.lang;
        while let Ok(msg) = self.note_rx.try_recv() {
            self.modal = Some(Modal::Error(msg));
        }
        if let Some(modal) = self.modal.take() {
            match dialogs::draw_modal(ctx, &modal, self.lang) {
                ModalResult::Keep => self.modal = Some(modal),
                ModalResult::Close => {}
                ModalResult::ConfirmTrash => {
                    if let Modal::ConfirmTrash { index, path, name } = modal {
                        match dialogs::move_to_trash(&path) {
                            Ok(()) => self.after_trash(ctx, index, &name),
                            Err(e) => self.modal = Some(Modal::Error(e)),
                        }
                    }
                }
            }
        }
        if let Some(dlg) = self.props.take() {
            let mut open = true;
            let mut inner_close = false;
            dialogs::show_dialog(ctx, "roneyview-properties", tr(lang, "ctx_properties"), [520.0, 340.0], &mut open, |_, ui| {
                if dialogs::draw_properties(ui, &dlg, lang) {
                    inner_close = true;
                }
            });
            if open && !inner_close {
                self.props = Some(dlg);
            }
        }
        if let Some(mut dlg) = self.wallpaper.take() {
            let mut open = true;
            let mut inner_close = false;
            dialogs::show_dialog(ctx, "roneyview-wallpaper", tr(lang, "ctx_wallpaper"), [460.0, 560.0], &mut open, |c, ui| {
                if dialogs::draw_wallpaper(c, ui, &mut dlg, lang) {
                    inner_close = true;
                }
            });
            if dlg.prefs != self.store.state.wallpaper {
                self.store.state.wallpaper = dlg.prefs.clone();
                self.store_dirty = true;
            }
            if open && !inner_close {
                self.wallpaper = Some(dlg);
            }
        }
    }

    fn set_low_memory(&mut self, ctx: &Context, on: bool) {
        let lang = self.lang;
        self.low_memory_mode = on;
        self.store.state.low_memory = on;
        self.store_dirty = true;
        // Buang segera semua halaman selain yang sedang dilihat.
        let (lo, hi) = keep_window(self.index, on, self.two_page_mode);
        self.cache.retain(|i, _| (lo..=hi).contains(i));
        if !on {
            self.request_pages(ctx); // nyalakan lagi prefetch
        }
        self.notify(if on {
            tr(lang, "notify_lowmem_on")
        } else {
            tr(lang, "notify_lowmem_off")
        });
    }

    fn set_two_page(&mut self, ctx: &Context, on: bool) {
        let lang = self.lang;
        self.two_page_mode = on;
        self.store.state.two_page = on;
        self.store_dirty = true;
        self.relayout(ctx);
        self.notify(if on { tr(lang, "notify_twopage_on") } else { tr(lang, "notify_twopage_off") });
    }

    fn set_rtl(&mut self, ctx: &Context, on: bool) {
        let lang = self.lang;
        self.rtl = on;
        self.store.state.rtl = on;
        self.store_dirty = true;
        self.placed_for = None;
        ctx.request_repaint();
        self.notify(if on {
            tr(lang, "help_rtl_desc")
        } else {
            "Arah baca kiri-ke-kanan"
        });
    }

    fn set_cover(&mut self, ctx: &Context, on: bool) {
        let lang = self.lang;
        self.cover_alone = on;
        self.store.state.cover_alone = on;
        self.store_dirty = true;
        self.relayout(ctx);
        self.notify(if on {
            tr(lang, "notify_cover_on")
        } else {
            tr(lang, "notify_cover_off")
        });
    }

    /// Susun ulang tampilan setelah pengaturan dua halaman berubah: rapatkan awal spread
    /// ke kisi yang benar lalu muat ulang halaman di sekitarnya.
    fn relayout(&mut self, ctx: &Context) {
        self.placed_for = None;
        self.zoom = Zoom::Fit(self.fit);
        let t = self.snap_start(self.index);
        if t != self.index && t < self.count() {
            self.go_to(ctx, t, false);
        } else {
            self.after_page_change(ctx);
        }
    }

    fn set_bar_lock(&mut self, locked: bool) {
        self.bar_locked = locked;
        self.store.state.bar_locked = locked;
        self.store_dirty = true;
    }

    /// Tombol prev/next di tepi kiri-kanan dan bar bawah (tombol + slider + Kunci).
    /// Semuanya muncul perlahan hanya saat pointer berada di zonanya.
    fn draw_nav_overlays(&mut self, ctx: &Context) {
        let n = self.count();
        if n == 0 {
            self.scrub = None;
            return;
        }
        let view = self.view_rect;
        let (zone_l, zone_r, zone_b) = nav_zones(view);
        let ptr = ctx.pointer_hover_pos();
        let busy = self.panning;
        let over = |z: Rect| !busy && ptr.is_some_and(|p| z.contains(p));

        // Arah baca kanan-ke-kiri menukar fungsi tombol kiri dan kanan.
        let can_prev = self.index > 0;
        let can_next = self.index + self.next_span() < n;
        let (can_left, can_right) = if self.rtl { (can_next, can_prev) } else { (can_prev, can_next) };
        let (left_act, right_act) = if self.rtl {
            (Action::Next, Action::Prev)
        } else {
            (Action::Prev, Action::Next)
        };
        let t_left = ctx.animate_bool_with_time(
            egui::Id::new("nav_left"),
            can_left && over(zone_l),
            FADE_SECS,
        );
        let t_right = ctx.animate_bool_with_time(
            egui::Id::new("nav_right"),
            can_right && over(zone_r),
            FADE_SECS,
        );
        // Bar tetap tampil selama dikunci atau slider sedang ditahan.
        let want_bar = self.bar_locked || self.scrub.is_some() || over(zone_b);
        let t_bar = ctx.animate_bool_with_time(egui::Id::new("nav_bar"), want_bar, FADE_SECS);

        let mut action: Option<Action> = None;
        let side_size = vec2(46.0, 88.0);
        if t_left > 0.02 {
            let clicked = egui::Area::new(egui::Id::new("nav_left_btn"))
                .order(egui::Order::Foreground)
                .pivot(Align2::LEFT_CENTER)
                .fixed_pos(pos2(view.min.x + 14.0, view.center().y))
                .show(ctx, |ui| {
                    ui.set_opacity(t_left);
                    icon_button(ui, side_size, Chevron::Left, true, 12.0, 120).clicked()
                })
                .inner;
            if clicked {
                action = Some(left_act);
            }
        }
        if t_right > 0.02 {
            let clicked = egui::Area::new(egui::Id::new("nav_right_btn"))
                .order(egui::Order::Foreground)
                .pivot(Align2::RIGHT_CENTER)
                .fixed_pos(pos2(view.max.x - 14.0, view.center().y))
                .show(ctx, |ui| {
                    ui.set_opacity(t_right);
                    icon_button(ui, side_size, Chevron::Right, true, 12.0, 120).clicked()
                })
                .inner;
            if clicked {
                action = Some(right_act);
            }
        }

        let mut bar: Option<BarOutput> = None;
        if t_bar > 0.02 {
            let bar_w = (view.width() - 24.0).clamp(220.0, 780.0);
            let pair_end = self
                .shown
                .as_ref()
                .filter(|sh| sh.start == self.index && sh.slots.len() == 2)
                .map(|sh| sh.slots[1].0);
            let input = BarInput {
                n,
                index: self.index,
                scrub: self.scrub,
                locked: self.bar_locked,
                width: bar_w - 20.0,
                rtl: self.rtl,
                can_left,
                can_right,
                pair_end,
                lang: self.lang,
            };
            let out = egui::Area::new(egui::Id::new("nav_bar_area"))
                .order(egui::Order::Foreground)
                .pivot(Align2::CENTER_BOTTOM)
                .fixed_pos(pos2(view.center().x, view.max.y - BAR_GAP))
                .show(ctx, |ui| {
                    ui.set_opacity(t_bar);
                    egui::Frame::new()
                        .fill(Color32::from_black_alpha(205))
                        .corner_radius(10.0)
                        .inner_margin(egui::Margin::symmetric(10, 6))
                        .show(ui, |ui| draw_bar(ui, &input))
                        .inner
                })
                .inner;
            bar = Some(out);
        }

        // Slider: halaman baru dibuka saat tombol mouse dilepas (bukan di tiap piksel
        // geseran), supaya menyeret melewati ratusan foto besar tidak memuat semuanya.
        let active = bar.as_ref().is_some_and(|b| b.active);
        if active {
            if let Some(b) = &bar {
                self.scrub = b.scrub.or(self.scrub);
            }
        } else if let Some(target) = self.scrub.take() {
            let t = self.snap_start(target);
            self.go_to(ctx, t, false);
        }

        // Petunjuk melayang di atas pegangan slider saat digeser.
        if let (Some(sc), Some(sl)) = (self.scrub, bar.as_ref().and_then(|b| b.slider)) {
            let (tl, tw) = (sl.left() + 9.0, (sl.width() - 18.0).max(1.0));
            let hx_ltr = scrub_x(sc, tl, tw, n);
            let hx = if self.rtl { 2.0 * tl + tw - hx_ltr } else { hx_ltr };
            let name = self
                .listing
                .as_ref()
                .and_then(|l| l.names.get(sc))
                .map_or(String::new(), |s| truncate_middle(s, 44));
            egui::Area::new(egui::Id::new("nav_scrub_tip"))
                .order(egui::Order::Foreground)
                .pivot(Align2::CENTER_BOTTOM)
                .fixed_pos(pos2(
                    hx.clamp(view.left() + 100.0, view.right() - 100.0),
                    sl.top() - 12.0,
                ))
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label(format!("{}/{}  {}", sc + 1, n, name));
                    });
                });
        }

        if let Some(b) = &bar {
            if b.left {
                action = Some(left_act);
            } else if b.right {
                action = Some(right_act);
            }
            if b.locked != self.bar_locked {
                self.set_bar_lock(b.locked);
            }
        }
        if let Some(a) = action {
            self.perform(ctx, a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_nomor_halaman() {
        assert_eq!(parse_page_number("1", 10), Some(0));
        assert_eq!(parse_page_number("10", 10), Some(9));
        assert_eq!(parse_page_number("  3  ", 10), Some(2));
        assert_eq!(parse_page_number("0", 10), None);
        assert_eq!(parse_page_number("11", 10), None);
        assert_eq!(parse_page_number("abc", 10), None);
        assert_eq!(parse_page_number("", 10), None);
        assert_eq!(parse_page_number("1", 0), None, "daftar kosong -> tidak ada halaman valid");
    }

    #[test]
    fn recent_tanpa_duplikat_dan_dibatasi() {
        let mut r = Vec::new();
        record_recent(&mut r, "/a");
        record_recent(&mut r, "/b");
        record_recent(&mut r, "/a");
        assert_eq!(r, vec!["/a".to_string(), "/b".to_string()]);
        record_recent(&mut r, "");
        assert_eq!(r.len(), 2, "path kosong diabaikan");
        for i in 0..30 {
            record_recent(&mut r, &format!("/f{i}"));
        }
        assert_eq!(r.len(), MAX_RECENT);
        assert_eq!(r[0], "/f29");
    }

    #[test]
    fn langkah_frame_melingkar() {
        assert_eq!(step_index(0, 5, 1), 1);
        assert_eq!(step_index(4, 5, 1), 0, "maju dari frame terakhir kembali ke awal");
        assert_eq!(step_index(0, 5, -1), 4, "mundur dari frame pertama ke akhir");
        assert_eq!(step_index(2, 5, -1), 1);
        assert_eq!(step_index(0, 1, 1), 0);
        assert_eq!(step_index(0, 0, 1), 0, "tanpa frame tidak panik");
    }

    #[test]
    fn colorimage_ke_rgba_benar() {
        let img = ColorImage::new(
            [2, 1],
            vec![
                Color32::from_rgba_unmultiplied(255, 0, 0, 255),
                Color32::from_rgba_unmultiplied(0, 255, 0, 128),
            ],
        );
        let rgba = colorimage_to_rgba(&img);
        assert_eq!((rgba.width(), rgba.height()), (2, 1));
        assert_eq!(rgba.as_raw(), &[255, 0, 0, 255, 0, 255, 0, 128]);
    }

    #[test]
    fn frame_tersimpan_sebagai_png_valid() {
        let img = ColorImage::new([2, 2], vec![Color32::RED; 4]);
        let rgba = colorimage_to_rgba(&img);
        let dir = std::env::temp_dir().join("rv-frame-test");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("f.png");
        crate::wallpaper::write_png(&rgba, &p).unwrap();
        let b = std::fs::read(&p).unwrap();
        assert_eq!(&b[..8], b"\x89PNG\r\n\x1a\n", "tanda tangan PNG");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tier_resolusi_mengikuti_zoom_dan_cap_8_langkah() {
        assert_eq!(tier_pixels(0.25), 2_000_000); // fit
        assert_eq!(tier_pixels(1.0), 2_000_000); // 100%
        assert_eq!(tier_pixels(6.9), 2_000_000);
        assert_eq!(tier_pixels(7.0), 4_000_000); // zoom >= 7x -> refine
        assert_eq!(tier_pixels(8.0), 4_000_000);
        assert_eq!(MAX_ZOOM_STEPS, 8); // maksimal 8x tekan zoom dari fit
    }

    #[test]
    fn mode_muat_menghitung_skala_dengan_benar() {
        let img = vec2(2000.0, 1000.0);
        let view = vec2(1000.0, 800.0);
        assert!((fit_scale(FitMode::Fit, img, view) - 0.5).abs() < 1e-6);
        assert!((fit_scale(FitMode::Width, img, view) - 0.5).abs() < 1e-6);
        assert!((fit_scale(FitMode::Height, img, view) - 0.8).abs() < 1e-6);
        assert_eq!(fit_scale(FitMode::Original, img, view), 1.0);
        // Gambar kecil: Fit tidak memperbesar, FitUpscale memperbesar.
        let small = vec2(100.0, 50.0);
        assert_eq!(fit_scale(FitMode::Fit, small, view), 1.0);
        assert!((fit_scale(FitMode::FitUpscale, small, view) - 10.0).abs() < 1e-6);
    }

    #[test]
    fn ukuran_nol_tidak_menyebabkan_pembagian_nol() {
        assert!(fit_scale(FitMode::Fit, vec2(0.0, 0.0), vec2(0.0, 0.0)).is_finite());
    }

    #[test]
    fn offset_dijepit_dan_gambar_kecil_dipusatkan() {
        let view = vec2(100.0, 100.0);
        assert_eq!(clamp_offset(vec2(50.0, 50.0), vec2(80.0, 80.0), view), Vec2::ZERO);
        assert_eq!(
            clamp_offset(vec2(500.0, -500.0), vec2(300.0, 200.0), view),
            vec2(100.0, -50.0)
        );
    }

    #[test]
    fn zona_navigasi_kiri_kanan_bawah() {
        let view = Rect::from_min_size(pos2(0.0, 24.0), vec2(1000.0, 700.0));
        let (l, r, b) = nav_zones(view);
        assert!(l.contains(pos2(10.0, 300.0)));
        assert!(!l.contains(pos2(500.0, 300.0)));
        assert!(r.contains(pos2(990.0, 300.0)));
        assert!(!r.contains(pos2(500.0, 300.0)));
        assert!(b.contains(pos2(500.0, 720.0)));
        assert!(!b.contains(pos2(500.0, 300.0)));
        assert_eq!(side_zone_width(200.0), 70.0);
        assert_eq!(side_zone_width(1000.0), 150.0);
        assert_eq!(side_zone_width(5000.0), 170.0);
        // jendela sangat pendek: zona bawah tidak keluar dari area
        let tiny = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 60.0));
        let (_, _, b) = nav_zones(tiny);
        assert!(b.min.y >= tiny.min.y);
    }

    #[test]
    fn slider_menjangkau_setiap_gambar_tepat_dan_dijepit_di_ujung() {
        for n in [2usize, 3, 46, 100, 1000] {
            for idx in 0..n {
                let x = scrub_x(idx, 10.0, 300.0, n);
                assert_eq!(scrub_index(x, 10.0, 300.0, n), idx, "n={n} idx={idx}");
            }
        }
        // contoh pengguna: gambar ke-46 dari 100 = indeks 45
        let x = scrub_x(45, 0.0, 400.0, 100);
        assert_eq!(scrub_index(x, 0.0, 400.0, 100) + 1, 46);
        // di luar slider: dijepit
        assert_eq!(scrub_index(-500.0, 10.0, 300.0, 100), 0);
        assert_eq!(scrub_index(9999.0, 10.0, 300.0, 100), 99);
        // kasus tepi tidak panik
        assert_eq!(scrub_index(5.0, 0.0, 100.0, 0), 0);
        assert_eq!(scrub_index(5.0, 0.0, 100.0, 1), 0);
        assert_eq!(scrub_index(f32::NAN, 0.0, 100.0, 10), 0);
        assert_eq!(scrub_index(5.0, 0.0, 0.0, 10), 0);
        assert_eq!(scrub_x(0, 7.0, 100.0, 1), 7.0);
    }

    #[test]
    fn nama_panjang_dipendekkan_di_tengah_tanpa_memotong_karakter() {
        assert_eq!(truncate_middle("pendek.jpg", 44), "pendek.jpg");
        let long = "foto_liburan_keluarga_besar_2026_bagian_sangat_panjang_sekali.jpg";
        let t = truncate_middle(long, 30);
        assert_eq!(t.chars().count(), 30);
        assert!(t.starts_with("foto_liburan"));
        assert!(t.ends_with(".jpg"));
        assert!(t.contains("..."));
        // karakter multibyte tidak boleh membuat panik
        let uni = "日本語のとても長いファイル名その二その三その四.png";
        let t = truncate_middle(uni, 12);
        assert_eq!(t.chars().count(), 12);
    }

    #[test]
    fn jendela_cache_normal_dan_mode_hemat() {
        assert_eq!(keep_window(10, false, false), (8, 13));
        assert_eq!(keep_window(0, false, false), (0, 3));
        assert_eq!(keep_window(10, true, false), (10, 10));
        assert_eq!(keep_window(0, true, false), (0, 0));
        // dua halaman: jendela melebar, mode hemat menyimpan tepat sepasang
        assert_eq!(keep_window(10, false, true), (7, 15));
        assert_eq!(keep_window(10, true, true), (10, 11));
        assert_eq!(keep_window(0, true, true), (0, 1));
    }

    #[test]
    fn awal_spread_mengikuti_kisi_dengan_dan_tanpa_sampul() {
        // dengan sampul tunggal: 0 | 1 2 | 3 4 | 5 6 ...
        let cover: Vec<usize> = (0..8).map(|t| snap_start_for(t, true, true)).collect();
        assert_eq!(cover, vec![0, 1, 1, 3, 3, 5, 5, 7]);
        // tanpa sampul: 0 1 | 2 3 | 4 5 ...
        let plain: Vec<usize> = (0..8).map(|t| snap_start_for(t, true, false)).collect();
        assert_eq!(plain, vec![0, 0, 2, 2, 4, 4, 6, 6]);
        // mode satu halaman tidak mengubah apa pun
        assert!((0..8).all(|t| snap_start_for(t, false, true) == t));
    }

    #[test]
    fn maju_mundur_melewati_spread_tanpa_kehilangan_halaman() {
        let none = |_: usize| false;
        // semua halaman potret, 11 halaman, dengan sampul: 0 | 1 2 | 3 4 | 5 6 | 7 8 | 9 10
        let mut walk = vec![0usize];
        let mut s = 0;
        loop {
            let span = if s == 0 { 1 } else { 2 };
            if s + span >= 11 {
                break;
            }
            s += span;
            walk.push(s);
        }
        assert_eq!(walk, vec![0, 1, 3, 5, 7, 9]);
        // mundur dari ujung harus menempuh urutan yang sama
        let mut back = vec![9usize];
        let mut cur = 9;
        while let Some(p) = prev_start_for(cur, true, true, &none) {
            back.push(p);
            cur = p;
        }
        back.reverse();
        assert_eq!(back, walk);
        // tanpa sampul: 0 2 4 ...
        assert_eq!(prev_start_for(4, true, false, &none), Some(2));
        assert_eq!(prev_start_for(2, true, false, &none), Some(0));
        assert_eq!(prev_start_for(1, true, false, &none), Some(0));
        assert_eq!(prev_start_for(0, true, false, &none), None);
        // mode satu halaman: mundur satu per satu
        assert_eq!(prev_start_for(5, false, true, &none), Some(4));
    }

    #[test]
    fn halaman_lebar_dilompati_sebagai_tunggal_saat_mundur() {
        // halaman 4 lebar: pasangan (3,4) tidak boleh terbentuk
        let wide4 = |i: usize| i == 4;
        assert_eq!(prev_start_for(5, true, true, &wide4), Some(4));
        // dengan sampul, dari 2 mundur ke 1 (halaman 1 tampil sendiri), bukan 0
        assert_eq!(prev_start_for(2, true, true, &|_| false), Some(1));
        assert_eq!(prev_start_for(1, true, true, &|_| false), Some(0));
        // pasangan biasa tetap utuh bila tak ada halaman lebar
        assert_eq!(prev_start_for(7, true, true, &wide4), Some(5));
    }

    #[test]
    fn dua_halaman_disamakan_tingginya_dan_tidak_dibagi_nol() {
        let (w, h) = pair_widths(&[(800.0, 1200.0), (800.0, 1200.0)]);
        assert_eq!((w.clone(), h), (vec![800.0, 800.0], 1200.0));
        // halaman kedua lebih pendek: diperbesar agar tingginya sama dengan yang tertinggi
        let (w, h) = pair_widths(&[(800.0, 1200.0), (400.0, 600.0)]);
        assert_eq!(h, 1200.0);
        assert!((w[0] - 800.0).abs() < 1e-3 && (w[1] - 800.0).abs() < 1e-3, "{w:?}");
        let (w, h) = pair_widths(&[(0.0, 0.0), (10.0, 0.0)]);
        assert!(h >= 1.0 && w.iter().all(|v| v.is_finite()));
    }
}
