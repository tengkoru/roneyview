//! Antarmuka Roneyview (egui/eframe): navigasi, zoom, geser, rotasi, layar penuh.

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, Context, FontId, Key, Modifiers, PointerButton, Pos2,
    Rect, Sense, TextureHandle, TextureOptions, Vec2, pos2, vec2,
};
use eframe::glow::HasContext;

use crate::dialogs::{self, Modal, ModalResult, PropsDialog, WallpaperDialog};
use crate::fileinfo::{self, PageFacts};
use crate::loader::{self, AnimFrame, Command, Loaded, Outcome};
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

/// Animasi (GIF/WebP/APNG): semua frame disimpan di sisi CPU dan diunggah
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
    /// Benar bila ini pratinjau blur progresif; versi tajam menyusul.
    preview: bool,
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
    Help,
    Quit,
}

#[derive(Clone, Copy)]
enum DialogKind {
    File,
    Folder,
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

    fit: FitMode,
    zoom: Zoom,
    rotation: u8,
    offset: Vec2,
    land_bottom: bool,
    view_rect: Rect,
    display_scale: f32,
    flip_accum: f32,
    flip_block_until: Instant,

    anim_key: Option<(u64, usize)>,
    anim_frame: usize,
    anim_next: Instant,
    anim_playing: bool,

    fullscreen: bool,
    show_help: bool,
    mipmaps: bool,
    notice: Option<(String, Instant)>,
    title: String,
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
        let bar_locked = store.state.bar_locked;
        let two_page_mode = store.state.two_page;
        let rtl = store.state.rtl;
        let cover_alone = store.state.cover_alone;
        let low_memory_mode = store.state.low_memory;

        let mut app = RoneyApp {
            store,
            store_dirty: false,
            last_save: Instant::now(),
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
            fit,
            zoom: Zoom::Fit(fit),
            rotation: 0,
            offset: Vec2::ZERO,
            land_bottom: false,
            view_rect: Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0)),
            display_scale: 1.0,
            flip_accum: 0.0,
            flip_block_until: Instant::now(),
            anim_key: None,
            anim_frame: 0,
            anim_next: Instant::now(),
            anim_playing: true,
            fullscreen: false,
            show_help: false,
            mipmaps,
            notice: None,
            title: String::new(),
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
        let (listing, start) = match Listing::open(path) {
            Ok(v) => v,
            Err(e) => {
                self.notify(e);
                return;
            }
        };
        self.install_listing(ctx, listing, start.unwrap_or(0));
    }

    /// Pasang daftar gambar baru dan tampilkan gambar ke-`index`.
    fn install_listing(&mut self, ctx: &Context, listing: Listing, index: usize) {
        let index = index.min(listing.len().saturating_sub(1));
        self.session += 1;
        let listing = Arc::new(listing);
        // Open harus sampai ke KEDUA worker pemuat.
        let open = || Command::Open {
            session: self.session,
            listing: listing.clone(),
        };
        if self.cmd_tx_main.send(open()).is_err() || self.cmd_tx_pref.send(open()).is_err() {
            self.notify("Thread pemuat gambar berhenti; mulai ulang Roneyview.");
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
        self.after_page_change(ctx);
    }

    #[cfg(feature = "dialogs")]
    fn start_dialog(&mut self, ctx: &Context, kind: DialogKind) {
        if self.dialog_busy {
            return;
        }
        self.dialog_busy = true;
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
                                .add_filter("Gambar dan arsip ZIP/CBZ", &exts)
                                .pick_file()
                        }
                        DialogKind::Folder => d.set_title("Buka folder gambar").pick_folder(),
                    }
                })
                .unwrap_or(None);
                let _ = tx.send(picked);
                ctx.request_repaint();
            });
        if spawned.is_err() {
            self.dialog_busy = false;
            self.notify("Tidak dapat membuka dialog berkas.");
        }
    }

    #[cfg(not(feature = "dialogs"))]
    fn start_dialog(&mut self, _ctx: &Context, _kind: DialogKind) {
        self.notify("Dialog berkas tidak tersedia di build ini. Seret berkas ke jendela.");
    }

    fn poll_dialog(&mut self, ctx: &Context) {
        #[cfg(feature = "dialogs")]
        while let Ok(result) = self.dialog_rx.try_recv() {
            self.dialog_busy = false;
            if let Some(p) = result {
                self.open_path(ctx, &p);
            }
        }
        #[cfg(not(feature = "dialogs"))]
        let _ = ctx;
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
                self.notify("Memori sistem hampir habis: prefetch dimatikan");
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
                Outcome::Preview(d) => {
                    // Pratinjau blur: tampilkan langsung supaya ada gambarnya,
                    // versi tajam (Ready) menyusul dari pemrosesan yang sama.
                    let (lo, hi) = keep_window(self.index, self.low_memory_mode, self.two_page_mode);
                    if !(lo..=hi).contains(&msg.index) {
                        continue; // sudah terlalu jauh, buang
                    }
                    let px = d.pixels;
                    self.dims.insert(msg.index, px.orig);
                    let decoded = [px.image.size[0] as u32, px.image.size[1] as u32];
                    let bytes = decoded[0] as usize * decoded[1] as usize * 4;
                    let tex = ctx.load_texture(
                        format!("pg{}-{}-prev", msg.session, msg.index),
                        px.image,
                        self.texture_options(false),
                    );
                    let page = Arc::new(Page {
                        tex,
                        orig: px.orig,
                        decoded,
                        preview: true,
                        file_size: d.file_size,
                        bytes,
                        anim: None,
                        format: px.format.clone(),
                    });
                    self.cache.insert(msg.index, page);
                    ctx.request_repaint();
                }
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
                        preview: false,
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
        let n = self.count();
        if n == 0 {
            return;
        }
        let target = self.index + self.next_span();
        if target >= n {
            self.notify("Ini gambar terakhir");
        } else {
            self.go_to(ctx, target, false);
        }
    }

    fn go_prev(&mut self, ctx: &Context, land_bottom: bool) {
        let wide = |i: usize| self.is_wide(i);
        match prev_start_for(self.index, self.two_page_mode, self.cover_alone, &wide) {
            None => self.notify("Ini gambar pertama"),
            Some(t) => self.go_to(ctx, t, land_bottom),
        }
    }

    fn go_to(&mut self, ctx: &Context, target: usize, land_bottom: bool) {
        if target >= self.count() || target == self.index {
            return;
        }
        self.dir = if target > self.index { 1 } else { -1 };
        self.index = target;
        self.rotation = 0;
        self.zoom = Zoom::Fit(self.fit);
        self.land_bottom = land_bottom;
        self.after_page_change(ctx);
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
        if page.preview {
            return; // versi tajam sedang di jalan (Ready menyusul Preview)
        }
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
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
        ctx.request_repaint();
    }

    fn rotate_checked(&mut self, quarter_turns: u8) {
        if self.shown.as_ref().is_some_and(|s| s.slots.len() == 2) {
            self.notify("Rotasi tidak tersedia pada mode dua halaman");
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
                take(none, Key::L, Action::ToggleBarLock, i);
                take(none, Key::D, Action::ToggleTwoPage, i);
                take(none, Key::K, Action::ToggleRtl, i);
                take(none, Key::M, Action::ToggleLowMemory, i);
                take(none, Key::Enter, Action::ToggleFullscreen, i);
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
            ui.menu_button("Berkas", |ui| {
                item(ui, "Buka berkas atau arsip...", "Ctrl+O", acts, Action::OpenFile);
                item(ui, "Buka folder...", "Ctrl+Shift+O", acts, Action::OpenFolder);
                ui.separator();
                item(ui, "Keluar", "Ctrl+Q", acts, Action::Quit);
            });
            ui.menu_button("Tampilan", |ui| {
                let current = match self.zoom {
                    Zoom::Fit(m) => Some(m),
                    Zoom::Custom(_) => None,
                };
                for (m, label, short) in [
                    (FitMode::Fit, "Muat ke jendela (kecilkan saja)", "F"),
                    (FitMode::FitUpscale, "Muat ke jendela (perbesar juga)", "Shift+F"),
                    (FitMode::Width, "Sesuaikan lebar", "W"),
                    (FitMode::Height, "Sesuaikan tinggi", "H"),
                    (FitMode::Original, "Ukuran asli (100%)", "0"),
                ] {
                    let b = egui::Button::new(label)
                        .selected(current == Some(m))
                        .shortcut_text(short);
                    if ui.add(b).clicked() {
                        acts.push(Action::Fit(m));
                    }
                }
                ui.separator();
                item(ui, "Perbesar", "+", acts, Action::ZoomIn);
                item(ui, "Perkecil", "-", acts, Action::ZoomOut);
                ui.separator();
                item(ui, "Putar searah jarum jam", "R", acts, Action::RotateCw);
                item(ui, "Putar berlawanan jarum jam", "Shift+R", acts, Action::RotateCcw);
                ui.separator();
                item(ui, "Jeda / putar animasi", "P", acts, Action::TogglePlay);
                let lock = egui::Button::new("Kunci bar bawah")
                    .selected(self.bar_locked)
                    .shortcut_text("L");
                if ui.add(lock).clicked() {
                    acts.push(Action::ToggleBarLock);
                }
                let saver = egui::Button::new("Mode hemat memori")
                    .selected(self.low_memory_mode)
                    .shortcut_text("M");
                let two = egui::Button::new("Mode dua halaman")
                    .selected(self.two_page_mode)
                    .shortcut_text("D");
                if ui.add(two).clicked() {
                    acts.push(Action::ToggleTwoPage);
                }
                let rtl = egui::Button::new("Arah baca kanan-ke-kiri (manga)")
                    .selected(self.rtl)
                    .shortcut_text("K");
                if ui.add(rtl).clicked() {
                    acts.push(Action::ToggleRtl);
                }
                let cover = egui::Button::new("Halaman pertama tunggal (sampul)").selected(self.cover_alone);
                if ui.add(cover).clicked() {
                    acts.push(Action::ToggleCover);
                }
                if ui
                    .add(saver)
                    .on_hover_text("Hanya gambar yang sedang dilihat disimpan di RAM; tanpa prefetch")
                    .clicked()
                {
                    acts.push(Action::ToggleLowMemory);
                }
                ui.separator();
                item(ui, "Layar penuh", "Enter", acts, Action::ToggleFullscreen);
            });
            ui.menu_button("Navigasi", |ui| {
                let (kp, kn) = if self.rtl { ("Kanan", "Kiri") } else { ("Kiri", "Kanan") };
                item(ui, "Sebelumnya", kp, acts, Action::Prev);
                item(ui, "Berikutnya", kn, acts, Action::Next);
                item(ui, "Pertama", "Home", acts, Action::First);
                item(ui, "Terakhir", "End", acts, Action::Last);
            });
            ui.menu_button("Bantuan", |ui| {
                item(ui, "Pintasan keyboard", "F1", acts, Action::Help);
            });
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let Some(l) = &self.listing else {
                ui.label("Siap. Seret gambar, folder, atau ZIP ke jendela ini.");
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
                ui.colored_label(Color32::from_rgb(235, 110, 110), "gagal dimuat");
                ui.separator();
            } else if !self.cache.contains_key(&self.index) {
                ui.label("memuat...");
                ui.separator();
            }
            ui.add(egui::Label::new(name).truncate());
        });
    }
}

impl RoneyApp {
    fn draw_view(&mut self, ctx: &Context, ui: &mut egui::Ui, inp: &FrameInput) {
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

        // Badge status loading di atas gambar: "Memuat..." kalau gambarnya
        // belum ada, "Mempertajam..." kalau yang tampil masih pratinjau blur.
        self.draw_loading_badge(&painter, rect);

        // Menu klik kanan.
        let mut chosen: Option<CtxChoice> = None;
        resp.context_menu(|ui| {
            if ui.button("Set as wallpaper...").clicked() {
                chosen = Some(CtxChoice::Wallpaper);
                ui.close();
            }
            if ui.button("Properties").clicked() {
                chosen = Some(CtxChoice::Properties);
                ui.close();
            }
            ui.menu_button("Tindakan", |ui| {
                if ui.button("Buka di dalam folder").clicked() {
                    chosen = Some(CtxChoice::OpenFolder);
                    ui.close();
                }
                if ui.button("Pindahkan ke sampah").clicked() {
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
        let badge = if self.errors.contains_key(&self.index) {
            None
        } else if !self.cache.contains_key(&self.index) {
            Some("Memuat…")
        } else if self.cache.get(&self.index).is_some_and(|p| p.preview) {
            Some("Mempertajam…")
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

    fn draw_placeholder(&self, painter: &egui::Painter, rect: Rect) {        let (text, color, size) = match (&self.listing, self.errors.get(&self.index)) {
            (None, _) => (
                "Roneyview\n\nSeret gambar, folder, atau ZIP/CBZ ke sini\natau tekan Ctrl+O. F1 untuk pintasan.".to_string(),
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
            (Some(_), None) => ("Memuat...".to_string(), Color32::from_gray(130), 16.0),
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
        if hovering_files {
            egui::Area::new(egui::Id::new("drop_hint"))
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.heading("Lepaskan untuk membuka");
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
            let mut open = true;
            egui::Window::new("Pintasan keyboard")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    egui::Grid::new("help_grid").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                        for (k, d) in [
                            ("Kanan / PgDn / Spasi", "Gambar berikutnya"),
                            ("Kiri / PgUp / Backspace", "Gambar sebelumnya"),
                            ("Home / End", "Pertama / terakhir"),
                            ("Roda mouse", "Geser; pindah gambar bila sudah mentok"),
                            ("Atas / Bawah", "Geser vertikal"),
                            ("Seret kiri", "Geser gambar"),
                            ("Ctrl + roda / + / -", "Perbesar / perkecil"),
                            ("F / Shift+F", "Muat ke jendela (kecilkan / perbesar juga)"),
                            ("W / H", "Sesuaikan lebar / tinggi"),
                            ("0 atau 1", "Ukuran asli 100%"),
                            ("R / Shift+R", "Putar kanan / kiri"),
                            ("P", "Jeda / putar animasi (GIF, WebP, APNG)"),
                            ("L", "Kunci / lepas bar bawah"),
                            ("M", "Mode hemat memori (buang gambar dari RAM setelah dilihat)"),
                            ("D", "Mode dua halaman"),
                            ("K", "Arah baca kanan-ke-kiri (panah kiri = berikutnya)"),
                            ("Klik kanan", "Set as wallpaper, Properties, Tindakan"),
                            ("Mouse ke tepi kiri/kanan", "Tombol sebelumnya / berikutnya"),
                            ("Mouse ke bawah", "Bar: tombol, slider lompat, Kunci"),
                            ("Enter / F11 / klik ganda", "Layar penuh (Esc keluar)"),
                            ("Ctrl+O", "Buka berkas atau arsip"),
                            ("Ctrl+Shift+O", "Buka folder"),
                            ("Ctrl+Q", "Keluar"),
                        ] {
                            ui.monospace(k);
                            ui.label(d);
                            ui.end_row();
                        }
                    });
                });
            self.show_help = open;
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

        let inp = self.read_input(ctx);
        if let Some(p) = inp.dropped.first() {
            self.open_path(ctx, p);
        }
        // Saat kotak konfirmasi terbuka, pintasan keyboard utama tidak berlaku.
        if self.modal.is_none() {
            for a in &inp.actions {
                self.perform(ctx, *a);
            }
        }

        self.tick_animation(ctx);

        let mut acts: Vec<Action> = Vec::new();
        if !self.fullscreen {
            egui::TopBottomPanel::top("menu").show(ctx, |ui| self.menu_bar(ui, &mut acts));
            egui::TopBottomPanel::bottom("status").show(ctx, |ui| self.status_bar(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG))
            .show(ctx, |ui| self.draw_view(ctx, ui, &inp));
        self.update_title(ctx);
        self.draw_nav_overlays(ctx);
        for a in acts {
            self.perform(ctx, a);
        }

        self.draw_overlays(ctx, inp.hovering_files);
        self.draw_dialogs(ctx);

        if self.store_dirty {
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

fn paint_page(painter: &egui::Painter, tex: egui::TextureId, rect: Rect, rotation: u8) {
    let uv = [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)];
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
            .add_sized([lock_w, btn.y], egui::Checkbox::new(&mut locked, "Kunci"))
            .on_hover_text("Kunci: bar ini tetap tampil walau pointer menjauh (L)");
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

    fn handle_ctx(&mut self, ctx: &Context, choice: CtxChoice, index: usize) {
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
                    rows: fileinfo::properties_rows(&listing, index, &facts),
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
            CtxChoice::OpenFolder | CtxChoice::Trash => {
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
                if matches!(choice, CtxChoice::OpenFolder) {
                    dialogs::open_in_folder(&path, self.note_tx.clone(), ctx.clone());
                    self.notify("Membuka folder...");
                } else {
                    self.modal = Some(Modal::ConfirmTrash { index, path, name });
                }
            }
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
        match Listing::open(&listing.origin) {
            Ok((l, _)) => self.install_listing(ctx, l, index),
            Err(_) => self.clear_listing(ctx),
        }
        self.notify(format!("Dipindahkan ke Tempat Sampah: {name}"));
    }

    fn draw_dialogs(&mut self, ctx: &Context) {
        while let Ok(msg) = self.note_rx.try_recv() {
            self.modal = Some(Modal::Error(msg));
        }
        if let Some(modal) = self.modal.take() {
            match dialogs::draw_modal(ctx, &modal) {
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
            dialogs::show_dialog(ctx, "roneyview-properties", "Properties", [520.0, 340.0], &mut open, |_, ui| {
                if dialogs::draw_properties(ui, &dlg) {
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
            dialogs::show_dialog(ctx, "roneyview-wallpaper", "Set as wallpaper", [460.0, 560.0], &mut open, |c, ui| {
                if dialogs::draw_wallpaper(c, ui, &mut dlg) {
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
            "Mode hemat memori aktif: hanya gambar yang dilihat disimpan di RAM"
        } else {
            "Mode hemat memori mati: gambar sekitar dimuat lebih dulu agar cepat"
        });
    }

    fn set_two_page(&mut self, ctx: &Context, on: bool) {
        self.two_page_mode = on;
        self.store.state.two_page = on;
        self.store_dirty = true;
        self.relayout(ctx);
        self.notify(if on { "Mode dua halaman aktif" } else { "Mode dua halaman mati" });
    }

    fn set_rtl(&mut self, ctx: &Context, on: bool) {
        self.rtl = on;
        self.store.state.rtl = on;
        self.store_dirty = true;
        self.placed_for = None;
        ctx.request_repaint();
        self.notify(if on {
            "Arah baca kanan-ke-kiri (manga): panah kiri = berikutnya"
        } else {
            "Arah baca kiri-ke-kanan"
        });
    }

    fn set_cover(&mut self, ctx: &Context, on: bool) {
        self.cover_alone = on;
        self.store.state.cover_alone = on;
        self.store_dirty = true;
        self.relayout(ctx);
        self.notify(if on {
            "Halaman pertama tampil sendiri (sampul)"
        } else {
            "Halaman pertama dipasangkan dengan kedua"
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
